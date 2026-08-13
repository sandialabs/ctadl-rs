//! Thin wrappers around [`std::process::Command`].
//!
//! We deliberately build and run `Command`s directly rather than shelling out
//! through `sh -c`, so arguments never go through a shell and failures carry the
//! captured stdout/stderr for diagnostics.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// Locate an executable on `PATH`, mirroring `command -v`.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in windows_pathext() {
                let with_ext = candidate.with_extension(ext.trim_start_matches('.'));
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_pathext() -> Vec<String> {
    std::env::var_os("PATHEXT")
        .map(|v| {
            std::env::split_paths(&v)
                .filter_map(|p| p.to_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                ".COM".to_string(),
                ".EXE".to_string(),
                ".BAT".to_string(),
                ".CMD".to_string(),
            ]
        })
}

/// Run `cmd` to completion, capturing output. Errors if it cannot be spawned or
/// exits with a non-zero status; the error embeds stdout/stderr for context.
///
/// `what` is a short human description used in error messages (e.g. `"javac"`).
pub fn run_checked(mut cmd: Command, what: &str) -> Result<Output> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn `{what}`"))?;
    if !output.status.success() {
        bail!(
            "`{what}` failed (exit {})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        );
    }
    Ok(output)
}

/// Like [`run_checked`] but returns the captured stdout as a `String`.
pub fn capture_stdout(cmd: Command, what: &str) -> Result<String> {
    let output = run_checked(cmd, what)?;
    String::from_utf8(output.stdout).with_context(|| format!("`{what}` produced non-UTF-8 output"))
}

/// Run `cmd` to completion, killing it if it takes longer than `timeout`.
///
/// Used for probes that must never become the hang they are meant to detect: a
/// JDK-less macOS answers some `java` invocations by spinning forever rather
/// than failing, and a probe that inherits that behaviour is worthless.
///
/// Output is captured, so `cmd` must not produce more than a pipe buffer's worth
/// (~64 KB) before exiting -- we only read the pipes once it has finished. That
/// holds for the version probes this is used for; do not reuse it for chatty
/// commands without draining the pipes concurrently.
pub fn run_with_timeout(mut cmd: Command, what: &str, timeout: Duration) -> Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{what}`"))?;

    let start = Instant::now();
    loop {
        match child
            .try_wait()
            .with_context(|| format!("failed to wait on `{what}`"))?
        {
            Some(_) => break,
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("`{what}` did not finish within {timeout:?}; killed it");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    child
        .wait_with_output()
        .with_context(|| format!("failed to collect output of `{what}`"))
}

/// Remove `dir` if it exists, then recreate it empty. Used for per-test scratch
/// directories so each run starts clean (matching the old scripts' `rm -rf`).
pub fn fresh_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("failed to clear {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(())
}
