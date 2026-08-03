//! A declarative bridge, end to end, across two imports.
//!
//! Lua is the frontend for the same reason `multi_import_sarif.rs` uses it: it is a real
//! frontend that needs no external toolchain, so this runs everywhere `cargo test` does. It also
//! happens to be the one frontend that publishes an **externals** column -- the set of called
//! names minus the names the import defined -- which is what gives the bridge a *bodyless*
//! callee to attach to, exactly as a Dex `native` method does. Matching runs against the VMT
//! rather than against `FunctionData`, and this is the case that proves it.
//!
//! The two artifacts give their functions **deliberately different names**. Functions are
//! interned by name, so two same-named halves already share a node; a flow between those would
//! prove nothing about bridging. The negative case pins the other half: without the model, the
//! same two artifacts produce no flow at all.
//!
//! The two-real-frontend counterpart is `cargo xtask regression --frontend jni`, whose
//! `bridge-model` case runs the same join declaratively under `--no-jni-bridge`, as a direct A/B
//! against the built-in pass.

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Once;

static INIT: Once = Once::new();

fn store_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp store"));
    INIT.call_once(|| {
        init_store_path(Some(dir.path())).expect("init store");
    });
    dir.path()
}

/// The app. `alpha_stub` is called and never defined here, so it is an *external* of this
/// import: a matchable, bodyless callee with no `FunctionData` at all. Nothing in this artifact
/// mentions the implementation, and taint has to leave through `alpha_stub` and come back for
/// the sink to fire.
const APP: &str = r#"-- app.lua
local function app_source()
  return io.read()
end

local function app_sink(x)
  print(x)
end

local function main_app()
  local t = app_source()
  local u = alpha_stub(t)
  app_sink(u)
end

main_app()
"#;

/// The library. `beta_impl` is the identity, and shares no name with anything in the app.
const LIB: &str = r#"-- lib.lua
local function beta_impl(x)
  return x
end

return beta_impl
"#;

/// A library whose callee takes its argument off a sub-path of its first parameter rather than
/// positionally -- the shape a Lua-to-C boundary has, where everything arrives on an
/// interpreter stack. Its behaviour is modelled by hand, at exactly the port map's path; see
/// `a_pathful_bridge_composes_at_an_exactly_matching_path`.
const LIB_PATHFUL: &str = r#"-- lib_pathful.lua
local function gamma_impl(s)
  return s
end

return gamma_impl
"#;

/// Sources and sinks live in the app and are named there, so the query needs no knowledge of
/// the boundary.
const QUERY_MODEL: &str = r#"{"model_generators":[
{"find":"methods","where":[{"constraint":"signature_match","names":["app_source"]}],
 "model":{"sources":[{"port":"Return","kind":"UserInput"}]}},
{"find":"methods","where":[{"constraint":"signature_match","names":["app_sink"]}],
 "model":{"sinks":[{"port":"Argument(0)","kind":"UserInput"}]}}
]}"#;

/// Imports `(app, lib)` under `case`-prefixed names and returns those names.
fn import_pair(case: &str, lib_text: &str) -> (String, String) {
    let dir = store_dir();
    let mut names = Vec::new();
    for (half, text) in [("app", APP), ("lib", lib_text)] {
        let name = format!("{case}_{half}");
        let src = dir.join(format!("{name}.lua"));
        std::fs::write(&src, text).expect("writing lua source");
        let import =
            ArtifactImport::try_create(&name, ArtifactLanguage::Lua, &src).expect("import args");
        cli::import(&import, cli::ImportOptions::default()).expect("importing lua");
        names.push(name);
    }
    (names[0].clone(), names[1].clone())
}

