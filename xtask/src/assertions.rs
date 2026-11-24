//! Parsing of analyzer outputs and known-answer checking.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// Read the `expected_lines` array from a test config. An empty array denotes a
/// negative test (no flow expected). A missing key is an error.
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

/// The Android ABI a JNI case's library is built for: the `native_abi` key, `arm64-v8a` when
/// absent. `x86` selects the 32-bit x86 toolchain.
pub fn read_native_abi(config: &Path) -> Result<String> {
    let value = read_config(config)?;
    match value.get("native_abi") {
        None => Ok("arm64-v8a".to_string()),
        Some(abi) => abi
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("`native_abi` in {} is not a string", config.display())),
    }
}

/// The native slot layout a JNI case's library must get when its debug info is stripped: the
/// `untyped_native_layout` key. Present only on the cases that also run stripped.
pub fn read_untyped_native_layout(config: &Path) -> Result<Option<String>> {
    let value = read_config(config)?;
    match value.get("untyped_native_layout") {
        None => Ok(None),
        Some(layout) => layout
            .as_str()
            .map(|s| Some(s.to_string()))
            .with_context(|| {
                format!(
                    "`untyped_native_layout` in {} is not a string",
                    config.display()
                )
            }),
    }
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

/// Collect every `byteOffset` value anywhere in a SARIF document's results. Meant for
/// the machine profile, whose results are the tainted *instructions* (there are no code
/// flows to scope to).
///
/// Endpoint results (`C0003.taint-source` / `C0004.taint-sink`) are excluded. They are
/// emitted in every profile, and their locations are the *declared* source and sink lines
/// rather than lines a flow reached -- counting them would make a sink's own line satisfy
/// `expected_lines`, and would let it trip `unexpected_lines` for a case whose point is
/// that a modeled endpoint stays clean.
pub fn collect_byte_offsets(sarif: &Path) -> Result<BTreeSet<i64>> {
    collect_non_endpoint_ints(sarif, "byteOffset")
}

/// Every integer `key` in a SARIF document's results other than the endpoint results, for
/// the reason [`collect_byte_offsets`] gives.
fn collect_non_endpoint_ints(sarif: &Path, key: &str) -> Result<BTreeSet<i64>> {
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
            collect_int_values(result, key, &mut out);
        }
    }
    Ok(out)
}

/// Whether some human-profile code flow connects a source to a sink: a single thread
/// flow that carries both a `source ...` step and a `sink ...` step (the step messages
/// the formatter emits for the two endpoints). This is the code-flow *integrity* check,
/// distinct from `expected_lines` coverage: a flow collapsed to its lone source step has
/// no sink step, so no thread connects the two.
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

/// Known-answer check of every code-flow step of a SARIF document: what is wrong with the
/// first step that does not say what it should, naming its result and its place in the flow,
/// or `None` when every step does.
///
/// A step has a string `properties.taintVertex`, and its message is
/// `<word> <taintVertex> in <function>`. A source or sink step, told by its message's first
/// word as [`codeflow_connects_source_and_sink`] tells them, has neither `callee` nor
/// `summarized`; every other step has exactly one: `callee`, a string, or `summarized`,
/// `true`.
pub fn check_step_properties(sarif: &Path) -> Result<Option<String>> {
    Ok(first_bad_step(&read_json(sarif)?))
}

