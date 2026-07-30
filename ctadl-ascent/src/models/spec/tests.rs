//! Parse/validate tests for bridge specs, `in` scopes and port maps.
//!
//! None of these needs a program: everything under test here is a property of the model file's
//! shape. That is the whole point of hoisting the scan out of the import loop.

use std::io::Write;

use super::*;
use crate::project::ArtifactLanguage::*;

/// Writes `text` to a temp file with the given extension and scans it.
fn scan(ext: &str, text: &str) -> Result<ModelFileSpecs, Error> {
    let mut file = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile()
        .expect("temp file");
    file.write_all(text.as_bytes()).expect("write");
    file.flush().expect("flush");
    let path = file.path().to_path_buf();
    let result = scan_model_files(&[path]);
    // Keep the file alive until after the scan.
    drop(file);
    result
}

/// One generator, in JSONL, wrapped so the caller writes only the interesting part.
fn scan_one(generator: serde_json::Value) -> Result<ModelFileSpecs, Error> {
    scan("jsonl", &serde_json::to_string(&generator).unwrap())
}

fn a_bridge() -> serde_json::Value {
    serde_json::json!({
        "find": "methods",
        "in": {"language": "lua"},
        "where": [{"constraint": "signature_match", "name": "mylib.add"}],
        "model": {"bridge": {
            "to": {"in": {"language": "pcode"},
                   "where": [{"constraint": "signature_match", "name": "l_add"}]},
            "arguments": [
                {"from": "Argument(0)", "to": "Argument(0).stack.[1]", "direction": "in"},
                {"from": "Return", "to": "Argument(0).stack.[-1]", "direction": "out"}
            ]
        }}
    })
}

/// The whole error chain, flattened. `Error::Context` renders only its own context, so the
/// loader message a test cares about lives one or more `source()` hops down.
fn errors_of(result: Result<ModelFileSpecs, Error>) -> String {
    let err = match result {
        Ok(_) => panic!("expected the scan to fail"),
        Err(e) => e,
    };
    let mut text = err.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&err);
    while let Some(e) = source {
        text.push_str(&format!("\n{e}"));
        source = e.source();
    }
    text
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[test]
fn parses_a_bridge() {
    let specs = scan_one(a_bridge()).expect("scan");
    assert_eq!(specs.bridges.len(), 1);
    let b = &specs.bridges[0];
    assert_eq!(b.index, 0);
    assert_eq!(b.from.scope.languages, vec![Lua]);
    assert_eq!(b.to.scope.languages, vec![Pcode]);
    assert!(b.ports_given);
    assert_eq!(b.ports.len(), 2);
    assert_eq!(*b.ports[0].from.index, 0);
    assert_eq!(b.ports[0].direction, Direction::In);
    assert_eq!(*b.ports[1].from.index, RETURN_INDEX);
    assert_eq!(b.ports[1].direction, Direction::Out);
    // Defaults: warn on both sides and on an ambiguous pairing.
    assert_eq!(b.from.on_unmatched, Severity::Warn);
    assert_eq!(b.to.on_unmatched, Severity::Warn);
    assert_eq!(b.on_ambiguous, Severity::Warn);
}

#[test]
fn a_file_with_no_bridge_yields_no_specs() {
    let specs = scan_one(serde_json::json!({
        "find": "methods",
        "where": [{"constraint": "signature_match", "name": "strcpy"}],
        "model": {"propagation": [{"input": "Argument(1)", "output": "Argument(0)"}]}
    }))
    .expect("scan");
    assert!(specs.is_empty());
}

