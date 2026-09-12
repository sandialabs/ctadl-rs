//! The model DSL engine, against real programs.
//!
//! `models/dsl/tests.rs` covers syntax, checking and migration, none of which needs an artifact.
//! Everything here needs one: the built-in relations are a view of a program, so what `fun`,
//! `param`, `callsite`, `subclass` and `uses_field` actually *contain* can only be pinned by
//! matching against one.

use std::collections::BTreeSet;

use ctadl_ascent::facts::TaintDirection;
use ctadl_ascent::models::{
    FormalIndexTypeTag, ImportScope, ProgramMatchIndex, ProgramModelMatches, dsl,
};
use ctadl_ir::Idx;
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::call::{
    CallEdges, CallStyle, JavaClass, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction,
    NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
};
use ctadl_ir::mir::{
    AccessPath, BasicBlockData, FieldPath, FunctionData, Functions, ParameterType, Program,
    Statement, StatementKind, VariableRef,
};
use ctadl_ir::mir::{LocalIdx, Symbol};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A function with `params` parameters and a body holding `statements`.
fn function(name: &str, params: usize, statements: Vec<Statement>) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    for _ in 0..params {
        f.params.parameters.push(ParameterType::ByVal);
    }
    let blocks = f.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(None));
    blocks[body].extend(if statements.is_empty() {
        vec![Statement::new_kind(StatementKind::Nop)]
    } else {
        statements
    });
    f
}

/// A direct call to `callee`, which is what `callsite`'s `callee_string` reads.
fn call(callee: &str) -> Statement {
    Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(std::iter::once(callee.to_string()).collect()),
        },
        rets: Default::default(),
        args: Default::default(),
    })
}

/// A `Load` of `field`, which is what `uses_field` reads.
fn load(field: &str) -> Statement {
    Statement::new_kind(StatementKind::Load {
        dest: VariableRef::new_local_idx(LocalIdx::new(0)),
        source: AccessPath::without_fields(VariableRef::new_local_idx(LocalIdx::new(1))),
        field: FieldPath::symbol(field),
    })
}

/// A Java program: `Lc/App;->handle()V` calls `Lc/Log;->emit(…)V`, `Lc/Sub;` extends `Lc/Base;`.
fn java_program() -> ProgramInfo {
    let methods = vec![
        (
            JavaClass("Lc/App;".into()),
            JavaSimpleName("handle".into()),
            JavaSignature("()V".into()),
            JavaMethod("Lc/App;->handle()V".into()),
        ),
        (
            JavaClass("Lc/Log;".into()),
            JavaSimpleName("emit".into()),
            JavaSignature("(Ljava/lang/String;)V".into()),
            JavaMethod("Lc/Log;->emit(Ljava/lang/String;)V".into()),
        ),
        (
            JavaClass("Lc/Sub;".into()),
            JavaSimpleName("run".into()),
            JavaSignature("()V".into()),
            JavaMethod("Lc/Sub;->run()V".into()),
        ),
    ];
    let mut hierarchy: hashbrown::HashMap<JavaClass, smallvec::SmallVec<[JavaClass; 2]>> =
        Default::default();
    hierarchy.insert(
        JavaClass("Lc/Sub;".into()),
        std::iter::once(JavaClass("Lc/Base;".into())).collect(),
    );
    ProgramInfo {
        vmt: VirtualMethodTable::Java {
            methods,
            hierarchy,
            natives: Vec::new(),
        },
        program: Program::new(Functions::new(vec![
            function(
                "Lc/App;->handle()V",
                1,
                vec![call("Lc/Log;->emit(Ljava/lang/String;)V"), load("token")],
            ),
            function("Lc/Log;->emit(Ljava/lang/String;)V", 2, vec![]),
            function("Lc/Sub;->run()V", 3, vec![]),
        ])),
        ..Default::default()
    }
}