/// Indexes the pair with `models`, queries it, and returns how many complete source-to-sink
/// flows the SARIF reports.
fn flows_found(case: &str, lib_text: &str, models: &[PathBuf]) -> usize {
    let dir = store_dir();
    let (app, lib) = import_pair(case, lib_text);
    let project = AnalysisProject::try_create(case, &[app, lib]).expect("project");
    cli::index(
        &project,
        &[],
        models,
        false,
        false,
        CallResolutionStrategy::Mixed,
        true,
        true,
        None,
    )
    .expect("indexing");

    let query_models = dir.join(format!("{case}-query.json"));
    std::fs::write(&query_models, QUERY_MODEL).expect("writing query model");
    let sarif: PathBuf = dir.join(format!("{case}.sarif"));
    cli::query(
        &project,
        &[query_models],
        false,
        &sarif,
        SarifProfile::Human,
        None,
    )
    .expect("querying");

    let text = std::fs::read_to_string(&sarif).expect("reading sarif");
    let sarif: Value = serde_json::from_str(&text).expect("parsing sarif");
    sarif["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|run| run["results"].as_array())
        .flatten()
        .filter(|r| r["ruleId"] == "C0001.tainted-path" && r["kind"] == "fail")
        .count()
}

/// Writes a model file whose generators are `generators`, one per line.
fn model_file(case: &str, generators: &[serde_json::Value]) -> PathBuf {
    let path = store_dir().join(format!("{case}-models.jsonl"));
    let text: String = generators
        .iter()
        .map(|g| format!("{}\n", serde_json::to_string(g).unwrap()))
        .collect();
    std::fs::write(&path, text).expect("writing model file");
    path
}

/// A bridge joining `app`'s `alpha_stub` to `lib`'s `beta_impl`, scoped by import name so the
/// two sides cannot be confused for each other.
fn plain_bridge(app: &str, lib: &str) -> serde_json::Value {
    serde_json::json!({
        "find": "methods",
        "in": {"import": app},
        "where": [{"constraint": "signature_match", "name": "alpha_stub"}],
        "model": {"bridge": {
            "to": {"in": {"import": lib},
                   "where": [{"constraint": "signature_match", "name": "beta_impl"}]},
            "arguments": [
                {"from": "Argument(0)", "to": "Argument(0)"},
                {"from": "Return", "to": "Return"}
            ]
        }}
    })
}

/// The positive case. Taint leaves the app through a callee with no body at all, crosses into
/// the library, and comes back.
#[test]
fn a_declarative_bridge_carries_taint_across_two_imports() {
    let case = "bridge_e2e_positive";
    let model = model_file(
        case,
        &[plain_bridge(&format!("{case}_app"), &format!("{case}_lib"))],
    );
    assert_eq!(
        flows_found(case, LIB, std::slice::from_ref(&model)),
        1,
        "the bridge should complete app_source -> alpha_stub -> beta_impl -> app_sink"
    );
}

/// The negative case: the same two artifacts, the same query, no model. Nothing joins the two
/// halves, so there is no flow. Without this, the positive case cannot distinguish "the bridge
/// worked" from "these two artifacts were already connected".
#[test]
fn without_the_model_nothing_joins_the_two_halves() {
    assert_eq!(
        flows_found("bridge_e2e_negative", LIB, &[]),
        0,
        "alpha_stub is an undefined external, so on its own it carries nothing"
    );
}

/// A bridge whose callee-side port names a **sub-path** of the callee's parameter -- the shape
/// no shipped code exercised, since every JNI port is an empty-path formal.
///
/// It comes with a precondition, and the fixture states it: composition past the seam is
/// exact-match only, so the callee's behaviour must *also* be modelled, in the port map's
/// vocabulary, at exactly the port map's path. Here the bridge delivers taint to
/// `Argument(0).stack` and the callee's hand-written summary reads it from exactly there. A
/// summary one level deeper or shallower would produce a residue path that is in neither the
/// program-path nor the model-path bucket, and the flow would drop silently.
///
/// Note the callee model maps `Argument(0).stack` to `Return` rather than to another sub-path
/// of `Argument(0)`: summary codegen skips a port pair whose two *indices* are equal, so a
/// same-index, different-path propagation cannot be expressed at all.
#[test]
fn a_pathful_bridge_composes_at_an_exactly_matching_path() {
    let case = "bridge_e2e_pathful";
    let app = format!("{case}_app");
    let lib = format!("{case}_lib");
    let model = model_file(
        case,
        &[
            serde_json::json!({
                "find": "methods",
                "in": {"import": app},
                "where": [{"constraint": "signature_match", "name": "alpha_stub"}],
                "model": {"bridge": {
                    "to": {"in": {"import": lib},
                           "where": [{"constraint": "signature_match", "name": "gamma_impl"}]},
                    "arguments": [
                        {"from": "Argument(0)", "to": "Argument(0).stack", "direction": "in"},
                        {"from": "Return", "to": "Return", "direction": "out"}
                    ]
                }}
            }),
            serde_json::json!({
                "find": "methods",
                "in": {"import": lib},
                "where": [{"constraint": "signature_match", "name": "gamma_impl"}],
                "model": {"propagation": [{"input": "Argument(0).stack", "output": "Return"}]}
            }),
        ],
    );
    assert!(
        flows_found(case, LIB_PATHFUL, std::slice::from_ref(&model)) >= 1,
        "a callee-side sub-path composes when the callee's summary lands on exactly that path"
    );
}

/// The negative half of the pathful case: keep the callee's model, drop the bridge. The two
/// artifacts are then unrelated again and no flow completes, so the previous test cannot be
/// passing on the strength of the callee model alone.
#[test]
fn the_pathful_case_needs_the_bridge_not_just_the_callee_model() {
    let case = "bridge_e2e_pathful_nobridge";
    let lib = format!("{case}_lib");
    let model = model_file(
        case,
        &[serde_json::json!({
            "find": "methods",
            "in": {"import": lib},
            "where": [{"constraint": "signature_match", "name": "gamma_impl"}],
            "model": {"propagation": [{"input": "Argument(0).stack", "output": "Return"}]}
        })],
    );
    assert_eq!(
        flows_found(case, LIB_PATHFUL, std::slice::from_ref(&model)),
        0,
        "without the bridge nothing connects alpha_stub to gamma_impl"
    );
}

/// The two artifacts must not already be joined by name coincidence, which is the one way these
/// fixtures could pass without a bridge.
#[test]
fn the_two_halves_share_no_function_name() {
    for callee in ["beta_impl", "gamma_impl"] {
        assert!(!APP.contains(callee), "the app must not define {callee}");
    }
    assert!(!LIB.contains("alpha_stub") && !LIB_PATHFUL.contains("alpha_stub"));
}
