//! Parsing of analyzer outputs and known-answer checking.
//!
//! These replace the old `jq`/`grep` pipelines with structured JSON parsing.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// Read the `expected_lines` array from a test config. An empty array denotes a
/// negative test (no flow expected). A missing key is an error, matching the
/// old `jq -e '.expected_lines'` check.
pub fn read_expected_lines(config: &Path) -> Result<Vec<i64>> {
    let value = read_json(config)?;
    let array = value
        .get("expected_lines")
        .with_context(|| format!("no `expected_lines` key in {}", config.display()))?;
    line_array(array, "expected_lines", config)
}

/// Read the optional `unexpected_lines` array from a test config: source lines that
/// must carry no flow. A missing key means the case makes no such claim, so existing
/// configs need not mention it.
///
/// This is the counterpart to [`read_expected_lines`] for cases whose point is that
/// some neighbouring line stays clean -- a sink on the struct field that was never
/// written, say. Without it such a case can only assert the lines that *are* tainted,
/// and would keep passing if the untainted line later became tainted.
pub fn read_unexpected_lines(config: &Path) -> Result<Vec<i64>> {
    let value = read_json(config)?;
    match value.get("unexpected_lines") {
        Some(array) => line_array(array, "unexpected_lines", config),
        None => Ok(Vec::new()),
    }
}

/// Known-answer check shared by every frontend: no line listed in `unexpected` may
/// appear among the lines a flow reached.
///
/// Returns the failure message naming the violated lines, or `None` when the
/// constraint holds. Callers supply whatever set of source lines they derived, so
/// this is independent of how a frontend maps its output back to source.
pub fn check_unexpected_lines(unexpected: &[i64], found: &BTreeSet<i64>) -> Option<String> {
    let violated: Vec<i64> = unexpected
        .iter()
        .copied()
        .filter(|line| found.contains(line))
        .collect();
    if violated.is_empty() {
        None
    } else {
        Some(format!(
            "unexpected lines {violated:?} carry a flow (reached lines {found:?})"
        ))
    }
}

/// Parse an array of source line numbers from a test config.
fn line_array(array: &Value, key: &str, config: &Path) -> Result<Vec<i64>> {
    let array = array
        .as_array()
        .with_context(|| format!("`{key}` is not an array in {}", config.display()))?;
    array
        .iter()
        .map(|v| {
            v.as_i64()
                .with_context(|| format!("non-integer in `{key}` of {}", config.display()))
        })
        .collect()
}

/// Collect every `byteOffset` value anywhere in a SARIF document.
///
/// The old DEX script grepped the raw file for `"byteOffset": N`, so we likewise
/// gather the key wherever it appears rather than walking a fixed path.
pub fn collect_byte_offsets(sarif: &Path) -> Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    collect_int_values(&read_json(sarif)?, "byteOffset", &mut out);
    Ok(out)
}

/// Collect every `absoluteAddress` value in a SARIF document (the pcode tests'
/// tainted-instruction addresses).
pub fn collect_absolute_addresses(sarif: &Path) -> Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    collect_int_values(&read_json(sarif)?, "absoluteAddress", &mut out);
    Ok(out)
}

/// Collect every `relativeAddress` value in a SARIF document: the
/// tainted-instruction addresses with the disassembler's image base already
/// subtracted, i.e. the section-relative offsets `addr2line` expects. This is
/// robust to whatever base Ghidra chose for the artifact.
pub fn collect_relative_addresses(sarif: &Path) -> Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    collect_int_values(&read_json(sarif)?, "relativeAddress", &mut out);
    Ok(out)
}

fn collect_int_values(value: &Value, key: &str, out: &mut BTreeSet<i64>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    if let Some(n) = v.as_i64() {
                        out.insert(n);
                    }
                }
                collect_int_values(v, key, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_int_values(v, key, out);
            }
        }
        _ => {}
    }
}

/// One entry of a `dex-reader --linemap-json` file.
#[derive(Deserialize)]
pub struct LineEntry {
    pub dex_offset: i64,
    pub line: i64,
}

pub fn load_linemap(path: &Path) -> Result<Vec<LineEntry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read linemap {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse linemap {}", path.display()))
}

/// Map a dex byte offset to a source line: the line of the entry with the
/// greatest `dex_offset` that is still `<= offset`.
pub fn map_offset_to_line(linemap: &[LineEntry], offset: i64) -> Option<i64> {
    linemap
        .iter()
        .filter(|e| e.dex_offset <= offset)
        .max_by_key(|e| e.dex_offset)
        .map(|e| e.line)
}

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse JSON {}", path.display()))
}