/// A native program with one 2-parameter function per name.
fn native_program(names: &[&str]) -> ProgramInfo {
    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: names
                .iter()
                .map(|name| {
                    (
                        NativeSimpleName((*name).into()),
                        NativeSignature(format!("(int, char *) {name}").into()),
                        NativeFunction(format!("<EXTERNAL>::{name}@00101008").into()),
                        NativeQualifiedName(format!("<EXTERNAL>::{name}").into()),
                    )
                })
                .collect(),
        },
        program: Program::new(Functions::new(
            names
                .iter()
                .map(|n| function(&format!("<EXTERNAL>::{n}@00101008"), 2, vec![]))
                .collect::<Vec<_>>(),
        )),
        ..Default::default()
    }
}

/// Runs `source` against `program_info` and returns everything it matched.
fn run(program_info: &ProgramInfo, source: &str) -> ProgramModelMatches {
    run_scoped(&[(ImportScope::unknown(), program_info)], source)
}

/// Runs `source` against several imports in one accumulator, the way `ctadl index` does.
fn run_scoped(imports: &[(ImportScope, &ProgramInfo)], source: &str) -> ProgramModelMatches {
    let file = match dsl::DslFile::from_text("test.ctadl", source) {
        Ok(f) => f,
        Err(e) => panic!("{e}"),
    };
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set);
    for (scope, program_info) in imports {
        let index = ProgramMatchIndex::new(program_info, scope.clone());
        matcher.observe_import(&index);
    }
    let mut out = ProgramModelMatches::default();
    matcher
        .finish(dsl::Phase::All, &mut out)
        .unwrap_or_else(|e| panic!("{e}"));
    out
}

fn endpoint_functions(matches: &ProgramModelMatches) -> BTreeSet<String> {
    matches
        .endpoints
        .iter()
        .map(|e| e.function.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// The relations
// ---------------------------------------------------------------------------

#[test]
fn fun_matches_on_name_and_parent() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, name = "emit", parent = "Lc/Log;");"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["Lc/Log;->emit(Ljava/lang/String;)V".to_string()])
    );
}

/// The design's `parent in {...}` example, which is the whole point of the set syntax.
#[test]
fn a_parent_set_is_a_union() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, parent in {"Lc/Log;", "Lc/Sub;"});"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from([
            "Lc/Log;->emit(Ljava/lang/String;)V".to_string(),
            "Lc/Sub;->run()V".to_string(),
        ])
    );
}

#[test]
fn arity_and_has_code_read_the_ir() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, arity > 2, has_code = true);"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["Lc/Sub;->run()V".to_string()])
    );
}

/// The design's `sink(F::arg(I)) :- fun(F), param(F, I)` — a port index bound in the body.
#[test]
fn param_expands_a_port_over_the_arity() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"sink(F::arg(I)) :- fun(F, name = "run"), param(F, I);"#,
    );
    let mut indices: Vec<i16> = matches.endpoints.iter().filter_map(|e| e.index).collect();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1, 2]);
    assert!(
        matches
            .endpoints
            .iter()
            .all(|e| e.selector_ty == FormalIndexTypeTag::Index)
    );
}

#[test]
fn callsite_joins_a_caller_to_its_callee() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"sink(S::arg(0)) :-
             callsite(C, S, callee_string = F),
             fun(F, name = "emit"),
             fun(C, name = "handle");"#,
    );
    assert_eq!(matches.endpoints.len(), 1);
    let endpoint = &matches.endpoints[0];
    // A port anchored at a site names the callee, restricted to that caller: that is exactly
    // the shape `find: callsites` with an `in_function` produced.
    assert_eq!(
        endpoint.function.to_string(),
        "Lc/Log;->emit(Ljava/lang/String;)V"
    );
    assert_eq!(
        endpoint.in_function.map(|f| f.to_string()),
        Some("Lc/App;->handle()V".to_string())
    );
    assert!(endpoint.callsite_scoped);
}

#[test]
fn uses_field_reads_loads_and_stores() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- uses_field(F, "token");"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["Lc/App;->handle()V".to_string()])
    );
    let none = run(
        &program_info,
        r#"source(F::return) :- uses_field(F, "absent");"#,
    );
    assert!(none.endpoints.is_empty());
}

