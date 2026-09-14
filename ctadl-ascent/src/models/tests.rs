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

/// Tests for `model.modes`, the directive that makes a model *replace* a body rather than add to
/// it.
///
/// What the loader has to get right is small but load-bearing: the directive names functions (so
/// phase 2 can resolve them to ids), it is independent of whether the same generator declares a
/// propagation, and an unrecognized value is an error rather than a silently ignored key -- a
/// typo'd mode that loads clean reads exactly like a mode that did nothing.
mod modes {
    use super::*;

    /// Runs `generators` against a native program and returns everything Stage 1 emitted.
    fn matches_of(names: &[&str], generators: Vec<serde_json::Value>) -> ProgramModelMatches {
        let program_info = native_program(names);
        let mut out = ProgramModelMatches::default();
        {
            let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
            let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
            ingest.encode_models(generators).expect("encoding models");
        }
        out
    }

    /// The same `where` that selects a propagation's functions selects the directive's.
    #[test]
    fn skip_analysis_records_the_matched_functions() {
        let matches = matches_of(
            &["f", "g"],
            vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "f"}],
                "model": {
                    "modes": ["skip-analysis"],
                    "propagation": [{"input": "Argument(0)", "output": "Return"}],
                },
            })],
        );
        assert_eq!(
            matches.skip_analysis,
            [facts::Str::from("f")].into_iter().collect()
        );
        // The propagation is unaffected: `skip-analysis` removes what the *body* contributes,
        // and the model's own rows are the whole point of writing one.
        assert_eq!(matches.propagations.len(), 1);
    }

    /// A generator with `modes` and no `propagation` is the honest model for a function that
    /// moves nothing, and it is what the ARM unwinder defaults use.
    #[test]
    fn skip_analysis_needs_no_propagation() {
        let matches = matches_of(
            &["f"],
            vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "f"}],
                "model": {"modes": ["skip-analysis"]},
            })],
        );
        assert_eq!(
            matches.skip_analysis,
            [facts::Str::from("f")].into_iter().collect()
        );
        assert!(matches.propagations.is_empty());
    }

    /// An unknown mode errors. Ignoring it would produce a model file that loads clean and
    /// analyzes the body anyway -- the one failure this directive cannot afford.
    #[test]
    fn an_unknown_mode_is_an_error() {
        let program_info = native_program(&["f"]);
        let mut out = ProgramModelMatches::default();
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
        let err = ingest
            .encode_models(vec![serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "f"}],
                "model": {"modes": ["skip_analysis"]},
            })])
            .expect_err("an unknown mode is rejected");
        let crate::error::Error::JsonModel(errors) = err else {
            panic!("expected a JSON model error, got: {err}");
        };
        assert_eq!(errors.len(), 1);
        // The message names the defined value, because the reason a mode is misspelled is that
        // the writer did not know how it is spelled.
        assert!(
            format!("{}", errors[0]).contains("skip-analysis"),
            "the error should name the defined value: {}",
            errors[0]
        );
        assert!(out.skip_analysis.is_empty());
    }
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

// ---------------------------------------------------------------------------------------
// `find: "dispatch"`
// ---------------------------------------------------------------------------------------

mod dispatch {
    use super::*;
    use crate::models::match_index::DispatchKeys;
    use ctadl_ir::mir::call::{
        CallStyle, JavaClass, JavaDispatch, JavaMethod, JavaSignature, JavaSimpleName,
        VirtualMethodTable,
    };
    use ctadl_ir::mir::{
        BasicBlockData, FunctionData, Functions, Program, Statement, StatementKind, VariableRef,
    };

    /// One caller whose body dispatches on each `(cls, name, descriptor)` given. The VMT
    /// declares `Lcom/example/Impl;->next(...)`, so a `find: methods` generator has something to
    /// match and the dispatch keys can be seen to differ from it.
    fn java_program(sites: &[(&str, &str, &str)]) -> ProgramInfo {
        use ctadl_ir::mir::builder::FunctionBuilder;

        let mut caller = FunctionData {
            name: "Lcom/example/Caller;->run()V".to_string(),
            ..Default::default()
        };
        {
            let mut fb = FunctionBuilder::new(&mut caller);
            let body = fb.add_block();
            let mut b = fb.at_block(body);
            let x: VariableRef = b.new_local_var("x");
            for (cls, name, desc) in sites {
                b.create_call(
                    CallStyle::JavaCall {
                        receiver: x.clone(),
                        cls: (*cls).into(),
                        simple_name: (*name).into(),
                        descriptor: (*desc).into(),
                        dispatch: JavaDispatch::Interface,
                        super_start: None,
                    },
                    Vec::new(),
                    Vec::new(),
                );
            }
            b.create_ret(Vec::<ctadl_ir::mir::Exp>::new());
        }
        let mut impl_fn = FunctionData::default();
        impl_fn.set_name("Lcom/example/Impl;->next()Ljava/lang/Object;".to_string());
        let blocks = impl_fn.blocks.blocks_mut();
        let entry = blocks.push(BasicBlockData::new(None));
        blocks[entry].extend(vec![Statement::new_kind(StatementKind::Nop)]);

        ProgramInfo {
            vmt: VirtualMethodTable::Java {
                methods: vec![(
                    JavaClass("Lcom/example/Impl;".into()),
                    JavaSimpleName("next".into()),
                    JavaSignature("()Ljava/lang/Object;".into()),
                    JavaMethod("Lcom/example/Impl;->next()Ljava/lang/Object;".into()),
                )],
                hierarchy: Default::default(),
                interfaces: vec![JavaClass("Ljava/util/Iterator;".into())],
                abstract_methods: Vec::new(),
                natives: Vec::new(),
            },
            program: Program::new(Functions::new(vec![caller, impl_fn])),
            ..Default::default()
        }
    }

