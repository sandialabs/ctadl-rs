//! What `ctadl query` writes on each step of a code flow: `taintVertex` on every step, and
//! `callee` or `summarized` on every step but a source or sink step.
//!
//! Each test queries a `.tnt` fixture under `tests/tnt` in the `human` profile and reads the
//! steps of its flows. A `.tnt` `source(...)` statement is anchored at no call, so it gives no
//! source step; the endpoint steps these tests see are sinks'.

use std::path::PathBuf;

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use serde_json::Value;

use crate::common::{step_properties_problem, store_dir};

/// The SARIF `ctadl query` writes in `profile` for the fixture `tests/tnt/<fixture>`, imported
/// and indexed under names made from `name`, which no other test in this binary uses.
fn query_fixture(fixture: &str, name: &str, profile: SarifProfile) -> Value {
    store_dir();
    let tnt: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "tnt", fixture]
        .iter()
        .collect();
    let import_name = format!("step_props_{name}");
    let import = ArtifactImport::try_create(&import_name, ArtifactLanguage::Flowy, &tnt).unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();
    let project = AnalysisProject::try_create(
        &format!("step_props_{name}_proj"),
        &[import_name.as_str()],
        SubImports::All,
    )
    .unwrap();
    cli::index(
        &project,
        &[],
        &[],
        false,
        cli::IndexOptions {
            strategy: CallResolutionStrategy::default(),
            ..Default::default()
        },
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    let sarif = out.path().join("out.sarif");
    crate::common::query(&project, &[], &sarif, profile, None).unwrap();
    serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap()
}

/// The `C0001` path results of `doc`.
fn path_results(doc: &Value) -> Vec<&Value> {
    doc["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["ruleId"]
                .as_str()
                .is_some_and(|id| id.starts_with("C0001"))
        })
        .collect()
}

/// The steps of a path result's one code flow.
fn steps(result: &Value) -> &Vec<Value> {
    let flows = result["codeFlows"].as_array().unwrap();
    assert_eq!(flows.len(), 1, "a path result has one code flow: {result}");
    flows[0]["threadFlows"][0]["locations"].as_array().unwrap()
}

/// A path result's `sinkFunctions`.
fn sink_functions(result: &Value) -> Vec<&str> {
    result["properties"]["sinkFunctions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect()
}

/// The one path result whose `sinkFunctions` is `functions`.
fn result_with_sinks<'a>(doc: &'a Value, functions: &[&str]) -> &'a Value {
    let found: Vec<_> = path_results(doc)
        .into_iter()
        .filter(|r| sink_functions(r) == functions)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected one path result with sinkFunctions {functions:?}: {doc}"
    );
    found[0]
}

fn message(step: &Value) -> &str {
    step["location"]["message"]["text"].as_str().unwrap()
}

fn line(step: &Value) -> u64 {
    step["location"]["physicalLocation"]["region"]["startLine"]
        .as_u64()
        .unwrap()
}

fn props(step: &Value) -> &serde_json::Map<String, Value> {
    step["properties"].as_object().unwrap()
}

/// The indirect call in `main` (`r = f(im, h)`) resolves to `get_a` and `get_b`; the flow into
/// each target's sink enters one of them there. Its step names that one, in `callee` and in
/// its message.
#[test]
fn call_step_names_the_function_entered() {
    let doc = query_fixture(
        "funcptr_two_targets.tnt",
        "funcptr_two_targets",
        SarifProfile::Human,
    );
    for (sink, entered) in [("sink_a", "get_a"), ("sink_b", "get_b")] {
        let result = result_with_sinks(&doc, &[sink]);
        let at_call: Vec<_> = steps(result).iter().filter(|s| line(s) == 45).collect();
        assert_eq!(at_call.len(), 1, "one step at the indirect call: {result}");
        let step = at_call[0];
        assert_eq!(props(step)["callee"], entered, "{step}");
        assert!(!props(step).contains_key("summarized"), "{step}");
        assert!(message(step).ends_with(&format!(" in {entered}")), "{step}");
    }
}

/// `src` returns a source-tainted value to `main` at each of two calls. The step at each call is
/// the return's: `callee` is `src`, the function returned from, and the message names `main`,
/// the one returned into.
#[test]
fn return_step_names_the_function_returned_from() {
    let doc = query_fixture("callsite_paths.tnt", "callsite_paths", SarifProfile::Human);
    let results = path_results(&doc);
    assert_eq!(results.len(), 2, "{doc}");
    let mut lines = Vec::new();
    for result in results {
        let returns: Vec<_> = steps(result)
            .iter()
            .filter(|s| s["kinds"] == serde_json::json!(["return"]))
            .collect();
        assert_eq!(returns.len(), 1, "one return step: {result}");
        let step = returns[0];
        assert_eq!(props(step)["callee"], "src", "{step}");
        assert!(!props(step).contains_key("summarized"), "{step}");
        assert!(message(step).ends_with(" in main"), "{step}");
        lines.push(line(step));
    }
    lines.sort_unstable();
    assert_eq!(lines, [35, 37]);
}

