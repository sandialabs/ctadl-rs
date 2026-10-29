//! Matching and scoping tests for bridging models and `in`.
//!
//! Emission is covered by the unit tests in `codegen/model_matches/tests.rs`; parse/validate by
//! `models/spec/tests.rs`. What lives here is the middle layer: what a JSON bridge pairs, given
//! two programs, and what `in` does to an ordinary generator. A JSON bridge is migrated to a DSL
//! rule whose two sides are two components of its body, paired by the engine once every import
//! has gone past.

use std::io::Write;

use ctadl_ascent::codegen::model_matches::codegen_model_matches;
use ctadl_ascent::index_engine::IndexFacts;
use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
use ctadl_ascent::models::{
    DslFile, DslMatcher, DslModelSet, DslPhase, ImportScope, ProgramMatchIndex,
    ProgramModelMatches, try_load_models,
};
use ctadl_ascent::project::ArtifactLanguage;
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::call::{
    NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A program whose VMT names `functions`, keyed the way a native (pcode) frontend keys them.
/// Nothing here has a body, which is the point: a bridge's most important sides are bodyless.
fn native_program(functions: &[&str]) -> ProgramInfo {
    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: functions
                .iter()
                .map(|n| {
                    (
                        NativeSimpleName((*n).into()),
                        NativeSignature((*n).into()),
                        NativeFunction((*n).into()),
                        NativeQualifiedName((*n).into()),
                    )
                })
                .collect(),
        },
        ..Default::default()
    }
}

/// The bridge the matching cases use: a Lua-side callee joined to a pcode-side implementation.
fn lua_to_pcode_bridge() -> serde_json::Value {
    serde_json::json!({
        "find": "methods",
        "in": {"language": "lua"},
        "where": [{"constraint": "signature_match", "name": "mylib.add"}],
        "model": {"bridge": {
            "to": {"in": {"language": "pcode"},
                   "where": [{"constraint": "signature_match", "name": "l_add"}]},
            "arguments": [{"from": "Argument(0)", "to": "Argument(0).stack.[1]", "direction": "in"}]
        }}
    })
}

/// Loads `generators` as one JSON model file, streams the given imports past it in order, and
/// returns the `(from, to)` pairs its bridges made.
fn stream(
    generators: &[serde_json::Value],
    imports: &[(ArtifactLanguage, &str, ProgramInfo)],
) -> Result<Vec<(String, String)>, ctadl_ascent::error::Error> {
    let file = DslFile::from_json_generators("bridges.jsonl", generators)?;
    let set = DslModelSet { files: vec![file] };
    let mut matcher = DslMatcher::new(&set, DslPhase::All);
    let mut matches = ProgramModelMatches::default();
    for (language, name, program_info) in imports {
        let index = ProgramMatchIndex::new(program_info, ImportScope::new(*language, name));
        matcher.observe_import(&index, &mut matches);
    }
    matcher.finish(&mut matches)?;
    let mut pairs: Vec<(String, String)> = matches
        .resolved_bridges
        .iter()
        .map(|b| (b.from.to_string(), b.to.to_string()))
        .collect();
    pairs.sort();
    Ok(pairs)
}

fn pair(from: &str, to: &str) -> (String, String) {
    (from.to_string(), to.to_string())
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Two programs, two sides, and the pairs that fall out -- asserted directly, without touching
/// the fact base.
#[test]
fn matches_each_side_in_the_import_its_scope_admits() {
    let pairs = stream(
        &[lua_to_pcode_bridge()],
        &[
            (
                ArtifactLanguage::Lua,
                "app",
                native_program(&["mylib.add", "other"]),
            ),
            (
                ArtifactLanguage::Pcode,
                "lib",
                native_program(&["l_add", "l_sub"]),
            ),
        ],
    )
    .expect("load");
    assert_eq!(pairs, vec![pair("mylib.add", "l_add")]);
}

/// Side B's import may arrive *before* side A's, at which point whether `from` matched is
/// unknowable. Each side is accumulated as the imports go past and the pairing is made at the
/// end, so it is identical either way.
#[test]
fn the_to_side_may_stream_before_the_from_side() {
    let forward = stream(
        &[lua_to_pcode_bridge()],
        &[
            (ArtifactLanguage::Lua, "app", native_program(&["mylib.add"])),
            (ArtifactLanguage::Pcode, "lib", native_program(&["l_add"])),
        ],
    )
    .expect("load");
    let reversed = stream(
        &[lua_to_pcode_bridge()],
        &[
            (ArtifactLanguage::Pcode, "lib", native_program(&["l_add"])),
            (ArtifactLanguage::Lua, "app", native_program(&["mylib.add"])),
        ],
    )
    .expect("load");
    assert_eq!(forward, vec![pair("mylib.add", "l_add")]);
    assert_eq!(forward, reversed);
}

/// "Unmatched" means not matched *anywhere in the project*, not per import. A side that matches
/// in one import and not another is matched.
#[test]
fn a_side_matched_in_any_import_counts_as_matched() {
    let pairs = stream(
        &[serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": "shared"}],
            "model": {"bridge": {
                "to": {"where": [{"constraint": "signature_match", "name": "impl"}]},
                "arguments": [{"from": "Argument(0)", "to": "Argument(0)"}]
            }}
        })],
        &[
            (ArtifactLanguage::Dex, "app", native_program(&["shared"])),
            (ArtifactLanguage::Pcode, "lib", native_program(&["impl"])),
        ],
    )
    .expect("load");
    assert_eq!(pairs, vec![pair("shared", "impl")]);
}