    /// Matches `generators` against a program whose call sites are `sites`.
    fn dispatch_of(
        sites: &[(&str, &str, &str)],
        generators: Vec<serde_json::Value>,
    ) -> Result<ProgramModelMatches, crate::error::Error> {
        let program_info = java_program(sites);
        let keys = DispatchKeys::from_program(&program_info.program);
        let mut out = ProgramModelMatches::default();
        {
            let match_index = ProgramMatchIndex::new_with_dispatch(
                &program_info,
                ImportScope::unknown(),
                Some(&keys),
            );
            let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
            ingest.encode_models(generators)?;
        }
        Ok(out)
    }

    fn errors_of(generators: Vec<serde_json::Value>) -> String {
        match dispatch_of(
            &[("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;")],
            generators,
        ) {
            Err(crate::error::Error::JsonModel(errors)) => errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
            other => panic!("expected a load error, got {other:?}"),
        }
    }

    fn model(propagation: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "find": "dispatch",
            "where": [{"constraint": "signature_match", "name": "next"}],
            "model": {"propagation": propagation},
        })
    }

    const ITERATOR: (&str, &str, &str) = ("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;");

    /// The point of the form: half an app's interface sites name a type it never declares, so
    /// `java.util.Iterator` has no row in the method universe and `find: methods` cannot name
    /// it. The keys of the call sites contain it by construction.
    #[test]
    fn matches_a_type_the_program_never_declares() {
        let out = dispatch_of(
            &[ITERATOR],
            vec![serde_json::json!({
                "find": "dispatch",
                "where": [{"constraint": "signature_match", "names": ["next"],
                           "parents": ["Ljava/util/Iterator;"]}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
            })],
        )
        .expect("loading");
        assert_eq!(out.dispatch.len(), 1);
        let (key, matched) = out.dispatch.iter().next().unwrap();
        assert_eq!((key.0.as_ref(), key.1.as_ref(), key.2.as_ref()), ITERATOR);
        assert!(matches!(matched.disposition, Disposition::Model(_)));
        assert_eq!(matched.provenance, vec!["<models>:0".to_string()]);
    }

    #[test]
    fn an_empty_propagation_is_a_skip() {
        let out = dispatch_of(&[ITERATOR], vec![model(serde_json::json!([]))]).expect("loading");
        assert_eq!(
            out.dispatch.values().next().map(|m| &m.disposition),
            Some(&Disposition::Skip)
        );
    }

    #[test]
    fn resolve_inline_loads() {
        let out = dispatch_of(
            &[ITERATOR],
            vec![serde_json::json!({
                "find": "dispatch",
                "where": [{"constraint": "signature_match", "name": "next"}],
                "model": {"resolve": "inline"},
            })],
        )
        .expect("loading");
        assert_eq!(
            out.dispatch.values().next().map(|m| &m.disposition),
            Some(&Disposition::Inline)
        );
    }

    /// "Forgot the model" and "meant to discard" have to be different documents, so neither key
    /// is an error and both keys is too.
    #[test]
    fn exactly_one_of_propagation_and_resolve_is_required() {
        for m in [
            serde_json::json!({}),
            serde_json::json!({"propagation": [], "resolve": "inline"}),
        ] {
            let err = errors_of(vec![serde_json::json!({
                "find": "dispatch",
                "where": [{"constraint": "signature_match", "name": "next"}],
                "model": m,
            })]);
            assert!(
                err.contains("propagation") && err.contains("resolve"),
                "{err}"
            );
        }
    }

    #[test]
    fn an_unknown_resolve_value_errors() {
        let err = errors_of(vec![serde_json::json!({
            "find": "dispatch",
            "where": [{"constraint": "signature_match", "name": "next"}],
            "model": {"resolve": "cha"},
        })]);
        assert!(err.contains("'cha'") && err.contains("'inline'"), "{err}");
    }

    /// An endpoint and a `modes` directive live on a function, and a dispatch generator has
    /// none.
    #[test]
    fn every_other_model_key_is_refused() {
        for key in [
            "sources",
            "sinks",
            "taint",
            "modes",
            "bridge",
            "access_paths",
        ] {
            let err = errors_of(vec![serde_json::json!({
                "find": "dispatch",
                "where": [{"constraint": "signature_match", "name": "next"}],
                "model": {key: []},
            })]);
            assert!(err.contains(key), "{key}: {err}");
        }
    }

    /// The constraints that need a `FunctionData`, plus `in_function`, which is refused because
    /// the policy is per signature rather than per site.
    #[test]
    fn per_function_constraints_are_refused_by_name() {
        for (constraint, extra) in [
            (
                "in_function",
                serde_json::json!({"inner": {"constraint": "name", "pattern": "x"}}),
            ),
            ("has_code", serde_json::json!({"value": true})),
            (
                "number_parameters",
                serde_json::json!({"inner": {"constraint": "==", "value": 1}}),
            ),
            ("uses_field", serde_json::json!({"name": "f"})),
        ] {
            let mut c = extra;
            c["constraint"] = serde_json::json!(constraint);
            let err = errors_of(vec![serde_json::json!({
                "find": "dispatch",
                "where": [c],
                "model": {"propagation": []},
            })]);
            assert!(err.contains(constraint), "{constraint}: {err}");
        }
    }

    /// `Inline > Model > Skip`, whichever order the generators arrive in, and both are named in
    /// the provenance so the diagnostics can say which one lost.
    #[test]
    fn dispositions_take_precedence_in_either_order() {
        let skip = model(serde_json::json!([]));
        let propagate = model(serde_json::json!([{"input": "Argument(0)", "output": "Return"}]));
        let inline = serde_json::json!({
            "find": "dispatch",
            "where": [{"constraint": "signature_match", "name": "next"}],
            "model": {"resolve": "inline"},
        });
        let pairs = [
            (skip.clone(), propagate.clone()),
            (propagate.clone(), skip.clone()),
        ];
        for (a, b) in pairs {
            let out = dispatch_of(&[ITERATOR], vec![a, b]).expect("loading");
            let matched = out.dispatch.values().next().expect("one key");
            assert!(matches!(matched.disposition, Disposition::Model(_)));
            assert_eq!(matched.provenance.len(), 2);
        }
        for (a, b) in [
            (inline.clone(), propagate.clone()),
            (propagate.clone(), inline.clone()),
        ] {
            let out = dispatch_of(&[ITERATOR], vec![a, b]).expect("loading");
            let matched = out.dispatch.values().next().expect("one key");
            assert_eq!(matched.disposition, Disposition::Inline);
            assert_eq!(matched.provenance.len(), 2);
        }
    }

    /// Two generators that both model one key union their propagation lists, as two generators
    /// matching one function do.
    #[test]
    fn two_models_on_one_key_union_their_propagations() {
        let out = dispatch_of(
            &[ITERATOR],
            vec![
                model(serde_json::json!([{"input": "Argument(0)", "output": "Return"}])),
                model(serde_json::json!([{"input": "Argument(1)", "output": "Return"}])),
            ],
        )
        .expect("loading");
        let Disposition::Model(ports) = &out.dispatch.values().next().unwrap().disposition else {
            panic!("expected a model");
        };
        assert_eq!(ports.len(), 2);
    }

    /// A dispatch generator narrows the call sites' signatures, and a method generator the
    /// program's implementations. The same `where` selects different things.
    #[test]
    fn the_two_universes_are_separate() {
        let out = dispatch_of(
            &[ITERATOR],
            vec![
                model(serde_json::json!([{"input": "Argument(0)", "output": "Return"}])),
                serde_json::json!({
                    "find": "methods",
                    "where": [{"constraint": "signature_match", "name": "next"}],
                    "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
                }),
            ],
        )
        .expect("loading");
        assert_eq!(out.dispatch.len(), 1, "the site's signature");
        assert_eq!(out.propagations.len(), 1, "the implementation");
        assert_eq!(
            out.propagations[0].function.as_ref(),
            "Lcom/example/Impl;->next()Ljava/lang/Object;"
        );
    }

    /// Without a dispatch universe the generator matches nothing, rather than everything: the
    /// caller decided not to collect the call sites, and an unnarrowed working set would model
    /// every signature in the program.
    #[test]
    fn no_dispatch_universe_matches_nothing() {
        let program_info = java_program(&[ITERATOR]);
        let mut out = ProgramModelMatches::default();
        {
            let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
            let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
            ingest
                .encode_models(vec![model(serde_json::json!([
                    {"input": "Argument(0)", "output": "Return"}
                ]))])
                .expect("loading");
        }
        assert!(out.dispatch.is_empty());
    }
}