#[test]
fn subclass_reads_the_hierarchy() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, parent = P), subclass(P, "Lc/Base;");"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["Lc/Sub;->run()V".to_string()])
    );
}

/// `subclass*` is reflexive, `subclass+` is not. A one-level hierarchy is enough to tell them
/// apart, and getting them backwards is the kind of thing that silently widens a model.
#[test]
fn the_two_closures_differ_on_the_reflexive_pair() {
    let program_info = java_program();
    let reflexive = run(
        &program_info,
        r#"source(F::return) :- fun(F, parent = P), subclass*(P, "Lc/Sub;");"#,
    );
    assert_eq!(
        endpoint_functions(&reflexive),
        BTreeSet::from(["Lc/Sub;->run()V".to_string()])
    );
    let strict = run(
        &program_info,
        r#"source(F::return) :- fun(F, parent = P), subclass+(P, "Lc/Sub;");"#,
    );
    assert!(strict.endpoints.is_empty());
}

/// A native symbol is published under a simple name and a decorated one. A model naming either
/// must match, which is what keeps a migrated `name` regex meaning what it meant.
#[test]
fn a_native_symbol_matches_both_of_its_spellings() {
    let program_info = native_program(&["system"]);
    for pattern in [
        r#"fun(F, name = "system")"#,
        r#"fun(F, name = "<EXTERNAL>::system@00101008")"#,
        r#"fun(F, qualified-id = "<EXTERNAL>::system")"#,
    ] {
        let matches = run(&program_info, &format!("sink(F::arg(0)) :- {pattern};"));
        assert_eq!(matches.endpoints.len(), 1, "{pattern} matched nothing");
    }
    let regex = run(
        &program_info,
        r#"sink(F::arg(0)) :- fun(F, name = N), regex_match(N, "^system$");"#,
    );
    assert_eq!(regex.endpoints.len(), 1);
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

#[test]
fn negation_excludes_what_the_inner_atom_matches() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F), !fun(F, parent = "Lc/Log;");"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from([
            "Lc/App;->handle()V".to_string(),
            "Lc/Sub;->run()V".to_string(),
        ])
    );
}

/// A negated group quantifies its own locals. Without that, "no name of F matches" would
/// decompose into two independent tests and mean something else.
#[test]
fn a_negated_group_is_one_existence_check() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F), !(fun(F, name = N) && regex_match(N, "^e"));"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from([
            "Lc/App;->handle()V".to_string(),
            "Lc/Sub;->run()V".to_string(),
        ])
    );
}

#[test]
fn boolean_combinations_filter_bound_variables() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, arity = A), (A = 1 || A = 3);"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from([
            "Lc/App;->handle()V".to_string(),
            "Lc/Sub;->run()V".to_string(),
        ])
    );
}

#[test]
fn a_set_test_in_atom_position_filters() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, name = N), N in {"emit", "run"};"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from([
            "Lc/Log;->emit(Ljava/lang/String;)V".to_string(),
            "Lc/Sub;->run()V".to_string(),
        ])
    );
}

// ---------------------------------------------------------------------------
// Heads
// ---------------------------------------------------------------------------

#[test]
fn several_heads_fire_from_one_body() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return, kind = "UserInput"), propagation(F::return <- F::arg(0)) :-
             fun(F, name = "emit");"#,
    );
    assert_eq!(matches.endpoints.len(), 1);
    assert_eq!(matches.endpoints[0].label.to_string(), "UserInput");
    assert_eq!(matches.propagations.len(), 1);
    assert_eq!(matches.propagations[0].dst.tag, FormalIndexTypeTag::Return);
    assert_eq!(matches.propagations[0].src.index, Some(0));
}

