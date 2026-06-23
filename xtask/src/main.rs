//! `cargo xtask` — developer task runner.
//!
//! Today the only task is `regression`, which ports the former bash harness
//! (`nightly/tests.sh` and friends) for the source-sink taint regression tests.
//!
//! Usage:
//!     cargo xtask regression
//!     cargo xtask regression --filter <name>
//!     cargo xtask regression --tests-dir <dir>

mod assertions;
mod baksmali;
mod dex;
mod discovery;
mod exec;
mod jvm;
mod regression;
mod taintbench;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

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
        Some("taintbench") => {
            let opts = parse_taintbench_args(args)?;
            taintbench::run(&opts)
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
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--filter" => {
                opts.filter = Some(args.next().context("--filter requires a value")?);
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
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    Ok(opts)
}

fn parse_taintbench_args(mut args: impl Iterator<Item = String>) -> Result<taintbench::Options> {
    let mut opts = taintbench::Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apps-dir" => {
                let dir = args.next().context("--apps-dir requires a value")?;
                opts.apps_dir = Some(PathBuf::from(dir));
            }
            "--apk" => {
                let spec = args.next().context("--apk requires a value (<name>=<path>)")?;
                let (name, path) = spec
                    .split_once('=')
                    .with_context(|| format!("--apk expects <name>=<path>, got `{spec}`"))?;
                opts.apks.push((name.to_string(), PathBuf::from(path)));
            }
            "--filter" => {
                opts.filter = Some(args.next().context("--filter requires a value")?);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}` (try `xtask --help`)"),
        }
    }
    Ok(opts)
}

fn print_help() {
    println!(
        "\
cargo xtask <task>

Tasks:
  regression                 Run the source-sink taint regression suite.
    --filter <name>          Only run cases whose name contains <name>.
    --tests-dir <dir>        Look for test cases under <dir> (default: auto-detect
                             `nightly/tests` or `tests` relative to the cwd).
    --jvm-samples <dir>      Directory of jvm-reader sample .java sources to
                             compile and check (default: auto-detect
                             `jvm-reader/tests/sample`). Also drive the
                             dex-reader checks (compiled down to .dex).
    --dex-apk <path>         Real-world APK to parse in the dex-reader smoke
                             test (default: auto-detect
                             `xtask/tests/dex/com.noto_54.apk`).

  taintbench                 Run the TaintBench source-sink regression suite:
                             analyze each app's APK with ctadl and match the
                             reported flows against the app's ground-truth
                             findings (on source and sink location only).
    --apps-dir <dir>         Directory of per-app subdirs, each with
                             findings.json + model.json (+ optional
                             expected.json baseline). Default: auto-detect
                             `taintbench/apps`.
    --apk <name>=<path>      APK to analyze for app <name> (the dir name).
                             Repeatable. Apps with no APK are skipped.
    --filter <name>          Only run apps whose name contains <name>.
"
    );
}
