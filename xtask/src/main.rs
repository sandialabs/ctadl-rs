//! `cargo xtask` — developer task runner.
//!
//! Today the only task is `regression`, which ports the former bash harness
//! (`nightly/tests.sh` and friends) for the source-sink taint regression tests.
//!
//! Usage:
//!     cargo xtask regression
//!     cargo xtask regression --frontend pcode
//!     cargo xtask regression --filter <name>
//!     cargo xtask regression --tests-dir <dir>

mod apk;
mod assertions;
mod baksmali;
mod dex;
mod discovery;
mod exec;
mod jvm;
mod models;
mod regression;
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
                    bail!("--frontend requires at least one of: dex, jvm, pcode, lua, jni, c");
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

fn print_help() {
    println!(
        "\
cargo xtask <task>

Tasks:
  regression                 Run the source-sink taint regression suite.
    --frontend <f>           Only exercise frontend <f>: `pcode`, `jvm`, `dex`,
                             `lua`, `jni` or `c` (default: all). Accepts a
                             comma-separated list and may be repeated; unselected
                             frontends are skipped entirely, so their toolchains
                             are not needed. E.g. `--frontend pcode` runs the
                             C/pcode cases and the Ghidra checks without any Java
                             toolchain, and `--frontend lua` runs just the Lua
                             source cases. `jni` is the two-import bridge cases,
                             which need the Java *and* Ghidra toolchains.
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
                             `jvm-reader/tests/sample`). Also drive the
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
