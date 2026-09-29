//! Real Android apps whose data flow crosses into native code and back.
//!
//! The JNI cases under `tests/jni` build a small native method from source; these import a real,
//! permissively licensed F-Droid app instead, so the APK importer, Ghidra's disassembly of a
//! stripped C++ library, the `RegisterNatives` scan, signature recovery, and the JNI bridge all run
//! on the kind of input they exist for. The APKs are not in the repo: the flake fetches each at a
//! pinned hash and names the directory in `CTADL_ANDROID_NATIVE_APKS`, which the dev shell and the
//! regression check set. Without it the cases skip.
//!
//! A spec pins what the bridge links and names flows that must be reported. A flow is matched on
//! its taint label and on prefixes of the SARIF `sourceCallee` and `sinkCallee`, so a spec can say
//! "a value read in Java reaches `Game::move` in the library" without spelling out an address.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSpec {
    /// APK file name inside the `CTADL_ANDROID_NATIVE_APKS` directory.
    pub apk: PathBuf,
    /// Query model path relative to this spec file, or absolute. Passed to `index` as well as to
    /// `query`, so the index's dispatch-model check sees the same sources and sinks.
    pub model: PathBuf,
    /// What the `jni bridge:` summary line must report.
    pub bridge: BridgeCounts,
    /// Flows the query must report.
    pub flows: Vec<FlowSpec>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BridgeCounts {
    pub linked: usize,
    pub registered: usize,
    pub prototype_mismatch: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    /// The source's taint label, as the model names it.
    pub label: String,
    /// Prefix of the flow's `sourceCallee`.
    pub source: String,
    /// Prefix of the flow's `sinkCallee`.
    pub sink: String,
    /// At least this many matching flows. Default 1.
    #[serde(default = "one")]
    pub min: usize,
}

fn one() -> usize {
    1
}

pub fn load_spec(path: &Path) -> Result<CaseSpec> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    json5::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Where the fetched APKs live; see the module docs.
pub const APKS_ENV: &str = "CTADL_ANDROID_NATIVE_APKS";

/// The case's APK (`None` when `CTADL_ANDROID_NATIVE_APKS` is unset) and model file.
pub fn resolve_paths(spec_path: &Path, spec: &CaseSpec) -> (Option<PathBuf>, PathBuf) {
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let apk = std::env::var_os(APKS_ENV).map(|dir| PathBuf::from(dir).join(&spec.apk));
    (apk, base.join(&spec.model))
}

/// Reads the counts off the index log's `jni bridge: N native method(s): L linked (R registered,
/// S from summary, M prototype mismatch), ...` line.
pub fn parse_bridge_line(index_log: &str) -> Option<BridgeCounts> {
    let line = index_log
        .lines()
        .find(|l| l.contains("jni bridge: ") && l.contains(" native method(s): "))?;
    let number_before = |word: &str| -> Option<usize> {
        let at = line.find(word)?;
        line[..at]
            .split_whitespace()
            .last()?
            .trim_start_matches('(')
            .parse()
            .ok()
    };
    Some(BridgeCounts {
        linked: number_before(" linked")?,
        registered: number_before(" registered")?,
        prototype_mismatch: number_before(" prototype mismatch")?,
    })
}

/// Checks the bridge line against the spec. `None` when it matches.
pub fn check_bridge(index_log: &str, expected: &BridgeCounts) -> Option<String> {
    match parse_bridge_line(index_log) {
        None => Some("index.log has no `jni bridge:` summary line".into()),
        Some(found) if &found != expected => Some(format!(
            "expected the bridge to report {expected:?}, but it reported {found:?}; see index.log"
        )),
        Some(_) => None,
    }
}

/// Checks that the SARIF reports every flow the spec names. `None` when it does.
pub fn check_flows(sarif: &Path, flows: &[FlowSpec]) -> Result<Option<String>> {
    let text =
        std::fs::read_to_string(sarif).with_context(|| format!("reading {}", sarif.display()))?;
    let doc: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", sarif.display()))?;
    let paths: Vec<&Value> = doc["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|run| run["results"].as_array().into_iter().flatten())
        .filter(|r| r["ruleId"] == "C0001.tainted-path" && r.get("codeFlows").is_some())
        .collect();
    let mut missing = Vec::new();
    for flow in flows {
        let found = paths
            .iter()
            .filter(|r| {
                let p = &r["properties"];
                let labelled = p["taintLabels"]
                    .as_array()
                    .is_some_and(|ls| ls.iter().any(|l| l == flow.label.as_str()));
                let starts = |key: &str, prefix: &str| {
                    p[key].as_str().is_some_and(|s| s.starts_with(prefix))
                };
                labelled && starts("sourceCallee", &flow.source) && starts("sinkCallee", &flow.sink)
            })
            .count();
        if found < flow.min {
            missing.push(format!(
                "{} -> {} ({}): {found} flow(s), expected at least {}",
                flow.source, flow.sink, flow.label, flow.min
            ));
        }
    }
    Ok((!missing.is_empty()).then(|| {
        format!(
            "the query reported {} flow(s), but: {}",
            paths.len(),
            missing.join("; ")
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_bridge_summary_line() {
        let log =
            "querying\njni bridge: 57 native method(s): 57 linked (51 registered, 0 from summary, \
                   2 prototype mismatch), 0 unresolved, 0 ambiguous\n";
        assert_eq!(
            parse_bridge_line(log),
            Some(BridgeCounts {
                linked: 57,
                registered: 51,
                prototype_mismatch: 2
            })
        );
        assert!(check_bridge(
            log,
            &BridgeCounts {
                linked: 57,
                registered: 51,
                prototype_mismatch: 0
            }
        )
        .is_some());
    }

    #[test]
    fn a_log_without_the_line_fails_the_check() {
        let expected = BridgeCounts {
            linked: 1,
            registered: 0,
            prototype_mismatch: 0,
        };
        assert!(check_bridge("nothing here", &expected).is_some());
    }

    #[test]
    fn the_chess_spec_parses() {
        let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("nightly/tests/android-native/jwtc-android-chess.json5");
        if spec.is_file() {
            let spec = load_spec(&spec).expect("parsing the chess spec");
            assert!(!spec.flows.is_empty());
        }
    }
}
