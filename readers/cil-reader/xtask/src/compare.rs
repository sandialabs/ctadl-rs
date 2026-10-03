//! `cargo xtask compare`: run cil-dump (in-process) and CecilDump side by
//! side over the corpus and diff (PLAN.md §6). `cargo xtask bless` is the
//! same run with the Cecil side saved as goldens in tests/expected/.
//!
//! Needs `dotnet` on PATH (nix devshell) to build/run CecilDump.

use crate::regression::{NO_GOLDEN, corpus, golden_name};
use std::path::Path;
use std::process::{Command, ExitCode};

/// Excluded from comparison (documented):
/// - *.winmd: Cecil applies WinRT projections on Windows metadata (renamed
///   types, virtual assembly refs); we read the raw metadata (PLAN.md
///   non-goal).
fn compare_skip(name: &str) -> bool {
    name.ends_with(".winmd")
}

fn build_cecil_dump(root: &Path) -> Result<std::path::PathBuf, String> {
    let dll = root.join("tools/CecilDump/bin/Release/net8.0/CecilDump.dll");
    let status = Command::new("dotnet")
        .args(["build", "-c", "Release", "--nologo", "-v", "q"])
        .current_dir(root.join("tools/CecilDump"))
        .status()
        .map_err(|e| format!("dotnet: {e}"))?;
    if !status.success() {
        return Err("CecilDump build failed".into());
    }
    Ok(dll)
}

fn cecil_dump(dll: &Path, input: &Path) -> Result<String, String> {
    let output = Command::new("dotnet")
        .arg(dll)
        .arg(input)
        .output()
        .map_err(|e| format!("dotnet: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "CecilDump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn rust_dump(input: &Path) -> Result<String, String> {
    let data = std::fs::read(input).map_err(|e| e.to_string())?;
    let module = cil_reader::Module::read(&data).map_err(|e| e.to_string())?;
    let sections: Vec<cil_dump::Section> = cil_dump::ALL_SECTIONS.iter().map(|&(_, s)| s).collect();
    cil_dump::dump(&module, &sections).map_err(|e| e.to_string())
}

pub fn run(root: &Path, args: &[String], bless: bool) -> ExitCode {
    let filter = args.iter().find(|a| !a.starts_with("--")).cloned();
    let dll = match build_cecil_dump(root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    if bless {
        std::fs::create_dir_all(root.join("tests/expected")).unwrap();
    }

    let max_hunks: usize = 3;
    let mut identical = 0usize;
    let mut different = 0usize;
    let mut errors = 0usize;

    for path in corpus(root) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if compare_skip(&name) {
            continue;
        }
        if let Some(f) = &filter
            && !name.contains(f.as_str())
        {
            continue;
        }

        let cecil = match cecil_dump(&dll, &path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("ERROR {name} (cecil): {e}");
                errors += 1;
                continue;
            }
        };
        let rust = match rust_dump(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("ERROR {name} (rust): {e}");
                errors += 1;
                continue;
            }
        };

        if bless && !NO_GOLDEN.contains(&name.as_str()) {
            std::fs::write(root.join("tests/expected").join(golden_name(&path)), &cecil).unwrap();
        }

        if cecil == rust {
            identical += 1;
            continue;
        }
        different += 1;
        eprintln!("DIFF {name}:");
        let mut shown = 0;
        for (i, (lc, lr)) in cecil.lines().zip(rust.lines()).enumerate() {
            if lc != lr {
                eprintln!("  line {}:", i + 1);
                eprintln!("    cecil: {lc}");
                eprintln!("    rust:  {lr}");
                shown += 1;
                if shown >= max_hunks {
                    break;
                }
            }
        }
        let (nc, nr) = (cecil.lines().count(), rust.lines().count());
        if nc != nr {
            eprintln!("  line counts differ: cecil {nc}, rust {nr}");
        }
    }

    println!("compare: {identical} identical, {different} different, {errors} errors");
    if different + errors > 0 && !bless {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
