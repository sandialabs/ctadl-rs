// Tests for what Stage 1 records in an `EndpointMatch`.
use super::*;
use crate::facts;
use crate::facts::TaintDirection;
use crate::models::json::ModelGeneratorIngest;
use ctadl_ir::mir;
use ctadl_ir::mir::{PathSegment, ProgramInfo};

/// A native (binary-frontend) program with one 2-parameter function per name, each carrying a
/// local called `buf`.
fn native_program(names: &[&str]) -> ProgramInfo {
    use ctadl_ir::mir::call::{
        NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };
    use ctadl_ir::mir::{
        BasicBlockData, FunctionData, Functions, ParameterType, Program, Statement, StatementKind,
    };

    let functions: Vec<FunctionData> = names
        .iter()
        .map(|name| {
            let mut f = FunctionData::default();
            f.set_name((*name).to_string());
            f.params.parameters.push(ParameterType::ByVal);
            f.params.parameters.push(ParameterType::ByVal);
            f.locals.get_or_intern("buf");
            let blocks = f.blocks.blocks_mut();
            let body = blocks.push(BasicBlockData::new(None));
            blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
            f
        })
        .collect();

    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: names
                .iter()
                .map(|name| {
                    (
                        NativeSimpleName((*name).into()),
                        NativeSignature((*name).into()),
                        NativeFunction((*name).into()),
                        NativeQualifiedName((*name).into()),
                    )
                })
                .collect(),
        },
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

/// Matches `generators` against a program containing `names` and returns what Stage 1 emitted.
fn endpoints_of(names: &[&str], generators: Vec<serde_json::Value>) -> Vec<EndpointMatch> {
    let program_info = native_program(names);
    let mut out = ProgramModelMatches::default();
    {
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
        ingest.encode_models(generators).expect("encoding models");
    }
    out.endpoints
}

/// Every field of an [`EndpointMatch`] is load-bearing at query time, and a dropped one narrows
/// or widens taint results silently. This pins each one against the port that sets it.
#[test]
fn a_function_anchored_port_fills_its_fields() {
    let endpoints = endpoints_of(
        &["f"],
        vec![serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": "f"}],
            "model": {"sources": [{"kind": "lbl1", "port": "Return.field1.sub"}]},
        })],
    );
    assert_eq!(
        endpoints,
        vec![EndpointMatch {
            function: facts::Str::from("f"),
            selector_ty: FormalIndexTypeTag::Return,
            // A `Return` port carries no formal index; Stage 2 supplies `RETURN_INDEX`.
            index: None,
            path: facts::Path::from_accesses([
                PathSegment::symbol("field1"),
                PathSegment::symbol("sub"),
            ]),
            label: facts::Str::from("lbl1"),
            direction: TaintDirection::Forward,
            wildcard: false,
            saturating: false,
            in_function: None,
            callsite_scoped: false,
            local_index: None,
        }]
    );
}

/// A sink port: the backward direction, a positional index, an empty access path, and the
/// sink-only `wildcard` (which defaults to `true`) -- the other end of the field matrix from
/// the test above.
#[test]
fn a_wildcard_sink_fills_its_fields() {
    let endpoints = endpoints_of(
        &["g"],
        vec![serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": "g"}],
            "model": {"sinks": [{"kind": "lbl2", "port": "Argument(1)"}]},
        })],
    );
    assert_eq!(
        endpoints,
        vec![EndpointMatch {
            function: facts::Str::from("g"),
            selector_ty: FormalIndexTypeTag::Index,
            index: Some(1),
            path: facts::Path::empty(),
            label: facts::Str::from("lbl2"),
            direction: TaintDirection::Backward,
            wildcard: true,
            saturating: false,
            in_function: None,
            callsite_scoped: false,
            local_index: None,
        }]
    );
}

