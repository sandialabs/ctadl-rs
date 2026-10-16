//! cil-reader regression checks: Cecil parity over Mono.Cecil's own test
//! corpus plus cil-reader's fixtures (Cecil's .il/.cs test sources, compiled
//! by the flake's `cilFixtures`).
//!
//! The dump format is the contract: `cil-dump` renders an assembly with
//! cil-reader, `tools/CecilDump` renders it with Mono.Cecil, and the two must
//! be byte-identical. Six checks of that, and two of the flow layer:
//!
//! - `cil:must-fail`     — non-CLI inputs (`text_file.txt`) are rejected.
//! - `cil:cecil-tests`   — cil-reader's ports of Cecil's own test suites, which
//!   read the same inputs; see [`cecil_tests`].
//! - `cil:read`          — every corpus assembly reads and dumps in full
//!   without an error or a panic.
//! - `cil:goldens`       — dumps of Cecil's committed assemblies match the
//!   committed Cecil goldens (`xtask/cil-goldens`). Needs no .NET.
//! - `cil:cecil-compare` — dumps match a live run of CecilDump, head to head,
//!   which also covers what has no golden: the fixtures (build outputs) and
//!   the inputs too large to keep goldens for. Needs the
//!   `cecil-dump` launcher (the flake's `cecilDump`, on PATH in the devshell
//!   and the regression check); self-skips without it.
//! - `cil:symbols`       — the sequence points of each input's portable PDB
//!   (`cil-dump --symbols`) match the committed goldens
//!   (`xtask/cil-goldens/symbols`; an input with no golden has no PDB) and a
//!   live run of `CecilDump --symbols`, as `cil:goldens` and
//!   `cil:cecil-compare` check the main dump.
//! - `cil:flow`          — cil-reader's flow layer: its invariants over the
//!   whole corpus, targeted tests on the `flow.il` fixture, and the
//!   `cil-dump --flow` goldens (`xtask/cil-goldens/flow`); see [`flow`].
//! - `cil:frontend`      — the CIL frontend (`ctadl-cil`): every corpus assembly
//!   imported and put through the index's SSA pipeline, and the IR goldens
//!   (`xtask/cil-goldens/mir`); see [`frontend`].
//! - `cil:taint`         — the index's summaries of the exec fixtures' methods
//!   against the flows the interpreter's runs show; see [`taint`]. Self-skips
//!   without `flow-run`.
//! - `cil:exec`          — the flow layer run: the exec fixtures' methods on
//!   .NET (`flow-run`) and in an interpreter over `MethodFlow`, with the
//!   same random inputs, must agree; see [`exec`]. Self-skips without
//!   `flow-run`.
//!
//! The corpus, golden naming and comparison rules live in [`parity`].
//! `cargo xtask cil bless` regenerates the goldens.
//!
//! Neither input is committed. Cecil comes from `--cecil-src`, else a
//! checkout cloned by hand into `readers/cil-reader/extern/cecil`, else
//! `$CECIL_SRC`; the fixtures from `--cil-fixtures`, else
//! `$CIL_READER_FIXTURES`. The dev shell and the regression check set both
//! variables; without both inputs the checks skip.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::regression::Outcome;
use parity::{CecilDump, Input};

pub mod cases;
mod cecil_tests;
pub mod exec;
pub mod flow;
pub mod frontend;
mod parity;
pub mod taint;

/// Failing inputs listed per check before the rest are summarized as a count.
const MAX_LISTED: usize = 10;

