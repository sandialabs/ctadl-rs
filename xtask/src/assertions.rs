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
    let value = read_config(config)?;
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
    read_optional_lines(config, "unexpected_lines")
}

/// Read the optional `expected_native_lines` array from a JNI case config: lines of the
/// case's *C* source that the flow must reach on the far side of the JNI boundary.
///
/// Separate from `expected_lines`, which for these cases names lines of the Java source.
/// The two halves are different files mapped back by different means -- Java through the dex
/// linemap, native through `addr2line` over the shared library -- so they cannot share a key.
/// A missing key means the case claims nothing about the native side.
pub fn read_expected_native_lines(config: &Path) -> Result<Vec<i64>> {
    read_optional_lines(config, "expected_native_lines")
}

/// Read an optional array of source line numbers from a test config, treating a missing key
/// as an empty claim.
fn read_optional_lines(config: &Path, key: &str) -> Result<Vec<i64>> {
    let value = read_config(config)?;
    match value.get(key) {
        Some(array) => line_array(array, key, config),
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

/// Collect every `byteOffset` value anywhere in a SARIF document. Used for the
/// machine profile, whose results are the tainted *instructions* (there are no code
/// flows to scope to); the union of these with the human profile's code-flow offsets
/// is what a DEX/JVM case's `expected_lines` must be covered by.
///
/// Endpoint results (`C0003.taint-source` / `C0004.taint-sink`) are excluded. They are
/// emitted in every profile, and their locations are the *declared* source and sink lines
/// rather than lines a flow reached -- counting them would make a sink's own line satisfy
/// `expected_lines`, and would let it trip `unexpected_lines` for a case whose point is
/// that a modeled endpoint stays clean.
pub fn collect_byte_offsets(sarif: &Path) -> Result<BTreeSet<i64>> {
    let value = read_json(sarif)?;
    let mut out = BTreeSet::new();
    for run in value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for result in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let rule_id = result.get("ruleId").and_then(Value::as_str).unwrap_or("");
            if matches!(rule_id, "C0003.taint-source" | "C0004.taint-sink") {
                continue;
            }
            collect_int_values(result, "byteOffset", &mut out);
        }
    }
    Ok(out)
}

/// Whether some human-profile code flow connects a source to a sink: a single thread
/// flow that carries both a `source ...` step and a `sink ...` step (the step messages
/// the formatter emits for the two endpoints). This is the code-flow *integrity* check
/// -- distinct from `expected_lines` coverage -- and is exactly what regressed when the
/// step dedup collapsed every flow to its lone source step: with only a source step and
/// no sink step, no thread connects the two.
pub fn codeflow_connects_source_and_sink(sarif: &Path) -> Result<bool> {
    let value = read_json(sarif)?;
    for run in value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for result in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for flow in result
                .get("codeFlows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for thread in flow
                    .get("threadFlows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let mut saw_source = false;
                    let mut saw_sink = false;
                    for loc in thread
                        .get("locations")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let msg = loc
                            .get("location")
                            .and_then(|l| l.get("message"))
                            .and_then(|m| m.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if msg.starts_with("source ") {
                            saw_source = true;
                        } else if msg.starts_with("sink ") {
                            saw_sink = true;
                        }
                    }
                    if saw_source && saw_sink {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Collect every `byteOffset` reached by a code-flow step, i.e. the offsets under
/// `runs[].results[].codeFlows[].threadFlows[].locations[]`.
///
/// Code flows are the tool's primary product -- the traced source -> ... -> sink
/// path -- so the DEX/JVM known-answer check reads its offsets from there rather than
/// from anywhere in the document. Scoping to code flows makes the check sensitive to
/// the flow actually being traced: a collapsed flow that kept only its source step
/// drops the intermediate and sink offsets here, whereas endpoint/result-level
/// locations (which are emitted regardless of whether the flow was traced) would
/// still carry them.
pub fn collect_codeflow_byte_offsets(sarif: &Path) -> Result<BTreeSet<i64>> {
    let value = read_json(sarif)?;
    let mut out = BTreeSet::new();
    for run in value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for result in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for flow in result
                .get("codeFlows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                // Under codeFlows the only `byteOffset` keys are a step's physical
                // location, so a recursive gather here stays scoped to steps.
                collect_int_values(flow, "byteOffset", &mut out);
            }
        }
    }
    Ok(out)
}

/// Collect every source line (`region.startLine`) reached by a code-flow step,
/// i.e. the start lines under
/// `runs[].results[].codeFlows[].threadFlows[].locations[]`.
///
/// This is the tree-sitter C frontend's analogue of
/// [`collect_codeflow_byte_offsets`]: because that frontend parses the source
/// directly, its SARIF carries real source spans, so a case's `expected_lines`
/// are matched against these start lines with no compiler or `addr2line` in the
/// loop. Scoping to code-flow steps (rather than any `startLine` in the
/// document) keeps the check sensitive to the flow actually being traced,
/// exactly as the byte-offset variant does.
pub fn collect_codeflow_source_lines(sarif: &Path) -> Result<BTreeSet<i64>> {
    let value = read_json(sarif)?;
    let mut out = BTreeSet::new();
    for run in value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for result in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for flow in result
                .get("codeFlows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                // Under a code-flow step the only `startLine` is the step's
                // physical-location region, so a recursive gather here stays
                // scoped to steps.
                collect_int_values(flow, "startLine", &mut out);
            }
        }
    }
    Ok(out)
}

/// Collect every `startLine` reached by a code-flow step, i.e. the lines under
/// `runs[].results[].codeFlows[].threadFlows[].locations[]`.
///
/// Source-level frontends (Lua and the other tree-sitter languages) emit UTF-8
/// regions whose location is a `region.startLine`, not a `byteOffset`, so a
/// source case's `expected_lines` are checked directly against these rather than
/// through a linemap. Scoped to code flows for the same reason as
/// [`collect_codeflow_byte_offsets`]: it stays sensitive to the flow actually
/// being traced.
pub fn collect_codeflow_start_lines(sarif: &Path) -> Result<BTreeSet<i64>> {
    let value = read_json(sarif)?;
    let mut out = BTreeSet::new();
    for run in value
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for result in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for flow in result
                .get("codeFlows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                collect_int_values(flow, "startLine", &mut out);
            }
        }
    }
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

/// Read a test config, which may be JSON or JSON5 (JSON5 is a superset, so this
/// parses both). Kept separate from [`read_json`] so the hot SARIF/linemap parsing
/// stays on `serde_json`; only the small, comment-carrying configs pay for JSON5.
fn read_config(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    json5::from_str(&text).with_context(|| format!("failed to parse config {}", path.display()))
}
