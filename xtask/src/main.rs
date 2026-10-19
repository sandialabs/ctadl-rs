//! `cargo xtask` — developer task runner.
//!
//! Three tasks. `regression` ports the former bash harness (`nightly/tests.sh` and friends)
//! for the source-sink taint regression tests, and is what CI runs. `report-eval` sweeps
//! `ctadl report` across a directory of real apps and keeps the per-app JSON; it is a
//! measurement tool rather than a check, and nothing depends on it passing. `cil`
//! regenerates the CIL goldens and opcode table, and runs single CIL checks.
//!
//! Usage:
//!     cargo xtask regression
//!     cargo xtask regression --frontend pcode
//!     cargo xtask regression --filter <name>
//!     cargo xtask regression --tests-dir <dir>
//!     cargo xtask report-eval --apks <dir>
//!     cargo xtask cil bless|codegen

mod android_icc;
mod apk;
mod assertions;
mod baksmali;
mod cil;
mod cil_codegen;
mod dex;
mod discovery;
mod exec;
mod jvm;
mod models;
mod regression;
mod report_eval;
mod sarif;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use crate::discovery::Frontend;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        // Test failures are an expected outcome, not an internal error: we have
        // already printed a per-test report, so just exit non-zero quietly.
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("xtask: error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Returns `Ok(true)` when every selected test passed (or was skipped).
fn run() -> Result<bool> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("regression") => {
            let opts = parse_regression_args(args)?;
            regression::run(&opts)
        }
        Some("report-eval") => {
            let opts = parse_report_eval_args(args)?;
            report_eval::run(&opts)
        }
        Some("cil") => run_cil(args),
        Some("-h" | "--help") | None => {
            print_help();
            Ok(true)
        }
        Some(other) => bail!("unknown subcommand `{other}` (try `xtask --help`)"),
    }
}

fn parse_regression_args(mut args: impl Iterator<Item = String>) -> Result<regression::Options> {
    let mut opts = regression::Options::default();
    // `--frontend` is additive across occurrences, so the first one seen has to
    // clear the default of "every frontend".
    let mut frontends: Option<BTreeSet<Frontend>> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--filter" => {
                opts.filter = Some(args.next().context("--filter requires a value")?);
            }
            "--frontend" => {
                let value = args.next().context("--frontend requires a value")?;
                let names: Vec<&str> = value.split(',').filter(|s| !s.trim().is_empty()).collect();
                if names.is_empty() {
                    bail!("--frontend requires at least one of: dex, jvm, pcode, lua, jni, c, android-icc, cil");
                }
                let selected = frontends.get_or_insert_with(BTreeSet::new);
                for name in names {
                    selected.insert(name.parse::<Frontend>()?);
                }
            }
            "--jobs" | "-j" => {
                let value = args.next().context("--jobs requires a value")?;
                let jobs: usize = value
                    .parse()
                    .with_context(|| format!("--jobs expects a number, got `{value}`"))?;
                if jobs == 0 {
                    bail!("--jobs must be at least 1");
                }
                opts.jobs = Some(jobs);
            }
            "--release" => {
                opts.release = true;
            }
            "--tests-dir" => {
                let dir = args.next().context("--tests-dir requires a value")?;
                opts.tests_dir = Some(PathBuf::from(dir));
            }
            "--jvm-samples" => {
                let dir = args.next().context("--jvm-samples requires a value")?;
                opts.jvm_samples = Some(PathBuf::from(dir));
            }
            "--dex-apk" => {
                let path = args.next().context("--dex-apk requires a value")?;
                opts.dex_apk = Some(PathBuf::from(path));
            }
            "--models-dir" => {
                let dir = args.next().context("--models-dir requires a value")?;
                opts.models_dir = Some(PathBuf::from(dir));
            }
            "--cil-goldens" => {
                let dir = args.next().context("--cil-goldens requires a value")?;
                opts.cil_goldens = Some(PathBuf::from(dir));
            }
            "--cecil-src" => {
                let dir = args.next().context("--cecil-src requires a value")?;
                opts.cecil_src = Some(PathBuf::from(dir));
            }
            "--cil-fixtures" => {
                let dir = args.next().context("--cil-fixtures requires a value")?;
                opts.cil_fixtures = Some(PathBuf::from(dir));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    if let Some(selected) = frontends {
        opts.frontends = selected;
    }
    Ok(opts)
}