/// Run every cil-reader check. `goldens` is the committed goldens directory;
/// `cecil` the Cecil checkout holding the corpus; `fixtures` the compiled
/// fixtures.
pub fn run_checks(
    goldens: Option<&Path>,
    cecil: Option<&Path>,
    fixtures: Option<&Path>,
    work: &Path,
) -> Result<Vec<(String, Outcome)>> {
    let Some(goldens) = goldens else {
        return Ok(skip_all("goldens directory not found; pass --cil-goldens"));
    };
    let Some(cecil) = cecil else {
        return Ok(skip_all(
            "no Cecil checkout: pass --cecil-src or set CECIL_SRC (the devshell does)",
        ));
    };
    let Some(fixtures) = fixtures else {
        return Ok(skip_all(&format!(
            "no compiled fixtures: pass --cil-fixtures or set {} (the devshell does; \
             `nix build .#cil-fixtures` builds them)",
            parity::FIXTURES_ENV
        )));
    };

    let inputs = parity::corpus(cecil, Some(fixtures));
    if !inputs.iter().any(|i| i.tag == parity::FIXTURES_TAG) {
        anyhow::bail!("no assemblies under {}", fixtures.display());
    }
    // Rendered once and shared by `read`, `goldens` and `cecil-compare`.
    let dumps = parallel_map(&inputs, |input| parity::rust_dump(&input.path));

    Ok(vec![
        ("cil:must-fail".to_string(), check_must_fail(cecil)),
        (
            "cil:cecil-tests".to_string(),
            cecil_tests::run(cecil, fixtures),
        ),
        ("cil:read".to_string(), check_read(&inputs, &dumps)),
        (
            "cil:goldens".to_string(),
            check_goldens(goldens, &inputs, &dumps)?,
        ),
        (
            "cil:cecil-compare".to_string(),
            check_cecil_compare(&inputs, &dumps, work)?,
        ),
        (
            "cil:symbols".to_string(),
            check_symbols(goldens, &inputs, work)?,
        ),
        (
            "cil:flow".to_string(),
            flow::run(&inputs, fixtures, goldens, work)?,
        ),
        (
            "cil:frontend".to_string(),
            frontend::run(&inputs, fixtures, goldens, work)?,
        ),
        ("cil:exec".to_string(), exec::run(fixtures, work)?),
        ("cil:taint".to_string(), {
            let ctadl = crate::regression::ctadl_bin()?;
            taint::run(&ctadl, fixtures, &taint::resolve_xtask_dir(), work)?
        }),
    ])
}

fn skip_all(why: &str) -> Vec<(String, Outcome)> {
    vec![("cil".to_string(), Outcome::Skip(why.to_string()))]
}

