//! Test-case discovery.
//!
//! A regression case is a source file paired with a JSON file that holds its
//! known answer (`expected_lines`) and the ctadl query model. We discover cases
//! by pairing each source with its config rather than hard-coding a list, so
//! adding a `.java` + matching `.json` is enough to register a new test.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};

/// A single regression test case.
pub struct TestCase {
    /// Human-facing name, used for reporting and `--filter` matching.
    pub name: String,
    pub kind: Kind,
}

pub enum Kind {
    /// Java compiled to DEX, mapped back to source lines via a dex linemap.
    Dex { java: PathBuf, config: PathBuf },
    /// Java compiled to a JAR, mapped back to source lines via a jvm linemap.
    Jvm { java: PathBuf, config: PathBuf },
    /// C compiled to an ELF object, mapped back to lines via `addr2line`.
    Pcode { source: PathBuf, query: PathBuf },
    /// C parsed directly by the tree-sitter frontend, with source lines read
    /// straight off the SARIF regions (no compiler, no Ghidra).
    C { source: PathBuf, query: PathBuf },
}

impl Kind {
    pub fn frontend(&self) -> Frontend {
        match self {
            Kind::Dex { .. } => Frontend::Dex,
            Kind::Jvm { .. } => Frontend::Jvm,
            Kind::Pcode { .. } => Frontend::Pcode,
            Kind::C { .. } => Frontend::C,
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
    /// The tree-sitter C frontend (`ctadl import -l c`). Exercises the same
    /// `tests/c/*.c` sources as `Pcode`, but parses them directly and reads
    /// source lines off the SARIF regions instead of going through Ghidra.
    C,
}

impl Frontend {
    pub const ALL: &'static [Frontend] =
        &[Frontend::Dex, Frontend::Jvm, Frontend::Pcode, Frontend::C];

    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Dex => "dex",
            Frontend::Jvm => "jvm",
            Frontend::Pcode => "pcode",
            Frontend::C => "c",
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
            "c" => Ok(Frontend::C),
            other => bail!("unknown frontend `{other}` (expected one of: dex, jvm, pcode, c)"),
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
    bail!("could not find a tests directory (looked for `nightly/tests` and `tests`); pass --tests-dir")
}

/// Discover all cases under `tests_dir`, sorted by name for deterministic order.
pub fn discover(tests_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = discover_dex(&tests_dir.join("java"))?;
    cases.extend(discover_pcode(&tests_dir.join("c"))?);
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// Pair each `Foo.java` with its kebab-cased `foo.json`. A `.java` without a
/// matching config (e.g. `MultiImplFlow.java`) is skipped, exactly as the old
/// `tests.sh` skipped it.
fn discover_dex(java_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !java_dir.is_dir() {
        return Ok(cases);
    }
    for entry in read_dir_sorted(java_dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("java") {
            continue;
        }
        let stem = file_stem(&entry)?;
        // Prefer a JSON5 config (it can carry inline comments) over a plain JSON one.
        let kebab = to_kebab_case(&stem);
        let json5 = java_dir.join(format!("{kebab}.json5"));
        let json = java_dir.join(format!("{kebab}.json"));
        let config = if json5.is_file() { json5 } else { json };
        if config.is_file() {
            let java = absolute(&entry)?;
            let config = absolute(&config)?;
            cases.push(TestCase {
                name: stem.clone(),
                kind: Kind::Dex {
                    java: java.clone(),
                    config: config.clone(),
                },
            });
            cases.push(TestCase {
                name: format!("Jvm:{stem}"),
                kind: Kind::Jvm { java, config },
            });
        }
    }
    Ok(cases)
}

/// Pair each `foo.c` with a query JSON, then register it against *both* C
/// frontends. We prefer `foo-query.json`, falling back to a shared `query.json`.
///
/// The same source drives the `Pcode` frontend (compiled through Ghidra) and the
/// tree-sitter `C` frontend, mirroring how a single `.java` yields both a `Dex`
/// and a `Jvm` case. The two are distinct `TestCase`s with distinct names, so
/// each is selected, run, and reported on its own -- a C failure cannot mask or
/// fail a Pcode case and vice versa. The Pcode case keeps the bare stem so
/// existing `--filter`/names are unchanged; the C case is prefixed `C:`.
fn discover_pcode(c_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !c_dir.is_dir() {
        return Ok(cases);
    }
    for entry in read_dir_sorted(c_dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("c") {
            continue;
        }
        let stem = file_stem(&entry)?;
        let specific = c_dir.join(format!("{stem}-query.json"));
        let shared = c_dir.join("query.json");
        let query = if specific.is_file() {
            specific
        } else if shared.is_file() {
            shared
        } else {
            continue;
        };
        let source = absolute(&entry)?;
        let query = absolute(&query)?;
        cases.push(TestCase {
            name: stem.clone(),
            kind: Kind::Pcode {
                source: source.clone(),
                query: query.clone(),
            },
        });
        cases.push(TestCase {
            name: format!("C:{stem}"),
            kind: Kind::C { source, query },
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

/// Convert `CamelCase` to `kebab-case`: `ArrayListIteratorFlow` ->
/// `array-list-iterator-flow`.
fn to_kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{to_kebab_case, Frontend};

    #[test]
    fn frontend_parses() {
        assert_eq!("pcode".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert_eq!("jvm".parse::<Frontend>().unwrap(), Frontend::Jvm);
        assert_eq!("dex".parse::<Frontend>().unwrap(), Frontend::Dex);
        assert_eq!("c".parse::<Frontend>().unwrap(), Frontend::C);
        // Tolerate stray whitespace/case from a comma-separated list.
        assert_eq!(" Pcode ".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert_eq!(" C ".parse::<Frontend>().unwrap(), Frontend::C);
        assert!("bogus".parse::<Frontend>().is_err());
    }

    #[test]
    fn kebab() {
        assert_eq!(to_kebab_case("SourceSinkExample"), "source-sink-example");
        assert_eq!(
            to_kebab_case("ArrayListIteratorFlow"),
            "array-list-iterator-flow"
        );
        assert_eq!(
            to_kebab_case("CrossClassStaticFieldFlow"),
            "cross-class-static-field-flow"
        );
    }
}