/// `cargo xtask cil <command>`: regenerate cil-reader's committed artifacts
/// from the pinned Mono.Cecil. Checking them is `regression --frontend cil`.
fn run_cil(mut args: impl Iterator<Item = String>) -> Result<bool> {
    let command = args.next().context(
        "`cil` requires a command: bless, bless-flow, bless-mir, exec, models or codegen",
    )?;
    let mut cil_dir = None;
    let mut fixtures = None;
    let mut ctadl = None;
    let mut goldens = None;
    let mut cecil_src = None;
    let mut filter = None;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cil-dir" => {
                cil_dir = Some(PathBuf::from(
                    args.next().context("--cil-dir requires a value")?,
                ));
            }
            "--cil-goldens" if matches!(command.as_str(), "bless" | "bless-flow" | "bless-mir") => {
                goldens = Some(PathBuf::from(
                    args.next().context("--cil-goldens requires a value")?,
                ));
            }
            "--cecil-src" => {
                cecil_src = Some(PathBuf::from(
                    args.next().context("--cecil-src requires a value")?,
                ));
            }
            "--cil-fixtures" => {
                fixtures = Some(PathBuf::from(
                    args.next().context("--cil-fixtures requires a value")?,
                ));
            }
            "--ctadl" if command == "taint" => {
                ctadl = Some(PathBuf::from(
                    args.next().context("--ctadl requires a value")?,
                ));
            }
            "--filter" if command == "bless" || command == "exec" => {
                filter = Some(args.next().context("--filter requires a value")?);
            }
            "--out" if command == "exec" || command == "taint" || command == "models" => {
                out = Some(PathBuf::from(
                    args.next().context("--out requires a value")?,
                ));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    if command == "bless-mir" {
        let goldens = cil::resolve_goldens(goldens.as_deref())?
            .context("goldens directory not found; pass --cil-goldens")?;
        let fixtures = cil::resolve_fixtures(fixtures.as_deref())?
            .context("no compiled fixtures: set CIL_READER_FIXTURES (the dev shell does)")?;
        return cil::frontend::bless(&goldens, &fixtures);
    }
    if command == "taint" {
        let fixtures = cil::resolve_fixtures(fixtures.as_deref())?
            .context("no compiled fixtures: set CIL_READER_FIXTURES (the dev shell does)")?;
        let ctadl = ctadl.context("`cil taint` requires --ctadl <path to the ctadl binary>")?;
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-taint"));
        std::fs::create_dir_all(&out)?;
        let outcome = cil::taint::run(&ctadl, &fixtures, &cil::taint::resolve_xtask_dir(), &out)?;
        return Ok(match outcome {
            regression::Outcome::Pass => {
                println!("PASS cil:taint");
                true
            }
            regression::Outcome::Skip(why) => {
                println!("SKIP cil:taint: {why}");
                true
            }
            regression::Outcome::Fail(why)
            | regression::Outcome::HardFail(why)
            | regression::Outcome::Xfail(why) => {
                println!("FAIL cil:taint: {why}");
                false
            }
        });
    }
    if command == "models" {
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-models"));
        std::fs::create_dir_all(&out)?;
        let outcome = cil::models::run(&cil::models::resolve_xtask_dir(), &out)?;
        return Ok(match outcome {
            regression::Outcome::Pass => {
                println!("PASS cil:models");
                true
            }
            regression::Outcome::Skip(why) => {
                println!("SKIP cil:models: {why}");
                true
            }
            regression::Outcome::Fail(why)
            | regression::Outcome::HardFail(why)
            | regression::Outcome::Xfail(why) => {
                println!("FAIL cil:models: {why}");
                false
            }
        });
    }
    if command == "exec" {
        let fixtures = cil::resolve_fixtures(fixtures.as_deref())?
            .context("no compiled fixtures: set CIL_READER_FIXTURES (the dev shell does)")?;
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-exec"));
        return cil::exec::run_standalone(&fixtures, &out, filter.as_deref());
    }
    let cil_dir = cil::resolve_cil_dir(cil_dir.as_deref())?
        .context("cil-reader directory not found; pass --cil-dir")?;
    let cecil = cil::resolve_cecil_src(cecil_src.as_deref(), Some(&cil_dir))?
        .context("no Cecil checkout: pass --cecil-src or set CECIL_SRC (the dev shell does)")?;
    match command.as_str() {
        "bless" => {
            let goldens = cil::resolve_goldens(goldens.as_deref())?
                .context("goldens directory not found; pass --cil-goldens")?;
            cil::bless(&goldens, &cecil, filter.as_deref())
        }
        "bless-flow" => {
            let goldens = cil::resolve_goldens(goldens.as_deref())?
                .context("goldens directory not found; pass --cil-goldens")?;
            let fixtures = cil::resolve_fixtures(fixtures.as_deref())?
                .context("no compiled fixtures: set CIL_READER_FIXTURES (the dev shell does)")?;
            cil::flow::bless(&goldens, &fixtures)
        }
        "codegen" => Ok(cil_codegen::run(&cecil, &cil_dir)),
        other => {
            bail!(
                "unknown `cil` command `{other}` (expected bless, bless-flow, bless-mir, exec, \
                 models or codegen)"
            )
        }
    }
}

