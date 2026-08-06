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
    /// Lua source imported directly; SARIF regions carry source lines, so no
    /// linemap or compilation step is needed.
    Lua { source: PathBuf, query: PathBuf },
    /// Java `native` methods plus the shared library implementing them,
    /// co-indexed as two imports of one project so the JNI bridge has both
    /// halves to join. The only two-import case kind.
    Jni {
        java: PathBuf,
        native: PathBuf,
        config: PathBuf,
        /// A declarative bridging model to use *instead of* the built-in JNI pass. When set,
        /// the case indexes with `--no-jni-bridge -m <this>`, so it asserts exactly what the
        /// built-in case asserts and the two are a direct A/B.
        bridge: Option<PathBuf>,
        /// How the two artifacts reach `ctadl import`. Every packaging asserts exactly the
        /// same thing, so the set is a direct A/B on the importer alone.
        packaging: Packaging,
    },
}

/// How a [`Kind::Jni`] case hands its two halves to `ctadl import`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Packaging {
    /// Import the DEX and the shared library as two separate artifacts. The baseline.
    Separate,
    /// Package both into one APK and import it once, as an ordinary Android app ships.
    /// Exercises the APK importer finding, extracting, and disassembling `lib/<abi>`.
    SingleApk,
    /// Package each into an APK of its own -- a base APK holding the DEX and a
    /// `config.<abi>.apk` holding only the library -- and import both. This is how an
    /// Android App Bundle is distributed, and what an XAPK download unpacks to: the
    /// native half arrives in an APK with no `classes*.dex` in it at all.
    SplitApks,
}