fn check_must_fail(cecil: &Path) -> Outcome {
    let mut failures = Vec::new();
    for path in parity::must_fail_inputs(cecil) {
        match std::fs::read(&path) {
            Ok(data) if cil_reader::Module::read(&data).is_ok() => {
                failures.push(format!(
                    "{}: read succeeded, expected a BadImage error",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    to_outcome(failures, "input(s)")
}

fn check_read(inputs: &[Input], dumps: &[Result<String, String>]) -> Outcome {
    let failures = inputs
        .iter()
        .zip(dumps)
        .filter_map(|(input, dump)| {
            dump.as_ref()
                .err()
                .map(|e| format!("{}: {e}", input.path.display()))
        })
        .collect();
    to_outcome(failures, "input(s)")
}

fn check_goldens(
    goldens: &Path,
    inputs: &[Input],
    dumps: &[Result<String, String>],
) -> Result<Outcome> {
    let mut failures = Vec::new();
    let mut compared = 0usize;
    for (input, dump) in inputs.iter().zip(dumps) {
        let Some(golden_name) = input.golden_name() else {
            continue;
        };
        let golden_path = goldens.join(&golden_name);
        if !golden_path.exists() {
            continue;
        }
        // An unreadable input already fails `cil:read`; do not count it twice.
        let Ok(dump) = dump else { continue };
        compared += 1;
        let golden = std::fs::read_to_string(&golden_path)
            .with_context(|| format!("failed to read {}", golden_path.display()))?;
        if golden != *dump {
            failures.push(describe_diff(&golden_name, "golden", &golden, dump));
        }
    }
    if compared == 0 {
        return Ok(Outcome::Fail(format!(
            "no goldens matched any corpus input under {}",
            goldens.display()
        )));
    }
    Ok(to_outcome(failures, "input(s)"))
}

fn check_cecil_compare(
    inputs: &[Input],
    dumps: &[Result<String, String>],
    work: &Path,
) -> Result<Outcome> {
    let Some(cecil_dump) = CecilDump::locate() else {
        return Ok(Outcome::Skip(format!(
            "`cecil-dump` not on PATH and ${} unset (the devshell provides it)",
            parity::CECIL_DUMP_ENV
        )));
    };
    let compared: Vec<(&Input, &String)> = inputs
        .iter()
        .zip(dumps)
        .filter(|(input, _)| !parity::compare_skip(&input.name()))
        .filter_map(|(input, dump)| dump.as_ref().ok().map(|d| (input, d)))
        .collect();
    let cecil_dumps = parallel_map(&compared, |(input, _)| cecil_dump.dump(&input.path));

    // Both sides of every mismatch are kept, since a diff of a multi-megabyte
    // dump is not something to reconstruct from the summary.
    let diffs = work.join("cecil-compare");
    let mut failures = Vec::new();
    for ((input, rust), cecil) in compared.iter().zip(&cecil_dumps) {
        let name = input.id();
        match cecil {
            Err(e) => failures.push(format!("{name}: {}", e.trim_end())),
            Ok(cecil) if cecil == *rust => {}
            Ok(cecil) => {
                std::fs::create_dir_all(&diffs)?;
                std::fs::write(diffs.join(format!("{name}.cecil")), cecil)?;
                std::fs::write(diffs.join(format!("{name}.rust")), rust)?;
                failures.push(describe_diff(&name, "cecil", cecil, rust));
            }
        }
    }
    let mut outcome = to_outcome(failures, "input(s)");
    if let Outcome::Fail(why) = &mut outcome {
        let _ = write!(
            why,
            "\nboth dumps of each mismatch are in {} (CecilDump: {})",
            diffs.display(),
            cecil_dump.describe()
        );
    }
    Ok(outcome)
}

/// Where the symbols goldens are.
fn symbols_goldens(goldens: &Path) -> PathBuf {
    goldens.join("symbols")
}

fn check_symbols(goldens: &Path, inputs: &[Input], work: &Path) -> Result<Outcome> {
    let dumps = parallel_map(inputs, |input| parity::rust_symbols(&input.path));
    let golden_dir = symbols_goldens(goldens);
    let mut failures = Vec::new();
    let mut with_symbols = 0usize;
    for (input, dump) in inputs.iter().zip(&dumps) {
        let dump = match dump {
            Ok(dump) => dump,
            Err(e) => {
                failures.push(format!("{}: {e}", input.id()));
                continue;
            }
        };
        let Some(golden_name) = input.golden_name() else {
            continue;
        };
        let golden_path = golden_dir.join(&golden_name);
        let golden = if golden_path.exists() {
            with_symbols += 1;
            std::fs::read_to_string(&golden_path)
                .with_context(|| format!("failed to read {}", golden_path.display()))?
        } else {
            parity::NO_SYMBOLS.to_string()
        };
        if golden != *dump {
            failures.push(describe_diff(&golden_name, "golden", &golden, dump));
        }
    }
    if with_symbols == 0 {
        return Ok(Outcome::Fail(format!(
            "no symbols goldens under {}",
            golden_dir.display()
        )));
    }

    // Live, when there is a CecilDump that has `--symbols`: one built before
    // it had the flag dumps the whole image instead, and is left out.
    let live = CecilDump::locate().filter(|c| {
        inputs
            .first()
            .and_then(|i| c.symbols(&i.path).ok())
            .is_some_and(|d| d.starts_with("[symbols]"))
    });
    if let Some(cecil_dump) = &live {
        let compared: Vec<(&Input, &String)> = inputs
            .iter()
            .zip(&dumps)
            .filter(|(input, _)| !parity::compare_skip(&input.name()))
            .filter_map(|(input, dump)| dump.as_ref().ok().map(|d| (input, d)))
            .collect();
        let cecil_dumps = parallel_map(&compared, |(input, _)| cecil_dump.symbols(&input.path));
        let diffs = work.join("symbols-compare");
        for ((input, rust), cecil) in compared.iter().zip(&cecil_dumps) {
            let name = input.id();
            match cecil {
                Err(e) => failures.push(format!("{name}: {}", e.trim_end())),
                Ok(cecil) if cecil == *rust => {}
                Ok(cecil) => {
                    std::fs::create_dir_all(&diffs)?;
                    std::fs::write(diffs.join(format!("{name}.cecil")), cecil)?;
                    std::fs::write(diffs.join(format!("{name}.rust")), rust)?;
                    failures.push(describe_diff(&name, "cecil", cecil, rust));
                }
            }
        }
    }
    let mut outcome = to_outcome(failures, "input(s)");
    if let Outcome::Fail(why) = &mut outcome {
        let _ = write!(
            why,
            "\nboth dumps of each live mismatch are in {}",
            work.join("symbols-compare").display()
        );
    }
    Ok(outcome)
}

fn describe_diff(name: &str, expected_label: &str, expected: &str, actual: &str) -> String {
    let (line, lines) = parity::first_divergence(expected, actual);
    let mut msg = format!("{name}: differs at line {line}");
    match lines {
        Some((le, la)) => {
            let _ = write!(msg, "\n  {expected_label}: {le}\n  rust: {la}");
        }
        None => {
            let _ = write!(
                msg,
                " (line counts: {expected_label} {}, rust {})",
                expected.lines().count(),
                actual.lines().count()
            );
        }
    }
    msg
}

fn to_outcome(failures: Vec<String>, what: &str) -> Outcome {
    if failures.is_empty() {
        return Outcome::Pass;
    }
    let mut msg = format!("{} {what} failed:", failures.len());
    for failure in failures.iter().take(MAX_LISTED) {
        let _ = write!(msg, "\n{failure}");
    }
    if failures.len() > MAX_LISTED {
        let _ = write!(msg, "\n... and {} more", failures.len() - MAX_LISTED);
    }
    Outcome::Fail(msg)
}

/// `items.map(f)` across the available cores, in order. CecilDump starts a
/// .NET runtime per input, so a serial pass over ~120 inputs is most of this
/// task's wall clock.
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let jobs = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else { break };
                let result = f(item);
                results.lock().unwrap().push((index, result));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, r)| r).collect()
}

/// Locate the cil-reader directory. With no override, look where it lives
/// relative to the repo root (`cargo xtask`) or the nightly cwd.
pub fn resolve_cil_dir(override_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    resolve_dir(
        override_dir,
        ["readers/cil-reader", "../readers/cil-reader"],
        |p| p.join("crates/cil-reader/Cargo.toml").is_file(),
    )
    .with_context(|| "failed to canonicalize cil-reader directory")
}

/// Locate the committed goldens: `--cil-goldens`, else `xtask/cil-goldens`
/// relative to the repo root (`cargo xtask`) or the nightly cwd.
pub fn resolve_goldens(override_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    resolve_dir(
        override_dir,
        ["xtask/cil-goldens", "../xtask/cil-goldens"],
        |p| p.is_dir(),
    )
    .with_context(|| "failed to canonicalize goldens directory")
}

/// `override_dir` if given, else the first of `candidates` that is `wanted`;
/// canonicalized either way.
fn resolve_dir(
    override_dir: Option<&Path>,
    candidates: [&str; 2],
    wanted: impl Fn(&Path) -> bool,
) -> Result<Option<PathBuf>> {
    if let Some(dir) = override_dir {
        return Ok(Some(std::fs::canonicalize(dir).with_context(|| {
            format!("failed to canonicalize {}", dir.display())
        })?));
    }
    candidates
        .into_iter()
        .map(PathBuf::from)
        .find(|p| wanted(p))
        .map(|p| std::fs::canonicalize(&p))
        .transpose()
        .map_err(Into::into)
}

/// Locate the compiled fixtures: `--cil-fixtures`, else `$CIL_READER_FIXTURES`.
pub fn resolve_fixtures(override_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = override_dir {
        return Ok(Some(std::fs::canonicalize(dir).with_context(|| {
            format!("failed to canonicalize {}", dir.display())
        })?));
    }
    Ok(std::env::var_os(parity::FIXTURES_ENV)
        .map(PathBuf::from)
        .filter(|d| d.is_dir()))
}

/// Locate the Cecil checkout: `--cecil-src`, else `extern/cecil` under the
/// cil-reader directory, else `$CECIL_SRC`. A checkout cloned there by hand is
/// a deliberate override of the pin, as it is for cil-reader's tests.
pub fn resolve_cecil_src(
    override_dir: Option<&Path>,
    cil_dir: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(dir) = override_dir {
        return Ok(Some(std::fs::canonicalize(dir).with_context(|| {
            format!("failed to canonicalize {}", dir.display())
        })?));
    }
    let vendored = cil_dir.map(|d| d.join("extern/cecil"));
    let pinned = std::env::var_os("CECIL_SRC").map(PathBuf::from);
    Ok(vendored.into_iter().chain(pinned).find(|d| d.is_dir()))
}

/// `cargo xtask cil bless [filter]`: regenerate the committed goldens
/// (`xtask/cil-goldens`, and its `symbols/`) from CecilDump. Needed after moving the flake's `cecil`
/// pin or changing the dump format on both sides; review the diff before
/// committing it. Only Cecil's own assemblies get goldens; see
/// `Input::golden_name`.
pub fn bless(goldens: &Path, cecil: &Path, filter: Option<&str>) -> Result<bool> {
    let cecil_dump = CecilDump::locate().with_context(|| {
        format!(
            "`cecil-dump` not on PATH and ${} unset; enter the dev shell or \
             `nix build .#cecil-dump`",
            parity::CECIL_DUMP_ENV
        )
    })?;
    std::fs::create_dir_all(goldens)?;
    let symbols_dir = symbols_goldens(goldens);
    std::fs::create_dir_all(&symbols_dir)?;
    let mut written = 0usize;
    let mut errors = 0usize;
    for input in parity::corpus(cecil, None) {
        let name = input.name();
        let Some(golden_name) = input.golden_name() else {
            continue;
        };
        if parity::compare_skip(&name) {
            continue;
        }
        if filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        match cecil_dump.dump(&input.path) {
            Ok(dump) => {
                std::fs::write(goldens.join(&golden_name), dump)?;
                written += 1;
            }
            Err(e) => {
                eprintln!("ERROR {name}: {}", e.trim_end());
                errors += 1;
            }
        }
        // An input without symbols has no golden.
        let symbols_path = symbols_dir.join(&golden_name);
        match cecil_dump.symbols(&input.path) {
            Ok(dump) if !dump.starts_with("[symbols]") => {
                anyhow::bail!(
                    "{} has no --symbols; rebuild it (`nix build .#cecil-dump`)",
                    cecil_dump.describe()
                );
            }
            Ok(dump) if dump == parity::NO_SYMBOLS => {
                if symbols_path.exists() {
                    std::fs::remove_file(&symbols_path)?;
                }
            }
            Ok(dump) => {
                std::fs::write(&symbols_path, dump)?;
                written += 1;
            }
            Err(e) => {
                eprintln!("ERROR {name} --symbols: {}", e.trim_end());
                errors += 1;
            }
        }
    }
    println!(
        "bless: wrote {written} golden(s) to {}, {errors} error(s)",
        goldens.display()
    );
    Ok(errors == 0)
}