fn parse_report_eval_args(mut args: impl Iterator<Item = String>) -> Result<report_eval::Options> {
    let mut opts = report_eval::Options::default();
    let mut apks: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apks" => {
                apks = Some(PathBuf::from(
                    args.next().context("--apks requires a value")?,
                ));
            }
            "--out" => {
                opts.out = PathBuf::from(args.next().context("--out requires a value")?);
            }
            "--filter" => {
                opts.filter = Some(args.next().context("--filter requires a value")?);
            }
            "--top" => {
                let value = args.next().context("--top requires a value")?;
                opts.top = value
                    .parse()
                    .with_context(|| format!("--top expects a number, got `{value}`"))?;
                if opts.top == 0 {
                    bail!("--top must be at least 1");
                }
            }
            "--debug" => opts.release = false,
            "--native-libs" => opts.native_libs = true,
            "--keep-stores" => opts.keep_stores = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    // Required rather than defaulted: the corpora this sweeps are one machine's local files,
    // and a hard-coded path would make a result look reproducible when it is not.
    opts.apks = apks.context("report-eval requires --apks <dir>")?;
    Ok(opts)
}

fn print_help() {
    println!(
        "\
cargo xtask <task>

Tasks:
  regression                 Run the source-sink taint regression suite.
    --frontend <f>           Only exercise frontend <f>: `pcode`, `jvm`, `dex`,
                             `lua`, `jni`, `c`, `android-icc` or `cil` (default:
                             all). Accepts a comma-separated list and may be
                             repeated; unselected
                             frontends are skipped entirely, so their toolchains
                             are not needed. E.g. `--frontend pcode` runs the
                             C/pcode cases and the Ghidra checks without any Java
                             toolchain, and `--frontend lua` runs just the Lua
                             source cases. `jni` is the two-import bridge cases,
                             which need the Java *and* Ghidra toolchains.
                             `android-icc` runs external DroidBench/ICC-Bench
                             APK specs under `tests/android-icc`. `cil` runs the
                             cil-reader checks against Mono.Cecil (see
                             --cecil-src).
    --filter <name>          Only run cases whose name contains <name>.
                             Composes with --frontend.
    -j, --jobs <n>           Run <n> cases concurrently (default: one per core,
                             capped). Cases are independent and each runs in its
                             own scratch dir, so this only trades memory for wall
                             clock; `-j 1` reverts to running them one at a time.
    --release                Build and exercise the release `ctadl` binary
                             (default: debug). Either way the binary is rebuilt
                             from current source before any case runs, so the
                             suite never runs against a stale on-disk binary.
    --tests-dir <dir>        Look for test cases under <dir> (default: auto-detect
                             `nightly/tests` or `tests` relative to the cwd).
    --jvm-samples <dir>      Directory of jvm-reader sample .java sources to
                             compile and check (default: auto-detect
                             `readers/jvm-reader/tests/sample`). Also drive the
                             dex-reader checks (compiled down to .dex).
    --dex-apk <path>         Real-world APK for the dex-reader smoke test and
                             the `apk:*` end-to-end checks (default: auto-detect
                             `xtask/tests/dex/com.noto_54.apk`). The `apk:*`
                             checks drive `ctadl` itself and need no toolchain,
                             so they run whenever `dex` is selected and this
                             resolves; both self-skip when it does not.
    --models-dir <dir>       Directory holding the model generator schema and
                             the built-in model files checked against it
                             (default: auto-detect `ctadl-ascent/src/models`).
                             The `models:*` checks self-skip when neither the
                             flag nor the default directory is present.
    --cil-goldens <dir>      The committed Cecil goldens that `cil:goldens`
                             compares against (default: auto-detect
                             `xtask/cil-goldens`).
    --cecil-src <dir>        Mono.Cecil checkout whose test assemblies the `cil:*`
                             checks read (default: a hand-cloned
                             `readers/cil-reader/extern/cecil`, else $CECIL_SRC,
                             which the dev shell sets). The checks self-skip
                             without one.
                             `cil:cecil-compare` also needs `cecil-dump` on PATH
                             (or $CECIL_DUMP); the dev shell provides it.
    --cil-fixtures <dir>     cil-reader's compiled fixtures (default:
                             $CIL_READER_FIXTURES, which the dev shell sets to
                             the flake's `cil-fixtures`). The `cil:*` checks
                             self-skip without them.

  cil <command>              Regenerate cil-reader's committed artifacts from the
                             pinned Mono.Cecil (checking them is
                             `regression --frontend cil`). Takes --cecil-src as
                             `regression` does, and --cil-dir <dir>, the
                             cil-reader directory (default: auto-detect
                             `readers/cil-reader`).
    bless [--filter <name>] [--cil-goldens <dir>]
                             Rewrite xtask/cil-goldens (or <dir>) from
                             `cecil-dump` (on PATH in the dev shell). Needed after
                             moving the flake's `cecil` pin.
    bless-flow [--cil-goldens <dir>]
                             Rewrite xtask/cil-goldens/flow (or <dir>/flow), the
                             `cil-dump --flow` goldens of the flow fixtures,
                             from $CIL_READER_FIXTURES. Review the diff.
    bless-mir [--cil-goldens <dir>] [--cil-fixtures <dir>]
                             Rewrite xtask/cil-goldens/mir (or <dir>/mir), the
                             IR the CIL frontend makes of the frontend fixtures,
                             from $CIL_READER_FIXTURES. Review the diff.
    taint --ctadl <path> [--out <dir>]
                             The `cil:taint` check on its own, against that
                             `ctadl` binary, with its outputs in
                             <dir>/cil-taint (default: target/xtask-cil-taint).
    exec [--filter <name>] [--out <dir>]
                             The `cil:exec` check on its own: run the exec
                             fixtures' methods (those whose name contains
                             <name>) on .NET with `flow-run` and in the
                             interpreter over the flow layer, print per-method
                             counts and every failure, and keep the outputs in
                             <dir>/cil-exec (default: target/xtask-cil-exec).
    models [--out <dir>]     The `cil:models` check on its own: the default .NET
                             models against what .NET does with the methods
                             they select (`model-run`, built with `dotnet` when
                             not on PATH), with its report in
                             <dir>/cil-models (default: target/xtask-cil-models).
    codegen                 Regenerate cil-reader's opcode table from Cecil's
                             OpCodes.cs.

  report-eval --apks <dir>   Run `ctadl report` over a directory of artifacts and
                             print a cross-app summary, keeping each app's JSON.
    --apks <dir>             Required. Directory of artifacts to sweep. There is
                             deliberately no default: the corpora this is for are
                             one machine's local files, and a baked-in path would
                             make a result look reproducible when it is not.
    --out <dir>              Where the per-app JSON, text reports, logs and
                             scratch stores go (default: target/report-eval).
    --filter <name>          Only sweep artifacts whose filename contains <name>.
    --top <n>                Rows in each report's worst-signature and fan-in
                             lists (default: 10).
    --debug                  Exercise the debug binary (default: release).
    --native-libs            Also import an APK's native libraries. Off by
                             default: they go through Ghidra, which is a much
                             larger measurement than the Dex call graph.
    --keep-stores            Keep each app's scratch store. A 200 MB app imports
                             to a couple of gigabytes, so a corpus adds up.
"
    );
}

