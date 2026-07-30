//! Matching and scoping tests for bridging models and `in`.
//!
//! Emission is covered by the unit tests in `codegen/model_matches/tests.rs`; parse/validate by
//! `models/spec/tests.rs`. What lives here is the middle layer: what the streaming matcher
//! records, given two programs, and what `in` does to an ordinary generator.

use std::io::Write;

use ctadl_ascent::codegen::model_matches::codegen_model_matches;
use ctadl_ascent::index_engine::IndexFacts;
use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
use ctadl_ascent::models::matches::observe_import;
use ctadl_ascent::models::{
    ImportScope, ModelFileSpecs, ProgramMatchIndex, ProgramModelMatches, scan_model_files,
    try_load_models,
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

/// Writes `generators` (one per line) to a `.jsonl` file that lives as long as the returned
/// handle, and scans it.
fn specs_of(generators: &[serde_json::Value]) -> (tempfile::NamedTempFile, ModelFileSpecs) {
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("temp file");
    for g in generators {
        writeln!(file, "{}", serde_json::to_string(g).unwrap()).expect("write");
    }
    file.flush().expect("flush");
    let specs = scan_model_files(&[file.path().to_path_buf()]).expect("scan");
    (file, specs)
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

fn names(set: &std::collections::BTreeSet<ctadl_ascent::facts::Str>) -> Vec<String> {
    set.iter().map(|s| s.as_str().to_string()).collect()
}

/// Streams the given imports past the matcher, in order, and returns what it recorded.
fn stream(
    specs: &ModelFileSpecs,
    imports: &[(ArtifactLanguage, &str, ProgramInfo)],
) -> ProgramModelMatches {
    let mut matches = ProgramModelMatches::default();
    matches.bridges.prepare(&specs.bridges);
    for (language, name, program_info) in imports {
        let index = ProgramMatchIndex::new(program_info, ImportScope::new(*language, name));
        observe_import(&index, &specs.bridges, &mut matches).expect("observe");
    }
    matches
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Two programs, two sides, and the pairs that fall out -- asserted directly, without touching
/// the fact base.
#[test]
fn matches_each_side_in_the_import_its_scope_admits() {
    let (_f, specs) = specs_of(&[lua_to_pcode_bridge()]);
    let matches = stream(
        &specs,
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
    );

    let side = matches.bridges.get(0);
    assert_eq!(names(&side.from), vec!["mylib.add"]);
    assert_eq!(names(&side.to), vec!["l_add"]);
    assert!(side.is_unique());
    let pairs: Vec<_> = side
        .pairs()
        .map(|(a, b)| (a.as_str().to_string(), b.as_str().to_string()))
        .collect();
    assert_eq!(pairs, vec![("mylib.add".to_string(), "l_add".to_string())]);
}

/// The streaming reconciliation, stated as a test: side B's import may arrive *before* side
/// A's, at which point whether `from` matched is unknowable. Evaluation is therefore eager and
/// the pairing is identical either way.
#[test]
fn the_to_side_may_stream_before_the_from_side() {
    let (_f, specs) = specs_of(&[lua_to_pcode_bridge()]);
    let forward = stream(
        &specs,
        &[
            (ArtifactLanguage::Lua, "app", native_program(&["mylib.add"])),
            (ArtifactLanguage::Pcode, "lib", native_program(&["l_add"])),
        ],
    );
    let reversed = stream(
        &specs,
        &[
            (ArtifactLanguage::Pcode, "lib", native_program(&["l_add"])),
            (ArtifactLanguage::Lua, "app", native_program(&["mylib.add"])),
        ],
    );
    assert_eq!(
        names(&forward.bridges.get(0).from),
        names(&reversed.bridges.get(0).from)
    );
    assert_eq!(
        names(&forward.bridges.get(0).to),
        names(&reversed.bridges.get(0).to)
    );
}

/// "Unmatched" means not matched *anywhere in the project*, not per import. A side that matches
/// in one import and not another is matched.
#[test]
fn a_side_matched_in_any_import_counts_as_matched() {
    let (_f, specs) = specs_of(&[serde_json::json!({
        "find": "methods",
        "where": [{"constraint": "signature_match", "name": "shared"}],
        "model": {"bridge": {"to": {"where": [{"constraint": "signature_match", "name": "impl"}]}}}
    })]);
    let matches = stream(
        &specs,
        &[
            (ArtifactLanguage::Dex, "app", native_program(&["shared"])),
            (ArtifactLanguage::Pcode, "lib", native_program(&["impl"])),
        ],
    );
    let side = matches.bridges.get(0);
    assert_eq!(names(&side.from), vec!["shared"]);
    assert_eq!(names(&side.to), vec!["impl"]);
}

/// A scope naming a language no import in the project has admits nothing, so both sides come
/// out empty -- the same observable condition as a `where` that matched nothing, and it gets
/// the same warning rather than a category of its own.
#[test]
fn a_scope_that_admits_no_import_matches_nothing() {
    let (_f, specs) = specs_of(&[lua_to_pcode_bridge()]);
    let matches = stream(
        &specs,
        &[(
            ArtifactLanguage::Dex,
            "app",
            native_program(&["mylib.add", "l_add"]),
        )],
    );
    let side = matches.bridges.get(0);
    assert!(side.from.is_empty(), "the lua scope admits no dex import");
    assert!(side.to.is_empty());
}

/// The cross product, and the counts the `on-ambiguous` warning reports.
#[test]
fn multiple_matches_on_both_sides_produce_the_full_cross_product() {
    let (_f, specs) = specs_of(&[serde_json::json!({
        "find": "methods",
        "in": {"language": "dex"},
        "where": [{"constraint": "name", "pattern": "^exec"}],
        "model": {"bridge": {
            "to": {"in": {"language": "pcode"},
                   "where": [{"constraint": "name", "pattern": "^impl"}]},
            "on-ambiguous": "ignore"
        }}
    })]);
    let matches = stream(
        &specs,
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
    );
    let side = matches.bridges.get(0);
    assert_eq!(side.from.len(), 2);
    assert_eq!(side.to.len(), 2);
    assert!(!side.is_unique());
    assert_eq!(side.pairs().count(), 4);
}

/// A malformed constraint on a bridge side is a hard error, exactly as it is anywhere else in
/// the loader. Only the evaluator can see this one, so it surfaces at match time rather than in
/// the pre-loop scan.
#[test]
fn a_bad_constraint_on_a_bridge_side_is_a_hard_error() {
    let (_f, specs) = specs_of(&[serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": [{"constraint": "no_such_constraint"}]}}}
    })]);
    let mut matches = ProgramModelMatches::default();
    matches.bridges.prepare(&specs.bridges);
    let program_info = native_program(&["a"]);
    let index = ProgramMatchIndex::new(
        &program_info,
        ImportScope::new(ArtifactLanguage::Pcode, "lib"),
    );
    let err = observe_import(&index, &specs.bridges, &mut matches).expect_err("should fail");
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
    let report =
        codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("codegen");
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

/// `ctadl query` reads these counts to say once that it is ignoring index-time constructs
/// instead of dropping them in silence.
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
            "model": {"bridge": {"to": {"where": []}}}
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
