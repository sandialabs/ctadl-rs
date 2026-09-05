//! A model file can name a Lua external.
//!
//! This lived in the Lua front end's own `mod tests` until the front end became
//! [`ctadl_lua`]. It is the one Lua test that needs the engine -- the model layer's match index
//! and its JSON ingest -- so it moves here rather than dragging `ctadl-ascent` back into
//! `ctadl-lua` as a dev-dependency. The other fourteen are pure IR-shape checks and stayed.
//!
//! It imports from a real file rather than calling the crate's in-memory `lower_lua_units`,
//! because that helper is private and there is no reason to widen it for one test: a file named
//! `m.lua` is the module `m` the in-memory version constructed by hand.

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