/// `A(x, y)` carries `x.bar` to `y.foo` by its summary, and the flow does not enter `A`. Its one
/// step, at that call, is summarized and names no callee.
#[test]
fn summarized_step_between_arguments() {
    let doc = query_fixture("ex4.tnt", "ex4", SarifProfile::Human);
    let results = path_results(&doc);
    assert_eq!(results.len(), 1, "{doc}");
    let steps = steps(results[0]);
    assert_eq!(steps.len(), 1, "one step, at the call to A: {doc}");
    let step = &steps[0];
    assert_eq!(line(step), 4, "{step}");
    assert_eq!(
        props(step).get("summarized"),
        Some(&Value::Bool(true)),
        "{step}"
    );
    assert!(!props(step).contains_key("callee"), "{step}");
    assert!(message(step).ends_with(" in A"), "{step}");
}

/// `put(x)` writes its argument to the global `g`, which the caller reads after the call. The
/// flow crosses the call into the global without entering `put`, so its step there is
/// summarized and names no callee.
#[test]
fn summarized_step_into_global() {
    let doc = query_fixture("step_props_global.tnt", "global", SarifProfile::Human);
    let result = result_with_sinks(&doc, &["snk"]);
    let at_call: Vec<_> = steps(result).iter().filter(|s| line(s) == 22).collect();
    assert_eq!(at_call.len(), 1, "one step at the call to put: {result}");
    let step = at_call[0];
    assert_eq!(
        props(step).get("summarized"),
        Some(&Value::Bool(true)),
        "{step}"
    );
    assert!(!props(step).contains_key("callee"), "{step}");
    assert!(message(step).ends_with(" in put"), "{step}");
}

/// The sink on the formal `h` of both `get_a` and `get_b` is anchored at the indirect call that
/// resolves to the two. Its step has `taintVertex` only, its message names the callee first by
/// name, and its result lists both callees. The flows into `sink_a` and `sink_b` still enter one
/// target each.
#[test]
fn endpoint_step_at_two_callees() {
    let doc = query_fixture(
        "step_props_two_callees.tnt",
        "two_callees",
        SarifProfile::Human,
    );
    let result = result_with_sinks(&doc, &["get_a", "get_b"]);
    let sink = steps(result).last().unwrap();
    assert!(message(sink).starts_with("sink "), "{sink}");
    assert!(message(sink).ends_with(" in get_a"), "{sink}");
    assert_eq!(
        props(sink).keys().collect::<Vec<_>>(),
        ["taintVertex"],
        "{sink}"
    );
    for (sink, entered) in [("sink_a", "get_a"), ("sink_b", "get_b")] {
        let result = result_with_sinks(&doc, &[sink]);
        assert!(
            steps(result)
                .iter()
                .any(|s| props(s).get("callee") == Some(&Value::from(entered))),
            "the flow into {sink} enters {entered}: {result}"
        );
    }
}

/// The `machine` profile writes no code flows, so no steps.
#[test]
fn machine_profile_has_no_steps() {
    let doc = query_fixture(
        "step_props_two_callees.tnt",
        "two_callees_machine",
        SarifProfile::Machine,
    );
    let results = doc["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{doc}");
    assert!(
        results.iter().all(|r| r.get("codeFlows").is_none()),
        "{doc}"
    );
}

/// The check every query in this binary runs reports a step that says the wrong thing, and
/// nothing on a file as `ctadl query` wrote it.
#[test]
fn check_rejects_a_bad_step() {
    let doc = query_fixture(
        "step_props_two_callees.tnt",
        "two_callees_check",
        SarifProfile::Human,
    );
    assert_eq!(step_properties_problem(&doc), None);

    // The first step with `callee`, and the first sink step, as JSON pointers.
    let mut call_step = None;
    let mut sink_step = None;
    for (n, result) in doc["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let Some(steps) = result.pointer("/codeFlows/0/threadFlows/0/locations") else {
            continue;
        };
        for (s, step) in steps.as_array().unwrap().iter().enumerate() {
            let at = format!("/runs/0/results/{n}/codeFlows/0/threadFlows/0/locations/{s}");
            if call_step.is_none() && props(step).contains_key("callee") {
                call_step = Some(at.clone());
            }
            if sink_step.is_none() && message(step).starts_with("sink ") {
                sink_step = Some(at);
            }
        }
    }
    let (call_step, sink_step) = (call_step.unwrap(), sink_step.unwrap());

    let edits: [(&str, Box<dyn Fn(&mut Value)>); 4] = [
        (
            "a call step given summarized too",
            Box::new(|d: &mut Value| {
                d.pointer_mut(&format!("{call_step}/properties"))
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("summarized".into(), Value::Bool(true));
            }),
        ),
        (
            "a call step with its callee removed",
            Box::new(|d: &mut Value| {
                d.pointer_mut(&format!("{call_step}/properties"))
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("callee");
            }),
        ),
        (
            "a sink step given a callee",
            Box::new(|d: &mut Value| {
                d.pointer_mut(&format!("{sink_step}/properties"))
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("callee".into(), Value::from("get_a"));
            }),
        ),
        (
            "a step whose taintVertex is changed",
            Box::new(|d: &mut Value| {
                *d.pointer_mut(&format!("{call_step}/properties/taintVertex"))
                    .unwrap() = Value::from("call-arg(0, 0)");
            }),
        ),
    ];
    for (what, edit) in edits {
        let mut edited = doc.clone();
        edit(&mut edited);
        assert_ne!(edited, doc, "{what}: the edit changed nothing");
        assert!(
            step_properties_problem(&edited).is_some(),
            "{what}: not reported"
        );
    }
}