impl Kind {
    pub fn frontend(&self) -> Frontend {
        match self {
            Kind::Dex { .. } => Frontend::Dex,
            Kind::Jvm { .. } => Frontend::Jvm,
            Kind::Pcode { .. } => Frontend::Pcode,
            Kind::C { .. } => Frontend::C,
            Kind::Lua { .. } => Frontend::Lua,
            Kind::Jni { .. } => Frontend::Jni,
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
    Lua,
    /// Not a frontend of its own: the JNI bridge, which needs the dex *and*
    /// pcode toolchains at once. It selects separately so `--frontend jni` runs
    /// the two-import cases without also running every dex and pcode case.
    Jni,
}

impl Frontend {
    pub const ALL: &'static [Frontend] = &[
        Frontend::Dex,
        Frontend::Jvm,
        Frontend::Pcode,
        Frontend::C,
        Frontend::Lua,
        Frontend::Jni,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Frontend::Dex => "dex",
            Frontend::Jvm => "jvm",
            Frontend::Pcode => "pcode",
            Frontend::C => "c",
            Frontend::Lua => "lua",
            Frontend::Jni => "jni",
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
            "lua" => Ok(Frontend::Lua),
            "jni" => Ok(Frontend::Jni),
            other => {
                bail!("unknown frontend `{other}` (expected one of: dex, jvm, pcode, c, lua, jni)")
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
    cases.extend(discover_lua(&tests_dir.join("lua"))?);
    cases.extend(discover_jni(&tests_dir.join("jni"))?);
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

/// Pair each `foo.lua` -- or each `foo/` directory of Lua sources, imported whole
/// as one `require` root -- with its `foo-query.json` config. A source without a
/// matching query is skipped, mirroring the pcode discovery. The name is prefixed
/// with `Lua:` so the reports (and `--filter`) distinguish it from other
/// frontends' cases.
fn discover_lua(lua_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !lua_dir.is_dir() {
        return Ok(cases);
    }
    for entry in read_dir_sorted(lua_dir)? {
        if !entry.is_dir() && entry.extension().and_then(|e| e.to_str()) != Some("lua") {
            continue;
        }
        let stem = file_stem(&entry)?;
        let query = lua_dir.join(format!("{stem}-query.json"));
        if !query.is_file() {
            continue;
        }
        cases.push(TestCase {
            name: format!("Lua:{stem}"),
            kind: Kind::Lua {
                source: absolute(&entry)?,
                query: absolute(&query)?,
            },
        });
    }
    Ok(cases)
}

/// Pair each `Foo.java` with its sibling `Foo.c` -- the shared library implementing
/// its `native` methods -- and its kebab-cased `foo.json`. All three must be present:
/// a case here is by construction about the boundary between the two artifacts, so a
/// `.java` with no `.c` (or no config) is not a degenerate JNI case, it is not one at
/// all. Cases report as `Jni:Foo`.
fn discover_jni(jni_dir: &Path) -> Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    if !jni_dir.is_dir() {
        return Ok(cases);
    }
    for entry in read_dir_sorted(jni_dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("java") {
            continue;
        }
        let stem = file_stem(&entry)?;
        let native = jni_dir.join(format!("{stem}.c"));
        if !native.is_file() {
            continue;
        }
        // Prefer a JSON5 config (it can carry inline comments), as the dex cases do.
        let kebab = to_kebab_case(&stem);
        let json5 = jni_dir.join(format!("{kebab}.json5"));
        let json = jni_dir.join(format!("{kebab}.json"));
        let config = if json5.is_file() { json5 } else { json };
        if !config.is_file() {
            continue;
        }
        cases.push(TestCase {
            name: format!("Jni:{stem}"),
            kind: Kind::Jni {
                java: absolute(&entry)?,
                native: absolute(&native)?,
                config: absolute(&config)?,
                bridge: None,
                packaging: Packaging::Separate,
            },
        });
        // The same two artifacts and the same claims, but packaged the way a real
        // Android app ships them and imported with one command. If the APK importer
        // finds and disassembles `lib/<abi>` correctly, both cases pass identically.
        cases.push(TestCase {
            name: format!("Jni:{stem}+apk"),
            kind: Kind::Jni {
                java: absolute(&entry)?,
                native: absolute(&native)?,
                config: absolute(&config)?,
                bridge: None,
                packaging: Packaging::SingleApk,
            },
        });
        // And the way an app *bundle* ships them: two APKs, the native one carrying no
        // DEX at all. Same claims again, so this is an A/B on whether a DEX-less APK
        // imports and co-indexes like any other native half.
        cases.push(TestCase {
            name: format!("Jni:{stem}+split-apks"),
            kind: Kind::Jni {
                java: absolute(&entry)?,
                native: absolute(&native)?,
                config: absolute(&config)?,
                bridge: None,
                packaging: Packaging::SplitApks,
            },
        });
        // A sibling `<kebab>.bridge.jsonl` turns the case into an A/B: the same two artifacts
        // and the same claims, joined by a hand-written `model.bridge` under
        // `--no-jni-bridge` instead of by the built-in pass. If the declarative construct is
        // as expressive as the pass for this boundary, both cases pass identically.
        let bridge = jni_dir.join(format!("{kebab}.bridge.jsonl"));
        if bridge.is_file() {
            cases.push(TestCase {
                name: format!("Jni:{stem}+bridge"),
                kind: Kind::Jni {
                    java: absolute(&entry)?,
                    native: absolute(&native)?,
                    config: absolute(&config)?,
                    bridge: Some(absolute(&bridge)?),
                    packaging: Packaging::Separate,
                },
            });
        }
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
    use super::{discover_jni, to_kebab_case, Frontend, Kind, Packaging};

    /// Every shipped JNI case is discovered, and one that carries a `<kebab>.bridge.jsonl`
    /// yields a second, declaratively-bridged case beside it.
    ///
    /// This is the cheap half of the A/B: the runner itself needs `javac`, `dx`, a C compiler
    /// and Ghidra, so it only runs in the nightly environment. That the pair is *discovered* --
    /// and that the two halves make the same claims, since they share a config file -- is
    /// checkable everywhere.
    #[test]
    fn a_bridge_model_beside_a_jni_case_yields_an_ab_pair() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("nightly/tests/jni");
        if !dir.is_dir() {
            return;
        }
        let cases = discover_jni(&dir).expect("discovering jni cases");
        let names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        for stem in ["JniFlow", "JniArgShift"] {
            assert!(
                names.contains(&format!("Jni:{stem}").as_str()),
                "the built-in case is missing: {names:?}"
            );
            assert!(
                names.contains(&format!("Jni:{stem}+bridge").as_str()),
                "the declarative A/B case is missing: {names:?}"
            );
            assert!(
                names.contains(&format!("Jni:{stem}+apk").as_str()),
                "the APK-packaged A/B case is missing: {names:?}"
            );
            assert!(
                names.contains(&format!("Jni:{stem}+split-apks").as_str()),
                "the split-APK A/B case is missing: {names:?}"
            );
        }
        // The variants of each case differ in exactly one thing: which mechanism joins
        // the boundary, or how the artifacts are packaged. Same sources, same config, so
        // the same assertions.
        for stem in ["JniFlow", "JniArgShift"] {
            let of = |suffix: &str| {
                let name = format!("Jni:{stem}{suffix}");
                cases
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| match &c.kind {
                        Kind::Jni {
                            java,
                            native,
                            config,
                            bridge,
                            packaging,
                        } => (
                            (java.clone(), native.clone(), config.clone()),
                            bridge.clone(),
                            *packaging,
                        ),
                        _ => panic!("expected a Jni case for {name}"),
                    })
                    .unwrap_or_else(|| panic!("no case named {name}"))
            };
            let (artifacts, bridge, packaging) = of("");
            assert!(bridge.is_none(), "the built-in case uses no model");
            assert_eq!(packaging, Packaging::Separate);

            // Only the mechanism differs.
            let (ab_artifacts, ab_bridge, ab_packaging) = of("+bridge");
            assert_eq!(artifacts, ab_artifacts);
            assert!(ab_bridge.is_some(), "the A/B case supplies one");
            assert_eq!(ab_packaging, Packaging::Separate);

            // Only the packaging differs; both use the built-in bridge.
            for (suffix, expected) in [
                ("+apk", Packaging::SingleApk),
                ("+split-apks", Packaging::SplitApks),
            ] {
                let (packaged_artifacts, packaged_bridge, packaged) = of(suffix);
                assert_eq!(artifacts, packaged_artifacts);
                assert!(
                    packaged_bridge.is_none(),
                    "{suffix} uses the built-in bridge"
                );
                assert_eq!(packaged, expected);
            }
        }
    }

    /// A case with no `.bridge.jsonl` gets the three built-in variants and no A/B one. That is
    /// how `JniRegister` ships: its boundary is joined by a `RegisterNatives` table recovered
    /// from the library, and a hand-written bridge model would be testing something else.
    #[test]
    fn a_jni_case_without_a_bridge_model_yields_no_ab_pair() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("nightly/tests/jni");
        if !dir.is_dir() {
            return;
        }
        let cases = discover_jni(&dir).expect("discovering jni cases");
        let names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        for suffix in ["", "+apk", "+split-apks"] {
            assert!(
                names.contains(&format!("Jni:JniRegister{suffix}").as_str()),
                "the RegisterNatives case is missing: {names:?}"
            );
        }
        assert!(
            !names.contains(&"Jni:JniRegister+bridge"),
            "no bridge model ships beside it, so there is nothing to A/B against"
        );
    }

    #[test]
    fn frontend_parses() {
        assert_eq!("pcode".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert_eq!("jvm".parse::<Frontend>().unwrap(), Frontend::Jvm);
        assert_eq!("dex".parse::<Frontend>().unwrap(), Frontend::Dex);
        assert_eq!("c".parse::<Frontend>().unwrap(), Frontend::C);
        assert_eq!("jni".parse::<Frontend>().unwrap(), Frontend::Jni);
        // Tolerate stray whitespace/case from a comma-separated list.
        assert_eq!(" Pcode ".parse::<Frontend>().unwrap(), Frontend::Pcode);
        assert_eq!(" C ".parse::<Frontend>().unwrap(), Frontend::C);
        assert!("bogus".parse::<Frontend>().is_err());
        // Every variant round-trips through its own name, so `--frontend <x>`
        // accepts anything the report prints.
        for frontend in Frontend::ALL {
            assert_eq!(frontend.as_str().parse::<Frontend>().unwrap(), *frontend);
        }
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