/// A scope naming a language no import in the project has admits nothing, so the bridge pairs
/// nothing -- the same observable condition as a `where` that matched nothing.
#[test]
fn a_scope_that_admits_no_import_matches_nothing() {
    let pairs = stream(
        &[lua_to_pcode_bridge()],
        &[(
            ArtifactLanguage::Dex,
            "app",
            native_program(&["mylib.add", "l_add"]),
        )],
    )
    .expect("load");
    assert!(pairs.is_empty(), "the lua scope admits no dex import");
}

/// Several matches on both sides pair as the full cross product.
#[test]
fn multiple_matches_on_both_sides_produce_the_full_cross_product() {
    let pairs = stream(
        &[serde_json::json!({
            "find": "methods",
            "in": {"language": "dex"},
            "where": [{"constraint": "name", "pattern": "^exec"}],
            "model": {"bridge": {
                "to": {"in": {"language": "pcode"},
                       "where": [{"constraint": "name", "pattern": "^impl"}]},
                "arguments": [{"from": "Argument(0)", "to": "Argument(0)"}],
                "on-ambiguous": "ignore"
            }}
        })],
        &[
            (
                ArtifactLanguage::Dex,
                "app",
                native_program(&["exec1", "exec2", "nope"]),
            ),
            (
                ArtifactLanguage::Pcode,
                "lib",
                native_program(&["impl1", "impl2"]),
            ),
        ],
    )
    .expect("load");
    assert_eq!(
        pairs,
        vec![
            pair("exec1", "impl1"),
            pair("exec1", "impl2"),
            pair("exec2", "impl1"),
            pair("exec2", "impl2"),
        ]
    );
}

/// A malformed constraint on a bridge side is a hard error, exactly as it is anywhere else in
/// the file, and it fails the load before any import is matched.
#[test]
fn a_bad_constraint_on_a_bridge_side_is_a_hard_error() {
    let err = stream(
        &[serde_json::json!({
            "find": "methods",
            "model": {"bridge": {
                "to": {"where": [{"constraint": "no_such_constraint"}]},
                "arguments": [{"from": "Argument(0)", "to": "Argument(0)"}]
            }}
        })],
        &[(ArtifactLanguage::Pcode, "lib", native_program(&["a"]))],
    )
    .expect_err("should fail");
    let err = crate::common::without_context(err);
    assert!(format!("{err}").contains("JSON model"), "{err}");
}

// ---------------------------------------------------------------------------
// `in` on an ordinary generator
// ---------------------------------------------------------------------------

/// Writes one generator to a file and loads it against `(language, import)`, returning the
/// functions it emitted endpoints for.
fn endpoints_under(
    generator: serde_json::Value,
    language: ArtifactLanguage,
    import: &str,
) -> Vec<String> {
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("temp file");
    writeln!(file, "{}", serde_json::to_string(&generator).unwrap()).expect("write");
    file.flush().expect("flush");

    let program_info = native_program(&["a", "b"]);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::new(language, import));
    let mut matches = ProgramModelMatches::default();
    try_load_models(&index, file.path(), &mut matches).expect("load");
    matches
        .endpoints
        .iter()
        .map(|r| r.function.to_string())
        .collect()
}

#[test]
fn in_scopes_an_ordinary_generator_to_its_import() {
    let generator = serde_json::json!({
        "find": "methods",
        "in": {"language": "pcode"},
        "where": [{"constraint": "signature_match", "name": "a"}],
        "model": {"sources": [{"kind": "K", "port": "Return"}]}
    });
    assert_eq!(
        endpoints_under(generator.clone(), ArtifactLanguage::Pcode, "lib"),
        vec!["a".to_string()]
    );
    // Same file, wrong artifact: the generator contributes nothing rather than matching by name
    // alone. This is what lets one `--models` file carry libc models for the binary and Java
    // models for the app.
    assert!(endpoints_under(generator, ArtifactLanguage::Dex, "app").is_empty());
}