/// A `Variable(name)` port carries its base `LocalIdx` in `local_index`, resolved against the
/// matched function's pre-optimization `locals`. Stage 2 cannot re-derive it -- see
/// [`EndpointMatch::local_index`] -- so it has to survive here.
#[test]
fn a_variable_port_carries_its_local_index() {
    let endpoints = endpoints_of(
        &["h"],
        vec![serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": "h"}],
            "model": {"sources": [{"kind": "lbl3", "port": "Variable(buf).headers"}]},
        })],
    );
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].selector_ty, FormalIndexTypeTag::Local);
    assert_eq!(endpoints[0].index, None);
    assert_eq!(endpoints[0].local_index, Some(0));
    assert_eq!(
        endpoints[0].path,
        facts::Path::from_accesses([PathSegment::symbol("headers")])
    );
}

/// The two spellings a bracketed segment can have must stay distinguishable, or a port naming
/// the real `Offset(8)` a binary frontend emits and one naming the synthetic `Symbol("[]")` the
/// dex/jvm/lua frontends emit collapse into each other.
///
/// This used to be pinned by round-tripping a segment through the model layer's columnar
/// access-path encoding, which stored one canonical escaped spelling per row. That encoding is
/// gone -- a matched port is a `facts::Path` from the moment it parses -- so the guarantee is
/// pinned where it now lives, at the spelling functions themselves.
#[test]
fn bracketed_segment_spellings_stay_distinct() {
    use ctadl_ir::mir::Offset;

    for seg in [
        PathSegment::Offset(Offset(8)),
        PathSegment::symbol("[8]"),
        PathSegment::symbol("[]"),
        PathSegment::symbol("plain"),
    ] {
        let spelled = mir::segment_to_string(&seg);
        assert_eq!(
            mir::parse_segment(&spelled).expect("re-parses"),
            seg,
            "{spelled:?} did not round-trip"
        );
    }
    assert_ne!(
        mir::segment_to_string(&PathSegment::Offset(Offset(8))),
        mir::segment_to_string(&PathSegment::symbol("[8]"))
    );
}

/// Tests for the per-generator capture the no-index model check reads.
///
/// The capture is what makes a count trustworthy without an index, so what is pinned here is
/// exactly the two ways a count can lie: an unnarrowed generator reported as zero, and a
/// narrowed one disagreeing with the set the matcher actually used.
mod capture {
    use super::*;
    use crate::models::json::MatchedFunctions;

    /// A frontend with no method table -- what pcode uses. The match index falls back to the
    /// IR function names, so a `where` still matches; it is only `matched_functions(&All)`
    /// that has nothing to enumerate.
    fn unknown_vmt_program(names: &[&str]) -> ProgramInfo {
        use ctadl_ir::mir::call::VirtualMethodTable;
        use ctadl_ir::mir::{FunctionData, Functions, ParameterType, Program};

        let functions: Vec<FunctionData> = names
            .iter()
            .map(|name| {
                let mut f = FunctionData::default();
                f.set_name((*name).to_string());
                f.params.parameters.push(ParameterType::ByVal);
                f
            })
            .collect();
        ProgramInfo {
            vmt: VirtualMethodTable::Unknown,
            program: Program::new(Functions::new(functions)),
            ..Default::default()
        }
    }

    /// Runs `generators` against `program_info` with the capture on, keeping every name.
    fn capture_of(
        program_info: &ProgramInfo,
        generators: Vec<serde_json::Value>,
    ) -> (
        BTreeMap<usize, MatchedFunctions>,
        BTreeMap<usize, crate::models::PropagationStats>,
    ) {
        let mut out = ProgramModelMatches::default();
        let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
        ingest.capture_matches(usize::MAX);
        ingest.encode_models(generators).expect("encoding models");
        (
            std::mem::take(&mut ingest.matched),
            std::mem::take(&mut ingest.propagation_stats),
        )
    }

