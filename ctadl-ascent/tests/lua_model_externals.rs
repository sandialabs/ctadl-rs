//! Checks that a model file can name a Lua external.
//!
//! This is the only Lua test that needs the engine, because it uses the model layer's match
//! index and its JSON reader. That is why it lives here instead of in [`ctadl_lua`], which
//! would have to take `ctadl-ascent` as a dev-dependency to hold it. The rest of the Lua tests
//! only check the shape of the IR, and they live in the front end next to the code they test.
//!
//! The test imports a real file instead of calling the crate's in-memory `lower_lua_units`.
//! That helper is private, and there is no reason to make it public for one test: a file named
//! `m.lua` gives the same module `m` that the in-memory version would build by hand.

use ctadl_ascent::models::{
    ImportScope, ProgramMatchIndex, ProgramModelMatches, json::ModelGeneratorIngest,
};

/// The externals column is what lets a Lua propagation model file have any effect. Before it
/// existed, a match index was built only from the functions defined in the file, so a model
/// that named a standard-library function matched nothing.
#[test]
fn a_model_can_name_a_lua_external() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("m.lua");
    std::fs::write(
        &path,
        r#"local function h(x) return os.getenv(x) end return h"#,
    )
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