/// [`check_step_properties`] on a parsed document.
fn first_bad_step(doc: &Value) -> Option<String> {
    let all = |v: &Value, key: &str| -> Vec<Value> {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for (r, run) in all(doc, "runs").iter().enumerate() {
        for (n, result) in all(run, "results").iter().enumerate() {
            let rule = result.get("ruleId").and_then(Value::as_str).unwrap_or("");
            for (c, flow) in all(result, "codeFlows").iter().enumerate() {
                for (t, thread) in all(flow, "threadFlows").iter().enumerate() {
                    for (s, step) in all(thread, "locations").iter().enumerate() {
                        let message = step
                            .pointer("/location/message/text")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(problem) = step_problem(step, message) {
                            return Some(format!(
                                "run {r}, result {n} ({rule}), code flow {c}, thread flow {t}, \
                                 step {s} ({message:?}): {problem}"
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

/// What is wrong with one step whose message is `message`, or `None`.
fn step_problem(step: &Value, message: &str) -> Option<String> {
    let props = step.get("properties");
    let prop = |key: &str| props.and_then(|p| p.get(key));
    let Some(vertex) = prop("taintVertex").and_then(Value::as_str) else {
        return Some("no string taintVertex".to_string());
    };
    let Some((word, rest)) = message.split_once(' ') else {
        return Some("the message has no vertex".to_string());
    };
    if !rest
        .strip_prefix(vertex)
        .is_some_and(|tail| tail.starts_with(" in "))
    {
        return Some(format!(
            "the message does not name taintVertex {vertex:?} as `{word} {vertex} in ...`"
        ));
    }
    let callee = prop("callee");
    let summarized = prop("summarized");
    if matches!(word, "source" | "sink") {
        if callee.is_some() || summarized.is_some() {
            return Some(format!("a {word} step has callee or summarized"));
        }
        return None;
    }
    match (callee, summarized) {
        (Some(Value::String(_)), None) | (None, Some(Value::Bool(true))) => None,
        (Some(_), Some(_)) => Some("both callee and summarized".to_string()),
        (None, None) => Some("neither callee nor summarized".to_string()),
        (Some(c), None) => Some(format!("callee {c} is not a string")),
        (None, Some(s)) => Some(format!("summarized {s} is not true")),
    }
}

/// The functions whose sinks a connected code flow ends in: for every thread flow that
/// has a source step, the function the last sink step names
/// (`sink call-arg(22, 0) in <function>`).
pub fn collect_codeflow_sink_functions(sarif: &Path) -> Result<BTreeSet<String>> {
    let value = read_json(sarif)?;
    let mut found = BTreeSet::new();
    let all = |v: Option<&Value>| -> Vec<Value> {
        v.and_then(Value::as_array).cloned().unwrap_or_default()
    };
    for run in all(value.get("runs")) {
        for result in all(run.get("results")) {
            for flow in all(result.get("codeFlows")) {
                for thread in all(flow.get("threadFlows")) {
                    let msgs: Vec<String> = all(thread.get("locations"))
                        .iter()
                        .filter_map(|loc| {
                            loc.get("location")
                                .and_then(|l| l.get("message"))
                                .and_then(|m| m.get("text"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect();
                    if !msgs.iter().any(|m| m.starts_with("source ")) {
                        continue;
                    }
                    if let Some(sink) = msgs.iter().rev().find(|m| m.starts_with("sink "))
                        && let Some((_, function)) = sink.split_once(" in ")
                    {
                        found.insert(function.to_string());
                    }
                }
            }
        }
    }
    Ok(found)
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

/// Collect every `startLine` reached by a code-flow step, i.e. the lines under
/// `runs[].results[].codeFlows[].threadFlows[].locations[]`.
///
/// Source-level frontends (Lua and the tree-sitter languages, C among them) emit
/// UTF-8 regions whose location is a `region.startLine`, not a `byteOffset`, so a
/// source case's `expected_lines` are checked directly against these rather than
/// through a linemap -- no compiler or `addr2line` in the loop. Scoped to code
/// flows for the same reason as [`collect_codeflow_byte_offsets`]: it stays
/// sensitive to the flow actually being traced.
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

/// Collect every `absoluteAddress` value in a SARIF document's results but the endpoint
/// results (the pcode tests' tainted-instruction addresses).
pub fn collect_absolute_addresses(sarif: &Path) -> Result<BTreeSet<i64>> {
    collect_non_endpoint_ints(sarif, "absoluteAddress")
}

/// Collect every `relativeAddress` value in a SARIF document's results but the endpoint
/// results: the tainted-instruction addresses with the disassembler's image base already
/// subtracted, i.e. the section-relative offsets `addr2line` expects. This is robust to
/// whatever base Ghidra chose for the artifact.
pub fn collect_relative_addresses(sarif: &Path) -> Result<BTreeSet<i64>> {
    collect_non_endpoint_ints(sarif, "relativeAddress")
}

fn collect_int_values(value: &Value, key: &str, out: &mut BTreeSet<i64>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key
                    && let Some(n) = v.as_i64()
                {
                    out.insert(n);
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

#[cfg(test)]
mod tests {
    use super::first_bad_step;
    use serde_json::{Value, json};

    /// One flow: a call step, a summarized step and a sink step, each as `ctadl query` writes
    /// it.
    fn flow() -> Value {
        let step = |message: &str, props: Value| json!({ "location": { "message": { "text": message } }, "properties": props });
        json!({ "runs": [{ "results": [{
            "ruleId": "C0001.tainted-path",
            "codeFlows": [{ "threadFlows": [{ "locations": [
                step("call call-arg(3, 1) in get_a",
                     json!({ "taintVertex": "call-arg(3, 1)", "callee": "get_a" })),
                step("call call-arg(5, 0).bar in A",
                     json!({ "taintVertex": "call-arg(5, 0).bar", "summarized": true })),
                step("sink call-arg(7, 0) in sink_a",
                     json!({ "taintVertex": "call-arg(7, 0)" })),
            ]}]}],
        }]}]})
    }

    fn props(doc: &mut Value, step: usize) -> &mut serde_json::Map<String, Value> {
        doc.pointer_mut(&format!(
            "/runs/0/results/0/codeFlows/0/threadFlows/0/locations/{step}/properties"
        ))
        .unwrap()
        .as_object_mut()
        .unwrap()
    }

    #[test]
    fn check_step_properties_accepts_what_query_writes() {
        assert_eq!(first_bad_step(&flow()), None);
    }

    #[test]
    fn check_step_properties_rejects_a_bad_step() {
        let edits: [(&str, fn(&mut Value)); 4] = [
            ("a call step given summarized too", |d| {
                props(d, 0).insert("summarized".into(), Value::Bool(true));
            }),
            ("a call step with its callee removed", |d| {
                props(d, 0).remove("callee");
            }),
            ("a sink step given a callee", |d| {
                props(d, 2).insert("callee".into(), json!("sink_a"));
            }),
            ("a step whose taintVertex is changed", |d| {
                props(d, 1).insert("taintVertex".into(), json!("call-arg(5, 0).baz"));
            }),
        ];
        for (what, edit) in edits {
            let mut doc = flow();
            edit(&mut doc);
            assert_ne!(doc, flow(), "{what}: the edit changed nothing");
            let problem = first_bad_step(&doc);
            assert!(problem.is_some(), "{what}: not reported");
        }
    }
}
