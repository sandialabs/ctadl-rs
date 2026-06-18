//! Test-case discovery.
//!
//! A regression case is a source file paired with a JSON file that holds its
//! known answer (`expected_lines`) and the ctadl query model. We discover cases
//! by pairing each source with its config rather than hard-coding a list, so
//! adding a `.java` + matching `.json` is enough to register a new test.

use std::path::{Path, PathBuf};

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
    /// C compiled to an ELF object, mapped back to lines via `addr2line`.
    Pcode { source: PathBuf, query: PathBuf },
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
        let config = java_dir.join(format!("{}.json", to_kebab_case(&stem)));
        if config.is_file() {
            cases.push(TestCase {
                name: stem,
                kind: Kind::Dex {
                    java: absolute(&entry)?,
                    config: absolute(&config)?,
                },
            });
        }
    }
    Ok(cases)
}

/// Pair each `foo.c` with a query JSON. We prefer `foo-query.json`, falling back
/// to a shared `query.json` (the single C case here uses the latter).
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
        cases.push(TestCase {
            name: stem,
            kind: Kind::Pcode {
                source: absolute(&entry)?,
                query: absolute(&query)?,
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
    use super::to_kebab_case;

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