#[test]
fn in_can_name_the_import_rather_than_the_language() {
    let generator = serde_json::json!({
        "find": "methods",
        "in": {"import": "lib"},
        "where": [{"constraint": "signature_match", "name": "a"}],
        "model": {"sources": [{"kind": "K", "port": "Return"}]}
    });
    assert_eq!(
        endpoints_under(generator.clone(), ArtifactLanguage::Pcode, "lib"),
        vec!["a".to_string()]
    );
    assert!(endpoints_under(generator, ArtifactLanguage::Pcode, "app").is_empty());
}

/// `dex` and `apk` are the same frontend and share a slot model, so scoping across both is the
/// natural thing to write -- and the VMT variant, which is what selects the *built-in* default
/// file, cannot express it.
#[test]
fn in_distinguishes_languages_the_vmt_cannot() {
    let generator = serde_json::json!({
        "find": "methods",
        "in": {"languages": ["dex", "apk"]},
        "where": [{"constraint": "signature_match", "name": "a"}],
        "model": {"sources": [{"kind": "K", "port": "Return"}]}
    });
    for language in [ArtifactLanguage::Dex, ArtifactLanguage::Apk] {
        assert_eq!(
            endpoints_under(generator.clone(), language, "app"),
            vec!["a".to_string()],
            "{language} should be admitted"
        );
    }
    assert!(
        endpoints_under(generator, ArtifactLanguage::Jar, "app").is_empty(),
        "a jar numbers parameters differently, so the same map would be wrong for it"
    );
}

// ---------------------------------------------------------------------------
// Access paths
// ---------------------------------------------------------------------------

/// The human-declared registry: paths that occur nowhere in the IR, so nothing else registers
/// them, reaching the initial indexer paths through phase 2.
#[test]
fn declared_access_paths_survive_loading_and_reach_the_facts() {
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("temp file");
    writeln!(
        file,
        "{}",
        serde_json::to_string(&serde_json::json!({
            "find": "methods",
            "model": {"access_paths": [".next.next.next", ".stack.[1]"]}
        }))
        .unwrap()
    )
    .expect("write");
    file.flush().expect("flush");

    let program_info = native_program(&["a"]);
    let index = ProgramMatchIndex::new(
        &program_info,
        ImportScope::new(ArtifactLanguage::Pcode, "lib"),
    );
    let mut matches = ProgramModelMatches::default();
    try_load_models(&index, file.path(), &mut matches).expect("load");
    assert_eq!(matches.access_paths.len(), 2);

    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    let report = codegen_model_matches(&matches, &mut facts, &mut source_info);
    assert_eq!(report.declared_paths, 2);
    assert_eq!(facts.paths.len(), 2);
}

/// A misspelled path is a load-time error naming it, not a silently-mutated path.
#[test]
fn a_malformed_declared_access_path_fails_the_load() {
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("temp file");
    writeln!(
        file,
        "{}",
        serde_json::to_string(&serde_json::json!({
            "find": "methods",
            "model": {"access_paths": [".a..b"]}
        }))
        .unwrap()
    )
    .expect("write");
    file.flush().expect("flush");

    let program_info = native_program(&["a"]);
    let index = ProgramMatchIndex::new(
        &program_info,
        ImportScope::new(ArtifactLanguage::Pcode, "lib"),
    );
    let mut matches = ProgramModelMatches::default();
    assert!(try_load_models(&index, file.path(), &mut matches).is_err());
}

// ---------------------------------------------------------------------------
// Index-time-only model accounting
// ---------------------------------------------------------------------------

/// A load counts the index-time constructs it saw, so a caller that ignores them can say so
/// instead of dropping them in silence.
///
/// The bridge carries an `arguments` map: without one the JSON format pairs the arguments the
/// two sides share, which needs the fact base, and the migration refuses to guess it.
#[test]
fn index_time_constructs_are_counted_for_the_query_side_warning() {
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("temp file");
    for g in [
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": "a"}],
            "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
        }),
        serde_json::json!({
            "find": "methods",
            "model": {"bridge": {
                "to": {"where": []},
                "arguments": [{"from": "Argument(0)", "to": "Argument(0)", "direction": "in"}]
            }}
        }),
    ] {
        writeln!(file, "{}", serde_json::to_string(&g).unwrap()).expect("write");
    }
    file.flush().expect("flush");

    let program_info = native_program(&["a"]);
    let index = ProgramMatchIndex::new(
        &program_info,
        ImportScope::new(ArtifactLanguage::Pcode, "lib"),
    );
    let mut matches = ProgramModelMatches::default();
    let report = try_load_models(&index, file.path(), &mut matches).expect("load");
    assert_eq!(report.index_time_models.propagations, 1);
    assert_eq!(report.index_time_models.bridges, 1);
    assert!(report.index_time_models.describe().contains("bridging"));
}