#[test]
fn scans_json_and_json5_as_well_as_jsonl() {
    let one = serde_json::to_string(&a_bridge()).unwrap();
    let wrapped = format!(r#"{{"model_generators": [{one}]}}"#);
    assert_eq!(scan("json", &wrapped).expect("json").bridges.len(), 1);
    assert_eq!(scan("json5", &wrapped).expect("json5").bridges.len(), 1);
}

/// A bridge's callee-side path is what makes it more than a bare call edge, so the port map has
/// to survive the canonical access-path grammar intact -- including the offset-vs-symbol
/// distinction, which gets it backwards silently rather than loudly.
#[test]
fn port_paths_keep_the_offset_symbol_distinction() {
    let specs = scan_one(a_bridge()).expect("scan");
    let to = specs.bridges[0].ports[0].to;
    let segs: Vec<_> = to.path.iter().cloned().collect();
    assert_eq!(
        segs,
        vec![
            ctadl_ir::mir::PathSegment::symbol("stack"),
            ctadl_ir::mir::PathSegment::offset(1),
        ]
    );
}

// ---------------------------------------------------------------------------
// `in` scopes
// ---------------------------------------------------------------------------

#[test]
fn language_is_the_one_element_case_of_languages() {
    let mut errors = Vec::new();
    let one = ProgramScope::parse(Some(&serde_json::json!({"language": "dex"})), 0, &mut errors);
    let many = ProgramScope::parse(
        Some(&serde_json::json!({"languages": ["dex"]})),
        0,
        &mut errors,
    );
    assert!(errors.is_empty(), "{errors:?}");
    // The two spellings must not be able to drift.
    assert_eq!(one, many);
}

#[test]
fn language_and_languages_together_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "in": {"language": "dex", "languages": ["apk"]},
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("mutually exclusive"), "{msg}");
}

#[test]
fn an_empty_languages_list_is_an_error() {
    // It would otherwise admit nothing, quietly -- the design's dominant failure mode.
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "in": {"languages": []},
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("must not be empty"), "{msg}");
}

#[test]
fn an_unknown_language_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "in": {"language": "kotlin"},
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("not a known artifact language"), "{msg}");
    assert!(msg.contains("'dex'"), "the message lists the choices: {msg}");
}

#[test]
fn scope_keys_are_anded_and_an_absent_key_is_unconstrained() {
    let mut errors = Vec::new();
    let scope = ProgramScope::parse(
        Some(&serde_json::json!({"languages": ["dex", "apk"], "import": "app"})),
        0,
        &mut errors,
    );
    assert!(errors.is_empty());
    assert!(scope.admits(&ImportScope::new(Dex, "app")));
    assert!(scope.admits(&ImportScope::new(Apk, "app")));
    assert!(!scope.admits(&ImportScope::new(Jar, "app")), "wrong language");
    assert!(!scope.admits(&ImportScope::new(Dex, "lib")), "wrong import");

    // Only the import named; every language admitted.
    let scope = ProgramScope::parse(
        Some(&serde_json::json!({"import": "app"})),
        0,
        &mut errors,
    );
    assert!(scope.admits(&ImportScope::new(Lua, "app")));
    assert!(!scope.admits(&ImportScope::new(Lua, "other")));
}

/// An import with no identity is admitted only by a scope that constrains nothing. Anything
/// else would apply a `pcode`-scoped libc model to a dex program.
#[test]
fn an_unknown_import_is_admitted_only_by_an_unconstrained_scope() {
    assert!(ProgramScope::default().admits(&ImportScope::unknown()));
    let mut errors = Vec::new();
    let scope = ProgramScope::parse(Some(&serde_json::json!({"language": "dex"})), 0, &mut errors);
    assert!(!scope.admits(&ImportScope::unknown()));
}

#[test]
fn an_unknown_key_in_a_scope_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "in": {"langauge": "dex"},
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("langauge"), "{msg}");
}

// ---------------------------------------------------------------------------
// Unknown keys, at all three levels
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_generator_key_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "wehre": [],
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("wehre"), "{msg}");
}

#[test]
fn an_unknown_bridge_key_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": []}, "cardinality": "one-to-one"}}
    })));
    assert!(msg.contains("cardinality"), "{msg}");
}

#[test]
fn an_unknown_key_in_the_to_block_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": [], "on-unmached": "ignore"}}}
    })));
    assert!(msg.contains("on-unmached"), "{msg}");
}

#[test]
fn an_unknown_port_map_key_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": []},
                             "arguments": [{"from": "Argument(0)", "to": "Argument(1)",
                                            "dirction": "in"}]}}
    })));
    assert!(msg.contains("dirction"), "{msg}");
}

// ---------------------------------------------------------------------------
// Required and rejected shapes
// ---------------------------------------------------------------------------

#[test]
fn a_bridge_without_a_to_block_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"arguments": []}}
    })));
    assert!(msg.contains("'to'"), "{msg}");
}

/// `Argument(*)` has no correspondent on the other side, so there is nothing for it to map to.
#[test]
fn a_wildcard_port_is_rejected() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": []},
                             "arguments": [{"from": "Argument(*)", "to": "Argument(1)"}]}}
    })));
    assert!(msg.contains("wildcard has no"), "{msg}");
}

