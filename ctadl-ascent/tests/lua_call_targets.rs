//! Indirect calls through Lua closures.
//!
//! - `tests/lua/multitarget.lua`: a call that may reach either of two closures enters both. The
//!   same query defect as `test_cli_query_c_funcptr_with_two_targets`, through another front end.
//! - `tests/lua/throughindirect.lua`: a call target crosses an indirect call, both ways. The same
//!   index defect as `test_cli_query_c_funcptr_through_indirect_call`.

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Once;

static INIT: Once = Once::new();

/// The store path is process-wide, so every test shares one temp store, set up once.
fn init_store() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp store"));
    INIT.call_once(|| {
        init_store_path(Some(dir.path())).expect("init store");
    });
}

fn lua_fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "lua", name]
        .iter()
        .collect()
}

/// Imports, indexes and queries `tests/lua/<stem>.lua` with `<stem>.json`, and returns the sinks
/// reached, without the `<module>.` prefix.
fn lua_sinks_reached(stem: &str) -> BTreeSet<String> {
    init_store();

    let import_name = format!("lua_{stem}");
    let import = ArtifactImport::try_create(
        &import_name,
        ArtifactLanguage::Lua,
        &lua_fixture(&format!("{stem}.lua")),
    )
    .unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();

    let project = AnalysisProject::try_create(
        &format!("{import_name}_proj"),
        &[&import_name],
        SubImports::All,
    )
    .unwrap();
    let models = vec![lua_fixture(&format!("{stem}.json"))];
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

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap();
    doc["runs"][0]["results"]
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
        .collect()
}

#[test]
fn a_call_with_two_possible_callees_enters_both() {
    assert_eq!(
        lua_sinks_reached("multitarget"),
        ["sink_a", "sink_b"].map(String::from).into()
    );
}

#[test]
fn a_call_target_crosses_an_indirect_call() {
    assert_eq!(
        lua_sinks_reached("throughindirect"),
        ["sink_down", "sink_down_formal", "sink_up"]
            .map(String::from)
            .into()
    );
}
