use std::path::{Path, PathBuf};
use std::process::Command;
use std::env;
use std::fs;
use crate::error::Error;

const MAXMEM: &str = "40G";
const LAUNCH_MODE: &str = "fg";
const VMARG_LIST: &str = "-XX:ParallelGCThreads=4 -XX:CICompilerCount=4 ";

pub fn run_ghidra_export(
    artifact_path: &Path,
    output_dir: &Path,
) -> Result<(), Error> {
    let ghidra_base = find_ghidra_base()?;
    let analyze_headless = find_analyze_headless(&ghidra_base)?;
    let script_dir = analyze_headless.parent().ok_or_else(|| {
        Error::PcodeConversion("Could not find script directory for Ghidra".to_string())
    })?;
    let launch_script = script_dir.join("launch.sh");

    let facts_dir = output_dir.join("facts");
    fs::create_dir_all(&facts_dir)?;

    let temp_project_dir = tempfile::tempdir()?;
    let project_path = temp_project_dir.path().to_string_lossy().to_string();
    let project_name = "headless";

    // Write ExportPcode.java to a temporary directory
    let script_temp_dir = tempfile::tempdir()?;
    let export_script_path = script_temp_dir.path().join("ExportPcode.java");
    fs::write(&export_script_path, include_str!("../../../../pcode-reader/ExportPcode.java"))?;

    let mut command = Command::new(&launch_script);
    command.args([
        LAUNCH_MODE,
        "jdk",
        "Ghidra-Headless",
        MAXMEM,
        VMARG_LIST,
        "ghidra.app.util.headless.AnalyzeHeadless",
        &project_path,
        project_name,
        "-import",
        &artifact_path.to_string_lossy(),
        "-deleteProject",
        "-postScript",
        "ExportPcode.java",
        &facts_dir.to_string_lossy(),
        "-scriptPath",
        &export_script_path.parent().unwrap().to_string_lossy(),
    ]);

    log::info!("Running Ghidra: {:?}", command);

    let status = command.status()?;

    if !status.success() {
        return Err(Error::PcodeConversion(format!(
            "Ghidra analyzeHeadless failed with status: {}",
            status
        )));
    }

    Ok(())
}

fn find_ghidra_base() -> Result<PathBuf, Error> {
    if let Ok(ghidra_home) = env::var("GHIDRA_HOME") {
        return Ok(PathBuf::from(ghidra_home));
    }

    if let Ok(ghidra_bin) = which::which("ghidra") {
        if let Ok(ghidra_bin) = ghidra_bin.canonicalize() {
            if let Some(parent) = ghidra_bin.parent() {
                return Ok(parent.to_path_buf());
            }
        }
    }

    Err(Error::PcodeConversion(
        "Could not find Ghidra. Set GHIDRA_HOME or add 'ghidra' to PATH.".to_string(),
    ))
}

fn find_analyze_headless(ghidra_base: &Path) -> Result<PathBuf, Error> {
    let candidates = [
        ghidra_base.parent().map(|p| p.join("lib/ghidra/support/analyzeHeadless")),
        Some(ghidra_base.join("support/analyzeHeadless")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(Error::PcodeConversion(format!(
        "Could not find Ghidra analyzeHeadless from ghidra directory {}",
        ghidra_base.display()
    )))
}

// No more need for find_export_script

