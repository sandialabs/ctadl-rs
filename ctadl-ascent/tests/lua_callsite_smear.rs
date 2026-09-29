//! The call-site smear, through the Lua front end.
//!
//! `tests/lua/smear.lua` holds five small functions. In each one the value passed to
//! `sink_hit_*` really comes from a source, and the value passed to `sink_clean_*` never does:
//! it merely shares an expression, a call site or a table with a tainted value. A correct query
//! reports exactly the five `sink_hit_*` calls.
//!
//! The front end is not the cause: `ctadl inspect --dump-ir` shows each clean variable is read
//! and never written. The extra flows are added by the shared query engine, which is why the
//! same five shapes misbehave identically through the tree-sitter C front end
//! (`eval_c/patches/smear-explained/`). Lua is used here because it needs no toolchain.
//!
//! Run by hand:
//!
//! ```text
//! ctadl import -l lua tests/lua/smear.lua --name smear --store $ST
//! ctadl index smear -m tests/lua/smear.json --store $ST
//! ctadl query smear -m tests/lua/smear.json --store $ST -s human -o out.sarif
//! ```

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Absolute path to a checked-in Lua test fixture under `tests/lua/`.
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
    // Lua functions are reported as `<module>.<name>`; keep the name.
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
