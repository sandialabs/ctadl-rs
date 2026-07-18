//! Test-case discovery.
//!
//! A regression case is a source file paired with a JSON file that holds its
//! known answer (`expected_lines`) and the ctadl query model. We discover cases
//! by pairing each source with its config rather than hard-coding a list, so
//! adding a `.java` + matching `.json` is enough to register a new test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};

use crate::exec;

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
    /// Python source (`.py`), whose SARIF locations already carry source lines.
    /// `interpreter` pins the Python used (one case per installed version, so a
    /// single run covers them all); `None` falls back to `$PYTHON`/`python3`.
    Python {
        source: PathBuf,
        query: PathBuf,
        interpreter: Option<PathBuf>,
    },
}

impl Kind {
    pub fn frontend(&self) -> Frontend {
        match self {
            Kind::Dex { .. } => Frontend::Dex,
            Kind::Jvm { .. } => Frontend::Jvm,
            Kind::Pcode { .. } => Frontend::Pcode,
            Kind::Python { .. } => Frontend::Python,
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
    Python,
}

impl Frontend {
    pub const ALL: &'static [Frontend] = &[
        Frontend::Dex,
        Frontend::Jvm,
        Frontend::Pcode,
        Frontend::Python,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Dex => "dex",
            Frontend::Jvm => "jvm",
            Frontend::Pcode => "pcode",
            Frontend::Python => "python",
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
            "python" | "py" => Ok(Frontend::Python),
            other => {
                bail!("unknown frontend `{other}` (expected one of: dex, jvm, pcode, python)")
            }
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
    cases.extend(discover_python(&tests_dir.join("python"))?);
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

/// Pair each `foo.py` with a query JSON (`foo-query.json`, else a shared
/// `query.json`). Names are prefixed `Python:` so they never collide with a
/// like-named C/Java case.
///
/// Each pairing is expanded across every installed versioned interpreter (see
/// [`discover_python_interpreters`]), so one regression run exercises the Python
/// frontend on every Python version present. The case name carries the version
/// (e.g. `Python:closure [3.13]`) so a per-version failure is unambiguous. When no
/// versioned interpreter is found, a single case falls back to `$PYTHON`/`python3`.
fn discover_python(py_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !py_dir.is_dir() {
        return Ok(cases);
    }
    let interpreters = discover_python_interpreters();
    for entry in read_dir_sorted(py_dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let stem = file_stem(&entry)?;
        let specific = py_dir.join(format!("{stem}-query.json"));
        let shared = py_dir.join("query.json");
        let query = if specific.is_file() {
            specific
        } else if shared.is_file() {
            shared
        } else {
            continue;
        };
        let source = absolute(&entry)?;
        let query = absolute(&query)?;
        if interpreters.is_empty() {
            cases.push(TestCase {
                name: format!("Python:{stem}"),
                kind: Kind::Python {
                    source: source.clone(),
                    query: query.clone(),
                    interpreter: None,
                },
            });
        } else {
            for (label, path) in &interpreters {
                cases.push(TestCase {
                    name: format!("Python:{stem} [{label}]"),
                    kind: Kind::Python {
                        source: source.clone(),
                        query: query.clone(),
                        interpreter: Some(path.clone()),
                    },
                });
            }
        }
    }
    Ok(cases)
}

/// Discover the versioned Python interpreters to exercise the frontend against.
///
/// If `$PYTHON` is set it pins the run to that single interpreter (letting a
/// caller target one version). Otherwise every `python3.<minor>` / `python3<minor>`
/// on `PATH` (minors 8..=20, covering current + upcoming releases) is collected,
/// so one run covers all installed versions. The two naming forms are deduped by
/// the interpreter's real path (the Nix regression shell exposes `python313`; a
/// system install exposes `python3.13`, sometimes both). Empty when none are
/// found, so the caller falls back to the ambient `python3` (and self-skips if
/// even that is absent).
pub fn discover_python_interpreters() -> Vec<(String, PathBuf)> {
    if let Some(pinned) = std::env::var_os("PYTHON") {
        let path = PathBuf::from(pinned);
        let label = interpreter_label(&path).unwrap_or_else(|| "PYTHON".to_string());
        return vec![(label, path)];
    }
    let mut found = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for minor in 8..=20u32 {
        // One interpreter per minor version: the dotted and undotted names often
        // both resolve (the Nix shell exposes `python313`, `pkgs.python3` also
        // ships `python3.13`). Take the first that resolves so a single `3.13`
        // label -- and thus one scratch dir per case -- is used.
        let resolved = [format!("python3.{minor}"), format!("python3{minor}")]
            .into_iter()
            .find_map(|name| exec::which(&name));
        let Some(path) = resolved else { continue };
        // Belt-and-braces: never list the same interpreter twice (e.g. two minors
        // symlinked to one build), which would collide on the scratch directory.
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(canonical) {
            found.push((format!("3.{minor}"), path));
        }
    }
    found
}

/// Best-effort version label (`"3.13"`) for a pinned `$PYTHON`, parsed from its
/// file name (`python3.13` / `python313`); `None` if it is not version-suffixed.
fn interpreter_label(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("python3")?;
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("3.{rest}"))
    } else {
        None
    }
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
        // Tolerate stray whitespace/case from a comma-separated list.
        assert_eq!(" Pcode ".parse::<Frontend>().unwrap(), Frontend::Pcode);
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