#[test]
fn a_bidirectional_propagation_is_two_rows() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"propagation(F::arg(2).foo <-> F::arg(0).bar) :- fun(F, name = "emit");"#,
    );
    assert_eq!(matches.propagations.len(), 2);
    let paths: BTreeSet<String> = matches
        .propagations
        .iter()
        .map(|p| {
            format!(
                "{}<-{}",
                p.dst.path.to_dot_string(),
                p.src.path.to_dot_string()
            )
        })
        .collect();
    assert_eq!(
        paths,
        BTreeSet::from([".foo<-.bar".to_string(), ".bar<-.foo".to_string()])
    );
}

#[test]
fn source_and_sink_flags_reach_the_match() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return, saturating = true), sink(F::arg(0), wildcard = false) :-
             fun(F, name = "emit");"#,
    );
    let source = matches
        .endpoints
        .iter()
        .find(|e| e.direction == TaintDirection::Forward)
        .expect("a source");
    assert!(source.saturating);
    assert!(!source.wildcard);
    let sink = matches
        .endpoints
        .iter()
        .find(|e| e.direction == TaintDirection::Backward)
        .expect("a sink");
    assert!(!sink.wildcard);
    assert!(!sink.saturating);
    // `wildcard` defaults to true, which is the existing sink semantics.
    let defaulted = run(
        &program_info,
        r#"sink(F::arg(0)) :- fun(F, name = "emit");"#,
    );
    assert!(defaulted.endpoints[0].wildcard);
}

#[test]
fn a_bare_head_registers_an_access_path() {
    let program_info = java_program();
    let matches = run(&program_info, r#"access_paths(".next.next");"#);
    assert_eq!(matches.access_paths.len(), 1);
    assert_eq!(
        matches
            .access_paths
            .iter()
            .next()
            .expect("one path")
            .to_dot_string(),
        ".next.next"
    );
}

#[test]
fn arg_wildcard_stays_a_tag_for_phase_two() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"propagation(F::return <- F::arg(_)) :- fun(F, name = "emit");"#,
    );
    assert_eq!(
        matches.propagations[0].src.tag,
        FormalIndexTypeTag::AnyArgument
    );
    assert_eq!(matches.propagations[0].src.index, None);
}

/// A grounding binds every variable in the rule, so a head that reads only some of them must
/// still fire once. Without the projection this emits one source per parameter.
#[test]
fn a_head_fires_once_per_distinct_binding_of_its_own_variables() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"source(F::return) :- fun(F, name = "run"), param(F, I);"#,
    );
    assert_eq!(matches.endpoints.len(), 1, "{:?}", matches.endpoints);
}

// ---------------------------------------------------------------------------
// Bridging across imports
// ---------------------------------------------------------------------------

/// The design's motivating case: one rule names a callee in a Lua import and its implementation
/// in a pcode one. No single import satisfies the body, so this only works because the body's
/// components are accumulated separately and joined after the loop.
#[test]
fn a_bridge_rule_spans_two_imports() {
    use ctadl_ascent::project::ArtifactLanguage;

    let lua = native_program(&["luaCallNativeAdd"]);
    let native = native_program(&["luaNativeAdd"]);
    let matches = run_scoped(
        &[
            (ImportScope::new(ArtifactLanguage::Lua, "app"), &lua),
            (ImportScope::new(ArtifactLanguage::Pcode, "lib"), &native),
        ],
        r#"bridge(F::arg(0) -> G::arg(0).stack[1]),
           bridge(F::return <- G::arg(0).stack[2]) :-
             fun(F, name = "luaCallNativeAdd", language = "lua"),
             fun(G, name = "luaNativeAdd", language = "pcode");"#,
    );
    assert_eq!(matches.resolved_bridges.len(), 1);
    let bridge = &matches.resolved_bridges[0];
    assert_eq!(
        bridge.from.to_string(),
        "<EXTERNAL>::luaCallNativeAdd@00101008"
    );
    assert_eq!(bridge.to.to_string(), "<EXTERNAL>::luaNativeAdd@00101008");
    // Two heads over one (from, to) pair make one bridge with two ports, not two bridges.
    assert_eq!(bridge.ports.len(), 2);
    assert_eq!(
        bridge.ports[0].direction,
        ctadl_ascent::models::Direction::In
    );
    assert_eq!(
        bridge.ports[1].direction,
        ctadl_ascent::models::Direction::Out
    );
}