/// `find: callsites` on a bridge is a hard error, not a silently ignored key: `call` is an EDB
/// relation, so a callsite bridge would see only the statically emitted call rows.
#[test]
fn find_callsites_with_a_bridge_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "callsites",
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("find: methods"), "{msg}");
}

#[test]
fn a_missing_find_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("find"), "{msg}");
}

/// The older traversal did `.as_array().unwrap()` on `where`, so a scalar there panicked.
#[test]
fn a_non_array_where_is_an_error_not_a_panic() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "where": {"constraint": "signature_match", "name": "x"},
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("must be an array"), "{msg}");

    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": "not-an-array"}}}
    })));
    assert!(msg.contains("must be an array"), "{msg}");
}

#[test]
fn each_severity_setting_parses() {
    for (text, want) in [
        ("ignore", Severity::Ignore),
        ("warn", Severity::Warn),
        ("error", Severity::Error),
    ] {
        let specs = scan_one(serde_json::json!({
            "find": "methods",
            "on-unmatched": text,
            "model": {"bridge": {"to": {"where": [], "on-unmatched": text},
                                 "on-ambiguous": text}}
        }))
        .unwrap_or_else(|e| panic!("{text}: {e:#}"));
        let b = &specs.bridges[0];
        assert_eq!(b.from.on_unmatched, want);
        assert_eq!(b.to.on_unmatched, want);
        assert_eq!(b.on_ambiguous, want);
    }
}

#[test]
fn an_unknown_severity_is_an_error() {
    let msg = errors_of(scan_one(serde_json::json!({
        "find": "methods",
        "on-unmatched": "shout",
        "model": {"bridge": {"to": {"where": []}}}
    })));
    assert!(msg.contains("'shout'"), "{msg}");
}

#[test]
fn an_omitted_arguments_map_is_recorded_as_omitted() {
    let specs = scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": []}}}
    }))
    .expect("scan");
    assert!(!specs.bridges[0].ports_given);
    assert!(specs.bridges[0].ports.is_empty());
}

#[test]
fn direction_defaults_to_both() {
    let specs = scan_one(serde_json::json!({
        "find": "methods",
        "model": {"bridge": {"to": {"where": []},
                             "arguments": [{"from": "Argument(0)", "to": "Argument(1)"}]}}
    }))
    .expect("scan");
    assert_eq!(specs.bridges[0].ports[0].direction, Direction::Both);
}

// ---------------------------------------------------------------------------
// Declared access paths
// ---------------------------------------------------------------------------

#[test]
fn a_declared_access_path_parses_with_the_canonical_grammar() {
    let path = parse_declared_access_path(".next.next.next", 0).expect("parse");
    assert_eq!(path.len(), 3);
    // Offsets stay offsets; `\[` escapes a field name that begins with a bracket.
    let path = parse_declared_access_path(r".stack.[1]", 0).expect("parse");
    let segs: Vec<_> = path.iter().cloned().collect();
    assert_eq!(
        segs,
        vec![
            ctadl_ir::mir::PathSegment::symbol("stack"),
            ctadl_ir::mir::PathSegment::offset(1),
        ]
    );
}

#[test]
fn a_malformed_declared_access_path_is_an_error() {
    assert!(parse_declared_access_path(".a..b", 0).is_err());
    // The empty path is registered by construction, so naming it is a mistake worth reporting.
    assert!(parse_declared_access_path("", 0).is_err());
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

#[test]
fn a_spec_carries_its_file_and_generator_index() {
    let two = format!(
        "{}\n{}\n",
        serde_json::to_string(&serde_json::json!({
            "find": "methods",
            "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
        }))
        .unwrap(),
        serde_json::to_string(&a_bridge()).unwrap()
    );
    let specs = scan("jsonl", &two).expect("scan");
    assert_eq!(specs.bridges.len(), 1);
    // The index counts *generators*, so the bridge is 1 even though it is the second line.
    assert_eq!(specs.bridges[0].index, 1);
    assert!(specs.bridges[0].provenance().ends_with(":1"));
}

/// Comment and blank lines do not consume a generator index, matching how the rest of the
/// loader numbers them.
#[test]
fn comment_lines_do_not_consume_a_generator_index() {
    let text = format!(
        "// a comment\n\n{}\n",
        serde_json::to_string(&a_bridge()).unwrap()
    );
    let specs = scan("jsonl", &text).expect("scan");
    assert_eq!(specs.bridges[0].index, 0);
}