#[cfg(test)]
mod tests {
    use super::{parse_regression_args, Frontend};
    use std::collections::BTreeSet;

    fn frontends(args: &[&str]) -> BTreeSet<Frontend> {
        parse_regression_args(args.iter().map(|s| s.to_string()))
            .unwrap()
            .frontends
    }

    #[test]
    fn no_frontend_flag_selects_everything() {
        assert_eq!(frontends(&[]), Frontend::ALL.iter().copied().collect());
        // An unrelated flag must not narrow the selection.
        assert_eq!(
            frontends(&["--filter", "ArrayFlow"]),
            Frontend::ALL.iter().copied().collect()
        );
    }

    #[test]
    fn frontend_flag_narrows_selection() {
        assert_eq!(
            frontends(&["--frontend", "pcode"]),
            [Frontend::Pcode].into()
        );
    }

    #[test]
    fn frontend_flag_is_additive() {
        let expected: BTreeSet<Frontend> = [Frontend::Jvm, Frontend::Pcode].into();
        // Comma-separated and repeated forms mean the same thing.
        assert_eq!(frontends(&["--frontend", "pcode,jvm"]), expected);
        assert_eq!(
            frontends(&["--frontend", "pcode", "--frontend", "jvm"]),
            expected
        );
    }

    #[test]
    fn jobs_flag_parses() {
        let jobs = |args: &[&str]| {
            parse_regression_args(args.iter().map(|s| s.to_string()))
                .unwrap()
                .jobs
        };
        // Absent means "let the harness pick", not "one".
        assert_eq!(jobs(&[]), None);
        assert_eq!(jobs(&["--jobs", "4"]), Some(4));
        assert_eq!(jobs(&["-j", "4"]), Some(4));
    }

    #[test]
    fn bad_jobs_is_an_error() {
        let err =
            |args: &[&str]| parse_regression_args(args.iter().map(|s| s.to_string())).is_err();
        assert!(err(&["--jobs"]));
        assert!(err(&["--jobs", "nope"]));
        // Zero workers would silently run nothing; say so instead.
        assert!(err(&["--jobs", "0"]));
    }

    #[test]
    fn bad_frontend_is_an_error() {
        let err =
            |args: &[&str]| parse_regression_args(args.iter().map(|s| s.to_string())).is_err();
        assert!(err(&["--frontend", "bogus"]));
        assert!(err(&["--frontend"]));
        // An empty selection would silently run nothing; say so instead.
        assert!(err(&["--frontend", ""]));
        assert!(err(&["--frontend", ","]));
    }
}