/// The `language` and `import` attributes are what scope a rule to one artifact — the DSL's
/// counterpart of the JSON `in` block.
#[test]
fn language_scopes_a_rule_to_one_import() {
    use ctadl_ascent::project::ArtifactLanguage;

    let lua = native_program(&["shared"]);
    let native = native_program(&["shared"]);
    let matches = run_scoped(
        &[
            (ImportScope::new(ArtifactLanguage::Lua, "app"), &lua),
            (ImportScope::new(ArtifactLanguage::Pcode, "lib"), &native),
        ],
        r#"source(F::return) :- fun(F, name = "shared", language = "pcode");"#,
    );
    // Both imports hold a function of this name, and the two programs name it identically, so
    // what is being pinned is that the rule fired once — for the pcode import only.
    assert_eq!(matches.endpoints.len(), 1);
    let by_import = run_scoped(
        &[
            (ImportScope::new(ArtifactLanguage::Lua, "app"), &lua),
            (ImportScope::new(ArtifactLanguage::Pcode, "lib"), &native),
        ],
        r#"source(F::return) :- fun(F, name = "shared", import = "app");"#,
    );
    assert_eq!(by_import.endpoints.len(), 1);
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

/// Each phase keeps its own heads, and reports how many rules it ignored. A rule contributing
/// at least one head to the running phase is never counted.
#[test]
fn each_phase_keeps_its_own_heads_and_counts_the_rest() {
    let program_info = java_program();
    let source = r#"
        source(F::return) :- fun(F, name = "emit");
        propagation(F::return <- F::arg(0)) :- fun(F, name = "emit");
        source(F::return), propagation(F::return <- F::arg(0)) :- fun(F, name = "run");
    "#;
    let file = dsl::DslFile::from_text("test.ctadl", source).expect("loads");
    let set = dsl::DslModelSet { files: vec![file] };

    for (phase, endpoints, summaries, skipped) in [
        (dsl::Phase::Index, 0usize, 2usize, 1usize),
        (dsl::Phase::Query, 2usize, 0usize, 1usize),
        (dsl::Phase::All, 2usize, 2usize, 0usize),
    ] {
        let mut matcher = dsl::DslMatcher::new(&set);
        let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        matcher.observe_import(&index);
        let mut out = ProgramModelMatches::default();
        let report = matcher.finish(phase, &mut out).expect("finishes");
        assert_eq!(out.endpoints.len(), endpoints, "{phase:?}");
        assert_eq!(out.propagations.len(), summaries, "{phase:?}");
        assert_eq!(report.skipped_rules(), skipped, "{phase:?}");
    }
}

/// A rule the phase skipped is reported as *skipped*, never as *matched nothing*. The two are
/// different problems, and only one of them is a problem.
#[test]
fn a_skipped_rule_is_not_reported_as_dead() {
    let program_info = java_program();
    let file = dsl::DslFile::from_text(
        "test.ctadl",
        r#"source(F::return) :- fun(F, name = "emit");"#,
    )
    .expect("loads");
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index);
    let mut out = ProgramModelMatches::default();
    let report = matcher
        .finish(dsl::Phase::Index, &mut out)
        .expect("finishes");
    assert_eq!(report.skipped_rules(), 1);
    assert!(report.dead_rules().is_empty(), "{:?}", report.dead_rules());
    assert!(report.phase_warning().is_some());
}

#[test]
fn a_rule_that_matches_nothing_is_reported() {
    let program_info = java_program();
    let file = dsl::DslFile::from_text(
        "test.ctadl",
        r#"source(F::return) :- fun(F, name = "absent");"#,
    )
    .expect("loads");
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index);
    let mut out = ProgramModelMatches::default();
    let report = matcher
        .finish(dsl::Phase::Query, &mut out)
        .expect("finishes");
    assert_eq!(report.dead_rules(), vec!["test.ctadl:0".to_string()]);
}