    #[test]
    fn a_narrowed_generator_agrees_with_matched_functions() {
        let program_info = native_program(&["f", "g"]);
        let (matched, _) = capture_of(
            &program_info,
            vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "f"}],
                "model": {"sources": [{"kind": "l", "port": "Return"}]},
            })],
        );
        let captured = matched.get(&0).expect("generator 0 captured");
        assert_eq!(captured.total(), Some(1));
        // The same set the matcher fanned its endpoints out over.
        assert_eq!(
            captured.names().iter().cloned().collect::<Vec<_>>(),
            vec!["f".to_string()]
        );
    }

    /// A generator with no `where` matches every function, and the capture says so as `All`
    /// rather than as a number -- including on a frontend where `matched_functions(&All)`
    /// returns an empty list. Reporting *that* as "matched 0 functions" is the count-that-lies
    /// the model check exists to prevent.
    #[test]
    fn a_where_less_generator_captures_all() {
        for program_info in [
            native_program(&["f", "g"]),
            unknown_vmt_program(&["f", "g"]),
        ] {
            let (matched, _) = capture_of(
                &program_info,
                vec![serde_json::json!({
                    "find": "methods",
                    "model": {"sources": [{"kind": "l", "port": "Return"}]},
                })],
            );
            assert_eq!(matched.get(&0), Some(&MatchedFunctions::All));
            assert_eq!(matched[&0].total(), None);
        }
    }

    /// The pcode-shaped case, spelled out: a `where`-narrowed generator on an `Unknown` VMT
    /// captures a real count even though the `All` arm of `matched_functions` could not.
    #[test]
    fn a_narrowed_generator_counts_on_an_unknown_vmt() {
        let program_info = unknown_vmt_program(&["f", "g"]);
        let (matched, _) = capture_of(
            &program_info,
            vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "f"}],
                "model": {"sinks": [{"kind": "l", "port": "Argument(0)"}]},
            })],
        );
        assert_eq!(matched[&0].total(), Some(1));
    }

    /// A propagation's two counts are the ends of one fan-out: two entries declared, and one
    /// row per (entry x matched function).
    #[test]
    fn propagation_counts_ports_and_rows() {
        let program_info = native_program(&["f", "g"]);
        let (_, propagation) = capture_of(
            &program_info,
            vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature", "pattern": "^[fg]$"}],
                "model": {"propagation": [
                    {"input": "Argument(0)", "output": "Return"},
                    {"input": "Argument(1)", "output": "Return"},
                ]},
            })],
        );
        let stats = propagation.get(&0).expect("generator 0 counted");
        assert_eq!(stats.ports_declared, 2);
        assert_eq!(stats.rows, 4);
    }

    /// Nothing is recorded unless the caller asked for it: `index` and `query` must pay
    /// nothing for a capture neither of them reads.
    #[test]
    fn capture_is_off_by_default() {
        let program_info = native_program(&["f"]);
        let mut out = ProgramModelMatches::default();
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
        ingest
            .encode_models(vec![serde_json::json!({
                "find": "methods",
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
            })])
            .expect("encoding models");
        assert!(ingest.matched.is_empty());
        assert!(ingest.propagation_stats.is_empty());
    }
}

// Tests for UniverseSet set difference (backs the `not` combinator).
mod universe_set_diff {
    use crate::models::universe_set::UniverseSet;
    use std::collections::BTreeSet;

    fn explicit<'a>(items: &[&'a str]) -> UniverseSet<&'a str> {
        items.iter().copied().collect()
    }

    fn as_set<'a>(u: &UniverseSet<&'a str>) -> BTreeSet<&'a str> {
        match u {
            UniverseSet::Explicit(s) => s.clone(),
            UniverseSet::All => panic!("expected Explicit, got All"),
        }
    }

    #[test]
    fn difference_removes_members() {
        // {a,b,c} \ {b} == {a,c}
        let mut a = explicit(&["a", "b", "c"]);
        a.difference_with(explicit(&["b"]));
        assert_eq!(as_set(&a), BTreeSet::from(["a", "c"]));
    }

    #[test]
    fn difference_with_all_is_empty() {
        // {a} \ All == {}
        let mut a = explicit(&["a"]);
        a.difference_with(UniverseSet::all());
        assert!(as_set(&a).is_empty());
    }
}
