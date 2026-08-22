//! `cargo xtask regression`: full-read sweep + golden dump diff over the
//! corpus (PLAN.md §5.3).
//!
//! Corpus: every .dll/.exe/.netmodule/.winmd under the Cecil test resource
//! directories plus tests/fixtures. `text_file.txt` is the must-fail case.
//! For each input: (1) full dump — every type, member, signature, custom
//! attribute, constant and method body decodes without a panic; (2) if a
//! golden exists under tests/expected/, the dump must match it byte-for-byte.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub const CORPUS_DIRS: &[&str] = &[
    "extern/cecil/Test/Resources/assemblies",
    "extern/cecil/rocks/Test/Resources/assemblies",
    "extern/cecil/symbols/mdb/Test/Resources/assemblies",
    "extern/cecil/symbols/pdb/Test/Resources/assemblies",
    "tests/fixtures",
];

/// Inputs that are not ECMA-335 CLI images (or intentionally malformed): the
/// sweep asserts these FAIL to load.
const MUST_FAIL: &[&str] = &["text_file.txt"];

/// Compared side-by-side but too big to commit goldens for.
pub const NO_GOLDEN: &[&str] = &[
    "Mono.Android.dll",
    "System.Private.Xml.dll",
    "mscorlib.dll",
    "FSharp.Core.dll",
    "cecil.dll",
];

/// Golden file name for a corpus input: corpus-dir-tagged, because several
/// inputs share a file name across corpus directories.
pub fn golden_name(path: &Path) -> String {
    let name = path.file_name().unwrap().to_string_lossy();
    let dir = path.parent().and_then(|p| p.to_str()).unwrap_or("");
    let tag = if dir.contains("rocks") {
        "rocks"
    } else if dir.contains("mdb") {
        "mdb"
    } else if dir.contains("symbols/pdb") {
        "pdb"
    } else if dir.ends_with("fixtures") {
        "fixtures"
    } else {
        "main"
    };
    format!("{tag}-{name}.dump")
}

pub fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in CORPUS_DIRS {
        let dir = root.join(dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "dll" | "exe" | "netmodule" | "winmd") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let filter = args.iter().find(|a| !a.starts_with("--")).cloned();
    let files = corpus(root);
    let mut failures = 0usize;
    let mut passed = 0usize;

    // must-fail check
    for name in MUST_FAIL {
        let path = root.join("extern/cecil/Test/Resources/assemblies").join(name);
        if let Ok(data) = std::fs::read(&path) {
            if cil_reader::Module::read(&data).is_ok() {
                eprintln!("FAIL {name}: expected a BadImage error");
                failures += 1;
            } else {
                passed += 1;
            }
        }
    }

    let all_sections: Vec<cil_dump::Section> =
        cil_dump::ALL_SECTIONS.iter().map(|&(_, s)| s).collect();

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some(f) = &filter
            && !name.contains(f.as_str())
        {
            continue;
        }
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("FAIL {name}: read: {e}");
                failures += 1;
                continue;
            }
        };
        let result = std::panic::catch_unwind(|| -> Result<String, cil_reader::Error> {
            let module = cil_reader::Module::read(&data)?;
            cil_dump::dump(&module, &all_sections)
        });
        let dump = match result {
            Ok(Ok(dump)) => dump,
            Ok(Err(e)) => {
                eprintln!("FAIL {name}: {e}");
                failures += 1;
                continue;
            }
            Err(_) => {
                eprintln!("FAIL {name}: panicked");
                failures += 1;
                continue;
            }
        };

        // Golden comparison, if one is committed.
        let golden_path = root.join("tests/expected").join(golden_name(path));
        if golden_path.exists() {
            let golden = std::fs::read_to_string(&golden_path).unwrap_or_default();
            if golden != dump {
                let diff_at = first_divergence(&golden, &dump);
                eprintln!("FAIL {name}: dump differs from golden at line {diff_at}");
                failures += 1;
                continue;
            }
        }
        passed += 1;
    }

    println!("regression: {passed} passed, {failures} failed");
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn first_divergence(a: &str, b: &str) -> usize {
    for (i, (la, lb)) in a.lines().zip(b.lines()).enumerate() {
        if la != lb {
            eprintln!("  golden: {la}");
            eprintln!("  actual: {lb}");
            return i + 1;
        }
    }
    a.lines().count().min(b.lines().count()) + 1
}
