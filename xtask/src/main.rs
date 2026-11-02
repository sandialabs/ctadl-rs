//! `cargo xtask` — developer task runner.
//!
//! Four tasks. `regression` runs the source-sink taint regression tests, and is what CI
//! runs. `report-eval` sweeps `ctadl report` across a directory of real apps and keeps the
//! per-app JSON; it is a measurement tool rather than a check, and nothing depends on it
//! passing. `cil` regenerates the CIL goldens and opcode table, and runs single CIL checks.
//! `dex-opcodes` checks dex-reader's opcode semantics against ART on a device.
//!
//! Usage:
//!     cargo xtask regression
//!     cargo xtask regression --frontend pcode
//!     cargo xtask regression --filter <name>
//!     cargo xtask regression --tests-dir <dir>
//!     cargo xtask regression --frontend pcode,jni --mem2reg full:0
//!     cargo xtask report-eval --apks <dir>
//!     cargo xtask dex-opcodes [--avd <name>] [--save]
//!     cargo xtask cil bless|bless-flow|bless-mir|codegen|exec|models|taint

mod android_icc;
mod apk;
mod asm_flow;
mod assertions;
mod baksmali;
mod cil;
mod cil_codegen;
mod dex;
mod dex_opcodes;
mod dexlib2;
mod discovery;
mod exec;
mod jvm;
mod jvm_opcodes;
mod regression;
mod report_eval;
mod sarif;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

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
        Some("dex-opcodes") => {
            let opts = parse_dex_opcodes_args(args)?;
            dex_opcodes::run(&opts)
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

fn parse_jobs(value: &str) -> Result<usize> {
    let jobs: usize = value
        .parse()
        .with_context(|| format!("--jobs expects a number, got `{value}`"))?;
    if jobs == 0 {
        bail!("--jobs must be at least 1");
    }
    Ok(jobs)
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
                    bail!(
                        "--frontend requires at least one of: dex, jvm, pcode, lua, jni, c, android-icc, cil"
                    );
                }
                let selected = frontends.get_or_insert_with(BTreeSet::new);
                for name in names {
                    selected.insert(name.parse::<Frontend>()?);
                }
            }
            "--jobs" | "-j" => {
                let value = args.next().context("--jobs requires a value")?;
                opts.jobs = Some(parse_jobs(&value)?);
            }
            // The value can also be attached: `-j4`, `--jobs=4`.
            other if other.starts_with("--jobs=") || other.starts_with("-j") => {
                let value = other
                    .strip_prefix("--jobs=")
                    .or_else(|| other.strip_prefix("-j"));
                opts.jobs = Some(parse_jobs(value.unwrap_or_default())?);
            }
            "--release" => {
                opts.release = true;
            }
            "--mem2reg" => {
                opts.mem2reg = Some(args.next().context("--mem2reg requires a value")?);
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

fn parse_dex_opcodes_args(mut args: impl Iterator<Item = String>) -> Result<dex_opcodes::Options> {
    let mut opts = dex_opcodes::Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => opts.out = PathBuf::from(args.next().context("--out requires a value")?),
            "--avd" => opts.avd = Some(args.next().context("--avd requires a value")?),
            "--filter" => opts.filter = Some(args.next().context("--filter requires a value")?),
            "--save" => opts.save = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    Ok(opts)
}

/// `cargo xtask cil <command>`: regenerate cil-reader's committed artifacts
/// (`bless`, `bless-flow`, `bless-mir`, `codegen`), or run one CIL check on its own
/// (`exec`, `models`, `taint`). The full check suite is `regression --frontend cil`.
fn run_cil(mut args: impl Iterator<Item = String>) -> Result<bool> {
    let command = args.next().context(
        "`cil` requires a command: bless, bless-flow, bless-mir, exec, models, taint or codegen",
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
    let need_fixtures = || {
        cil::resolve_fixtures(fixtures.as_deref())?
            .context("no compiled fixtures: set CIL_READER_FIXTURES (the dev shell does)")
    };
    let need_goldens = || {
        cil::resolve_goldens(goldens.as_deref())?
            .context("goldens directory not found; pass --cil-goldens")
    };
    if command == "bless-mir" {
        let goldens = need_goldens()?;
        let fixtures = need_fixtures()?;
        return cil::frontend::bless(&goldens, &fixtures);
    }
    if command == "taint" {
        let fixtures = need_fixtures()?;
        let ctadl = ctadl.context("`cil taint` requires --ctadl <path to the ctadl binary>")?;
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-taint"));
        std::fs::create_dir_all(&out)?;
        let outcome = cil::taint::run(&ctadl, &fixtures, &cil::taint::resolve_xtask_dir(), &out)?;
        return Ok(report_check("cil:taint", outcome));
    }
    if command == "models" {
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-models"));
        std::fs::create_dir_all(&out)?;
        let outcome = cil::models::run(&cil::models::resolve_xtask_dir(), &out)?;
        return Ok(report_check("cil:models", outcome));
    }
    if command == "exec" {
        let fixtures = need_fixtures()?;
        let out = out.unwrap_or_else(|| PathBuf::from("target/xtask-cil-exec"));
        return cil::exec::run_standalone(&fixtures, &out, filter.as_deref());
    }
    let cil_dir = cil::resolve_cil_dir(cil_dir.as_deref())?
        .context("cil-reader directory not found; pass --cil-dir")?;
    let cecil = cil::resolve_cecil_src(cecil_src.as_deref(), Some(&cil_dir))?
        .context("no Cecil checkout: pass --cecil-src or set CECIL_SRC (the dev shell does)")?;
    match command.as_str() {
        "bless" => {
            let goldens = need_goldens()?;
            cil::bless(&goldens, &cecil, filter.as_deref())
        }
        "bless-flow" => {
            let goldens = need_goldens()?;
            let fixtures = need_fixtures()?;
            cil::flow::bless(&goldens, &fixtures)
        }
        "codegen" => Ok(cil_codegen::run(&cecil, &cil_dir)),
        other => {
            bail!(
                "unknown `cil` command `{other}` (expected bless, bless-flow, bless-mir, exec, \
                 models, taint or codegen)"
            )
        }
    }
}

/// Prints a one-line verdict for the check `name`; returns whether it passed or skipped.
fn report_check(name: &str, outcome: regression::Outcome) -> bool {
    match outcome {
        regression::Outcome::Pass => {
            println!("PASS {name}");
            true
        }
        regression::Outcome::Skip(why) => {
            println!("SKIP {name}: {why}");
            true
        }
        regression::Outcome::Fail(why)
        | regression::Outcome::HardFail(why)
        | regression::Outcome::Xfail(why) => {
            println!("FAIL {name}: {why}");
            false
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
    --mem2reg <policy>       Index every case with `ctadl index --mem2reg <policy>`
                             (default: ctadl's own default). For a policy that
                             can lose flows, such as `full:0`: a case that loses
                             its flow is an XFAIL when an index's
                             notifications.sarif names a function it promoted
                             fully, and a FAIL when none does.
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

  dex-opcodes                Run every Dalvik opcode on ART (an attached device or
                             emulator) and check dex-reader's data_flow() and
                             can_throw() against what it reads, writes and throws.
                             Opt-in and slow; see xtask/tests/dex/README.md.
    --out <dir>              Where the generated cases, device log, results and
                             report go (default: target/dex-opcodes).
    --avd <name>             Boot this AVD headless if no device is attached,
                             and shut it down afterwards.
    --filter <name>          Only run cases whose name contains <name>.
    --save                   If every case passes, save ART's observations to
                             xtask/tests/dex/opcodes-art.txt, which the
                             `dex:opcodes-replay` regression check judges
                             dex-reader against without a device.

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
    use super::{Frontend, parse_regression_args};
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
        assert_eq!(jobs(&["-j4"]), Some(4));
        assert_eq!(jobs(&["--jobs=4"]), Some(4));
    }

    #[test]
    fn bad_jobs_is_an_error() {
        let err =
            |args: &[&str]| parse_regression_args(args.iter().map(|s| s.to_string())).is_err();
        assert!(err(&["--jobs"]));
        assert!(err(&["--jobs", "nope"]));
        // Zero workers would silently run nothing; say so instead.
        assert!(err(&["--jobs", "0"]));
        assert!(err(&["-j0"]));
        assert!(err(&["--jobs="]));
        assert!(err(&["-jx"]));
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
