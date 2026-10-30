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
//! `Jni:<stem>+bridge` cases run the same join declaratively under `--no-jni-bridge`, as a direct
//! A/B against the built-in pass.

use crate::common::store_dir;
use ctadl_ascent::cli;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use serde_json::Value;
use std::path::{Path, PathBuf};

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
    let (app, lib) = import_pair(case, lib_text);
    let project = AnalysisProject::try_create(case, &[app, lib], SubImports::All).expect("project");
    let query_models = store_dir().join(format!("{case}-query.json"));
    std::fs::write(&query_models, QUERY_MODEL).expect("writing query model");
    tainted_paths(&index_and_query(&project, models, &query_models))
}

/// Indexes `project` with `models`, queries it with `query_models`, and returns the SARIF
/// results.
fn index_and_query(
    project: &AnalysisProject,
    models: &[PathBuf],
    query_models: &Path,
) -> Vec<Value> {
    cli::index(project, &[], models, false, cli::IndexOptions::default()).expect("indexing");
    let sarif: PathBuf = store_dir().join(format!("{}.sarif", project.name));
    cli::query(
        project,
        &[query_models.to_path_buf()],
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
        .cloned()
        .collect()
}

/// How many of `results` are complete source-to-sink flows.
fn tainted_paths(results: &[Value]) -> usize {
    results
        .iter()
        .filter(|r| r["ruleId"] == "C0001.tainted-path" && r["kind"] == "fail")
        .count()
}

/// Writes what `ctadl migrate-models` makes of `json` next to it, and returns its path.
fn migrated(json: &Path) -> PathBuf {
    let (text, _) = ctadl_ascent::models::dsl::migrate::migrate_file(json, None).expect("migrates");
    let path = json.with_extension("ctadl");
    std::fs::write(&path, text).expect("writing the migration");
    path
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

/// A JSON model file and its `ctadl migrate-models` output give the same index and the same
/// query results, for a bridge whose two sides are in two imports. The pathful case is used
/// because it has both a bridge and a propagation, scoped to two different imports.
///
/// The project is indexed and queried twice, once with each pair of files, so everything but
/// the model files is held fixed.
#[test]
fn a_json_model_file_and_its_migration_give_the_same_results() {
    let case = "bridge_e2e_migrated";
    let app = format!("{case}_app");
    let lib = format!("{case}_lib");
    let json = model_file(
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
    let query_json = store_dir().join(format!("{case}-query.json"));
    std::fs::write(&query_json, QUERY_MODEL).expect("writing query model");

    let (app, lib) = import_pair(case, LIB_PATHFUL);
    let project = AnalysisProject::try_create(case, &[app, lib], SubImports::All).expect("project");
    let from_json = index_and_query(&project, std::slice::from_ref(&json), &query_json);
    let from_dsl = index_and_query(&project, &[migrated(&json)], &migrated(&query_json));

    assert!(tainted_paths(&from_json) >= 1, "the fixture finds a flow");
    assert_eq!(from_json, from_dsl);
}

/// Each JSON model file draws one deprecation warning however many times the process loads it:
/// here the model file by `ctadl index` over two imports, and both files by `ctadl query`.
#[test]
fn a_json_model_file_draws_one_deprecation_warning() {
    crate::common::capture_warnings();
    let case = "bridge_e2e_deprecated";
    let model = model_file(
        case,
        &[plain_bridge(&format!("{case}_app"), &format!("{case}_lib"))],
    );
    assert_eq!(flows_found(case, LIB, std::slice::from_ref(&model)), 1);

    let query = store_dir().join(format!("{case}-query.json"));
    for path in [&model, &query] {
        let expected = ctadl_ascent::models::dsl::json_deprecation_warning(path);
        let seen = crate::common::warnings()
            .iter()
            .filter(|w| **w == expected)
            .count();
        assert_eq!(seen, 1, "{expected}");
    }
    assert!(
        crate::common::warnings()
            .iter()
            .any(|w| w.contains("JSON model files are deprecated")
                && w.contains(&format!("ctadl migrate-models {}", model.display()))),
        "the warning says how to convert the file"
    );
}

/// A generator the migration cannot carry across exactly fails `ctadl index`, and the error
/// names the file and the generator. Here a bridge with no `arguments` map, which the JSON
/// loader filled in from the fact base and a rule cannot.
#[test]
fn a_migration_error_fails_index_naming_the_generator() {
    let case = "bridge_e2e_unmigratable";
    let mut unmapped = plain_bridge(&format!("{case}_app"), &format!("{case}_lib"));
    unmapped["model"]["bridge"]
        .as_object_mut()
        .unwrap()
        .remove("arguments");
    let model = model_file(
        case,
        &[
            plain_bridge(&format!("{case}_app"), &format!("{case}_lib")),
            unmapped,
        ],
    );
    let (app, lib) = import_pair(case, LIB);
    let project = AnalysisProject::try_create(case, &[app, lib], SubImports::All).expect("project");
    let err = cli::index(
        &project,
        &[],
        std::slice::from_ref(&model),
        false,
        cli::IndexOptions::default(),
    )
    .expect_err("an unmigratable generator fails the index");
    let message = format!("{:#}", anyhow::Error::from(err));
    assert!(
        message.contains(&model.display().to_string()),
        "names the file: {message}"
    );
    assert!(
        message.contains("cannot migrate 'bridge' in model generator at index 1"),
        "names the generator: {message}"
    );
    assert!(
        !message.contains("index 0"),
        "only the generator that failed: {message}"
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
