//! Test-case discovery.
//!
//! A regression case is a source file under one of the tests directory's case
//! folders. We discover cases by listing those folders rather than hard-coding
//! a list, so adding a source file is enough to register a new test.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};

/// A single regression test case.
pub struct TestCase {
    /// Human-facing name, used for reporting and `--filter` matching.
    pub name: String,
    pub kind: Kind,
}

pub enum Kind {
    /// C compiled to an ELF object and exported by Ghidra, checked on the facts the export
    /// writes. See `regression::check_pcode_facts`.
    PcodeFacts { source: PathBuf },
}

impl Kind {
    pub fn frontend(&self) -> Frontend {
        match self {
            Kind::PcodeFacts { .. } => Frontend::Pcode,
        }
    }
}

/// Which analyzer frontend a check exercises. `--frontend` selects on this so a
/// subset can be run without paying for the other frontends' toolchains: the
/// per-frontend reader checks are skipped entirely, not just filtered out of the
/// report.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Frontend {
    Dex,
    Jvm,
    Pcode,
    /// The cil-reader and CIL frontend checks (see [`crate::cil`]).
    Cil,
}

impl Frontend {
    pub const ALL: &'static [Frontend] =
        &[Frontend::Dex, Frontend::Jvm, Frontend::Pcode, Frontend::Cil];

    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Dex => "dex",
            Frontend::Jvm => "jvm",
            Frontend::Pcode => "pcode",
            Frontend::Cil => "cil",
        }
    }
}

impl FromStr for Frontend {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dex" => Ok(Frontend::Dex),
            "jvm" => Ok(Frontend::Jvm),
            "pcode" => Ok(Frontend::Pcode),
            "cil" => Ok(Frontend::Cil),
            other => bail!("unknown frontend `{other}` (expected one of: dex, jvm, pcode, cil)"),
        }
    }
}

/// Resolve the directory holding test cases.
///
/// With no explicit override we accept either layout: `nightly/tests` (running
/// `cargo xtask` from the repo root) or `tests` (running inside the Nix check,
/// whose cwd is the copied `nightly` dir).
pub fn resolve_tests_dir(override_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = override_dir {
        if dir.is_dir() {
            return Ok(dir.to_path_buf());
        }
        bail!("--tests-dir {} is not a directory", dir.display());
    }
    for candidate in ["nightly/tests", "tests"] {
        let path = Path::new(candidate);
        if path.is_dir() {
            return Ok(path.to_path_buf());
        }
    }
    bail!(
        "could not find a tests directory (looked for `nightly/tests` and `tests`); pass --tests-dir"
    )
}

/// Discover all cases under `tests_dir`, sorted by name for deterministic order.
pub fn discover(tests_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = discover_pcode_facts(&tests_dir.join("pcode-facts"))?;
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// Each `foo.c`, as the case `Facts:foo`.
fn discover_pcode_facts(dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !dir.is_dir() {
        return Ok(cases);
    }
    for entry in read_dir_sorted(dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("c") {
            continue;
        }
        cases.push(TestCase {
            name: format!("Facts:{}", file_stem(&entry)?),
            kind: Kind::PcodeFacts {
                source: absolute(&entry)?,
            },
        });
    }
    Ok(cases)
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .map(|res| res.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate {}", dir.display()))?;
    paths.sort();
    Ok(paths)
}

/// Canonicalize a path to absolute. Cases run in their own scratch directory,
/// so every input path must be absolute to remain valid after we change cwd.
fn absolute(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).with_context(|| format!("failed to resolve {}", path.display()))
}

fn file_stem(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .with_context(|| format!("invalid file name: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::Frontend;

    #[test]
    fn frontend_parses() {
        assert_eq!("pcode".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert_eq!("jvm".parse::<Frontend>().unwrap(), Frontend::Jvm);
        assert_eq!("dex".parse::<Frontend>().unwrap(), Frontend::Dex);
        assert_eq!("cil".parse::<Frontend>().unwrap(), Frontend::Cil);
        // Tolerate stray whitespace/case from a comma-separated list.
        assert_eq!(" Pcode ".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert!("bogus".parse::<Frontend>().is_err());
        // Every variant round-trips through its own name, so `--frontend <x>`
        // accepts anything the report prints.
        for frontend in Frontend::ALL {
            assert_eq!(frontend.as_str().parse::<Frontend>().unwrap(), *frontend);
        }
    }
}
