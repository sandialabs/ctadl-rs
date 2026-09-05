//! A model file can name a Lua external.
//!
//! The one Lua test that needs the engine -- the model layer's match index and its JSON ingest
//! -- so it lives here rather than in [`ctadl_lua`], which would need `ctadl-ascent` as a
//! dev-dependency to hold it. The rest of the Lua suite is pure IR-shape checks, and sits in the
//! front end beside what it tests.
//!
//! It imports from a real file rather than calling the crate's in-memory `lower_lua_units`,
//! because that helper is private and there is no reason to widen it for one test: a file named
//! `m.lua` is the module `m` the in-memory version would construct by hand.

use ctadl_ascent::models::{
    ImportScope, ProgramMatchIndex, ProgramModelMatches, json::ModelGeneratorIngest,
};

/// The externals column is what makes a Lua propagation model file do anything: before it,
/// every match index was built from the lowered definitions only, so a model naming a stdlib
/// function matched nothing.
#[test]
fn a_model_can_name_a_lua_external() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("m.lua");
    std::fs::write(&path, r#"local function h(x) return os.getenv(x) end return h"#)
        .expect("writing the module");
    let info = ctadl_lua::import_lua(&path).expect("lua lowering failed");

    for port in ["getenv", "os.getenv"] {
        let mut matches = ProgramModelMatches::default();
        let match_index = ProgramMatchIndex::new(&info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
        ingest
            .encode_models(vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": port}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
            })])
            .unwrap_or_else(|e| panic!("loading a model naming {port}: {e}"));
        drop(ingest);
        assert_eq!(
            matches
                .propagations
                .iter()
                .map(|p| p.function.to_string())
                .collect::<Vec<_>>(),
            vec!["os.getenv".to_string()],
            "a model naming `{port}` must summarize the external"
        );
    }
}
