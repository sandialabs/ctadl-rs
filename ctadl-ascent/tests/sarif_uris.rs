//! What a SARIF location says about *where* a finding is.
//!
//! A URI is supposed to locate a finding within the artifact that was scanned. The formatter
//! used to write the source file's absolute path with its leading slash removed, so a finding
//! in a Lua tree imported from `/home/me/src/kong` was reported at
//! `home/me/src/kong/pdk/request.lua` -- an absolute path wearing a relative path's clothes: it
//! resolves against nothing, it says more about the machine that ran the scan than about the
//! code, and it moves when the tree does. Locations are now written relative to the import root
//! (`kong/pdk/request.lua`) and carry the `uriBaseId` whose absolute value `run.originalUriBaseIds`
//! publishes once, which is the indirection SARIF §3.4.4 defines for exactly this.
//!
//! Lua is the frontend here because it needs no external toolchain, so this runs everywhere
//! `cargo test` does. Nothing being checked is Lua-specific: it is about the import root, which
//! every frontend has.

use ctadl_ascent::cli;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use serde_json::Value;
use std::collections::BTreeSet;
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

/// The tainted flow, in a module one directory down from the import root. Where it lives is the
/// point: a flow in a top-level file cannot tell a root-relative URI from a bare file name.
const LIB_SOURCE: &str = r#"local M = {}

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

function M.run()
  local x = source()
  sink(x)
end

return M
"#;

const MAIN_SOURCE: &str = r#"local reader = require("lib.reader")

reader.run()
"#;

const QUERY_MODEL: &str = r#"{"model_generators":[
{"find":"methods","where":[{"constraint":"signature_match","names":["lib.reader.source"]}],
 "model":{"sources":[{"port":"Return","kind":"UserInput"}]}},
{"find":"methods","where":[{"constraint":"signature_match","names":["lib.reader.sink"]}],
 "model":{"sinks":[{"port":"Argument(0)","kind":"UserInput"}]}}
]}"#;

/// Imports a two-file Lua tree as one artifact directory and returns its SARIF.
fn query_source_tree(case: &str) -> Value {
    let dir = store_dir();
    let tree = dir.join(case).join("app");
    std::fs::create_dir_all(tree.join("lib")).expect("creating source tree");
    std::fs::write(tree.join("main.lua"), MAIN_SOURCE).expect("writing main.lua");
    std::fs::write(tree.join("lib").join("reader.lua"), LIB_SOURCE).expect("writing reader.lua");

    let import =
        ArtifactImport::try_create(case, ArtifactLanguage::Lua, &tree).expect("import args");
    cli::import(&import, cli::ImportOptions::default()).expect("importing lua");

    let project = AnalysisProject::try_create(case, &[case.to_string()]).expect("project");
    cli::index(&project, &[], &[], false, cli::IndexOptions::default()).expect("indexing");

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
    serde_json::from_str(&text).expect("parsing sarif")
}

/// Every `artifactLocation` anywhere in the log, as `(uri, uriBaseId)`.
fn artifact_locations(value: &Value) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    collect(value, &mut out);
    fn collect(value: &Value, out: &mut Vec<(String, Option<String>)>) {
        match value {
            Value::Object(map) => {
                if let Some(uri) = map
                    .get("artifactLocation")
                    .and_then(|a| a.get("uri"))
                    .and_then(Value::as_str)
                {
                    let base = map["artifactLocation"]["uriBaseId"]
                        .as_str()
                        .map(str::to_string);
                    out.push((uri.to_string(), base));
                }
                for v in map.values() {
                    collect(v, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    out
}

/// A source location names the file's path *inside the imported tree*, not its path on this
/// machine.
#[test]
fn locations_are_relative_to_the_import_root() {
    let case = "sarif_uris_tree";
    let sarif = query_source_tree(case);
    let locations = artifact_locations(&sarif["runs"][0]["results"]);
    assert!(!locations.is_empty(), "the run reports some location");

    let uris: BTreeSet<&str> = locations.iter().map(|(uri, _)| uri.as_str()).collect();
    for uri in &uris {
        assert!(
            *uri == "app/main.lua" || *uri == "app/lib/reader.lua",
            "location URI {uri} is not a path inside the imported tree (of {uris:?})"
        );
    }
    assert!(
        uris.contains("app/lib/reader.lua"),
        "the flow's source is in the subdirectory, so some location is: {uris:?}"
    );
}

/// Each location says which base its URI is relative to, and the run says where that base is.
#[test]
fn locations_carry_a_base_the_run_defines() {
    let case = "sarif_uris_base";
    let sarif = query_source_tree(case);
    let run = &sarif["runs"][0];
    let bases = &run["originalUriBaseIds"];

    for (uri, base) in artifact_locations(&run["results"]) {
        let base = base.unwrap_or_else(|| panic!("location {uri} has no uriBaseId"));
        let root = bases[&base]["uri"]
            .as_str()
            .unwrap_or_else(|| panic!("run does not define uriBaseId {base} (has {bases:?})"));
        assert!(
            root.starts_with("file://") && root.ends_with('/'),
            "uriBaseId {base} resolves to {root}, which is not a directory URI"
        );
        // Resolving the URI against its base has to land on the file that was imported.
        let path = PathBuf::from(root.trim_start_matches("file://")).join(&uri);
        assert!(path.is_file(), "{root} + {uri} is not a file ({path:?})");
    }
}