// ---------------------------------------------------------------------------
// Loading through the ordinary entry point
// ---------------------------------------------------------------------------

#[test]
fn a_ctadl_file_loads_through_try_load_models() {
    use ctadl_ascent::models::try_load_models;

    let program_info = java_program();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("models.ctadl");
    std::fs::write(&path, r#"sink(F::arg(0)) :- fun(F, name = "emit");"#).expect("write");

    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut out = ProgramModelMatches::default();
    let report = try_load_models(&index, &path, &mut out).expect("load");
    assert_eq!(out.endpoints.len(), 1);
    // The DSL report is projected onto the counters every diagnostic surface reads, with a rule
    // index standing in for a generator index.
    assert!(report.dsl.is_some());
    let stats = report
        .endpoint_stats
        .get(&(0, TaintDirection::Backward))
        .expect("a backward entry for rule 0");
    assert_eq!(stats.ports_declared, 1);
    assert_eq!(stats.endpoints_matched, 1);
}

#[test]
fn a_malformed_ctadl_file_fails_the_load_naming_the_line() {
    use ctadl_ascent::models::try_load_models;

    let program_info = java_program();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("models.ctadl");
    std::fs::write(
        &path,
        "sink(F::arg(0)) :- fun(F, name = \"emit\");\nsink(G::arg(0)) :- fun(F);\n",
    )
    .expect("write");

    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut out = ProgramModelMatches::default();
    let err = try_load_models(&index, &path, &mut out).expect_err("should fail");
    let text = format!("{err}");
    assert!(text.contains(":2:"), "{text}");
    assert!(text.contains("'G' appears in the head"), "{text}");
}

/// The `.ctadl` and `.dl` extensions both select the engine; anything else goes to the JSON
/// loader, which is what keeps the files already in the wild working.
#[test]
fn the_extension_selects_the_loader() {
    use std::path::Path;
    assert!(ctadl_ascent::models::is_dsl_path(Path::new("m.ctadl")));
    assert!(ctadl_ascent::models::is_dsl_path(Path::new("m.dl")));
    assert!(!ctadl_ascent::models::is_dsl_path(Path::new("m.jsonl")));
    assert!(!ctadl_ascent::models::is_dsl_path(Path::new("m.json5")));
}

/// Symbol placement is a real hazard: a wrongly-encoded segment parses fine and matches
/// nothing. Pin that the DSL's quoting reaches the same `facts::Path` the JSON escape does.
#[test]
fn a_quoted_segment_decodes_to_the_symbol_it_names() {
    let program_info = java_program();
    let matches = run(
        &program_info,
        r#"propagation(F::return <- F::arg(0)."[]") :- fun(F, name = "emit");"#,
    );
    let path = matches.propagations[0].src.path;
    assert_eq!(
        path.iter().cloned().collect::<Vec<_>>(),
        vec![ctadl_ir::mir::PathSegment::Symbol(Symbol::from("[]"))]
    );
}

/// The design's callsite-anchored bridge example. A bridge attaches *inside* a function, so a
/// site anchor is a load-time error naming the way to write it — not a silent degradation to
/// the function-anchored form, which would bridge every call site while looking like it bridged
/// one.
#[test]
fn a_callsite_anchored_bridge_says_how_to_write_it() {
    let result = dsl::DslFile::from_text(
        "test.ctadl",
        r#"S::bridge(arg(1).baz -> G::arg(0).stack[2]) :-
             callsite(_, S, callee_string = F),
             fun(F, name = "luaCallNativeAdd", language = "lua"),
             fun(G, name = "luaNativeAdd", language = "pcode");"#,
    );
    let text = match result {
        Ok(_) => panic!("a site cannot anchor a bridge"),
        Err(e) => format!("{e}"),
    };
    assert!(text.contains("attaches inside a function"), "{text}");
    assert!(text.contains("callee_string = F"), "{text}");
}
