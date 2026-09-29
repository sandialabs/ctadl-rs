//! `tests/lua/smear.lua`: exactly the `sink_hit_*` calls are reached.

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn lua_fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "lua", name]
        .iter()
        .collect()
}

#[test]
fn a_clean_value_beside_a_tainted_one_stays_clean() {
    let store = tempfile::tempdir().expect("temp store");
    init_store_path(Some(store.path())).expect("init store");

    let import = ArtifactImport::try_create(
        "lua_smear",
        ArtifactLanguage::Lua,
        &lua_fixture("smear.lua"),
    )
    .unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();

    let project =
        AnalysisProject::try_create("lua_smear_proj", &["lua_smear"], SubImports::All).unwrap();
    let models = vec![lua_fixture("smear.json")];
    cli::index(
        &project,
        &[],
        &models,
        false,
        cli::IndexOptions {
            strategy: CallResolutionStrategy::default(),
            ..Default::default()
        },
    )
    .unwrap();

    let out = tempfile::tempdir().unwrap();
    let sarif = out.path().join("out.sarif");
    cli::query(&project, &models, &sarif, SarifProfile::default(), None).unwrap();

    let text = std::fs::read_to_string(&sarif).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    // Strip the `<module>.` prefix.
    let reached: BTreeSet<String> = doc["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["ruleId"]
                .as_str()
                .is_some_and(|id| id.contains("tainted-path"))
                && r["kind"].as_str() == Some("fail")
        })
        .flat_map(|r| r["properties"]["sinkFunctions"].as_array().unwrap().clone())
        .filter_map(|f| {
            f.as_str()
                .map(|s| s.rsplit('.').next().unwrap().to_string())
        })
        .collect();

    let expected: BTreeSet<String> = [
        "sink_hit_field",
        "sink_hit_operand",
        "sink_hit_retarg",
        "sink_hit_return",
        "sink_hit_sibling",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let clean: Vec<_> = reached
        .iter()
        .filter(|s| s.starts_with("sink_clean"))
        .collect();
    assert_eq!(reached, expected, "clean sinks wrongly reached: {clean:?}");
}
