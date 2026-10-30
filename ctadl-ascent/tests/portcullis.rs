//! The Portcullis engine, against real programs.
//!
//! `models/portcullis/tests.rs` covers syntax, checking and migration, none of which needs an
//! artifact. Everything here needs one: the built-in relations are a view of a program, so what
//! `fun`, `param`, `local`, `callsite`, `subclass` and `uses_field` actually *contain* can only be
//! pinned by matching against one.

use std::collections::BTreeSet;

use ctadl_ascent::facts::TaintDirection;
use ctadl_ascent::models::test_vmt;
use ctadl_ascent::models::{
    FormalIndexTypeTag, ImportScope, ProgramMatchIndex, ProgramModelMatches, portcullis,
};
use ctadl_ir::Idx;
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::call::{
    CallEdges, CallStyle, Dispatch, JavaClass, JavaMethod, JavaSignature, JavaSimpleName,
    VirtualMethodTable,
};
use ctadl_ir::mir::{
    AccessPath, BasicBlockData, FieldRef, FunctionData, Functions, ParameterType, Program,
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
        f.params.parameters.push(ParameterType::SharedFields);
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
        field: FieldRef::symbol(field),
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
        vmt: test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
            methods,
            hierarchy,
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        })),
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
        vmt: test_vmt::native(
            names
                .iter()
                .map(|name| {
                    (
                        format!("(int, char *) {name}"),
                        format!("<EXTERNAL>::{name}@00101008"),
                        format!("<EXTERNAL>::{name}"),
                    )
                })
                .collect::<Vec<_>>()
                .iter()
                .zip(names)
                .map(|((sig, fq, qualified), name)| {
                    (*name, sig.as_str(), fq.as_str(), qualified.as_str())
                }),
        ),
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
    let file = match portcullis::PortcullisFile::from_text("test.ctadl", source) {
        Ok(f) => f,
        Err(e) => panic!("{e}"),
    };
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::All);
    for (scope, program_info) in imports {
        let index = ProgramMatchIndex::new(program_info, scope.clone());
        matcher.observe_import(&index, &mut out);
    }
    matcher.finish(&mut out).unwrap_or_else(|e| panic!("{e}"));
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

/// `parent in {...}` is a union over the set, which is the whole point of the set syntax.
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

/// `sink(F::arg(I)) :- fun(F), param(F, I)` — a port index bound in the body.
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
    // the shape `find: callsites` with an `in_function` migrates to.
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

/// A native symbol has one simple name, its IR name, and other names: the namespace-qualified
/// one, here. `name` is the simple name alone, the IR name is `fun`'s column, and `name_of`
/// ranges over the simple name and the others.
#[test]
fn a_native_symbol_has_one_simple_name_and_others() {
    let program_info = native_program(&["system"]);
    for (body, matched) in [
        (r#"fun(F, name = "system")"#, 1),
        (r#"fun(F, name = "<EXTERNAL>::system@00101008")"#, 0),
        (r#"fun(F), F = "<EXTERNAL>::system@00101008""#, 1),
        (r#"name_of(F, "system")"#, 1),
        (r#"name_of(F, "<EXTERNAL>::system")"#, 1),
        (r#"name_of(F, "<EXTERNAL>::system@00101008")"#, 0),
    ] {
        let matches = run(&program_info, &format!("sink(F::arg(0)) :- {body};"));
        assert_eq!(matches.endpoints.len(), matched, "{body}");
    }
}

/// An attribute has one value, so testing it in the atom and testing the variable it binds are
/// the same test.
#[test]
fn an_attribute_test_and_a_test_of_its_variable_agree() {
    let program_info = native_program(&["system"]);
    for body in [
        r#"fun(F, name != "system")"#,
        r#"fun(F, name = N), N != "system""#,
    ] {
        let matches = run(&program_info, &format!("sink(F::arg(0)) :- {body};"));
        assert!(matches.endpoints.is_empty(), "{body}");
    }
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
    // `wildcard` defaults to true.
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

/// One rule names a callee in a Lua import and its implementation in a pcode one. No single import satisfies the body, so this only works because the body's
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
    let file = portcullis::PortcullisFile::from_text("test.ctadl", source).expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };

    for (phase, endpoints, summaries, skipped) in [
        (portcullis::Phase::Index, 0usize, 2usize, 1usize),
        (portcullis::Phase::Query, 2usize, 0usize, 1usize),
        (portcullis::Phase::All, 2usize, 2usize, 0usize),
    ] {
        let mut out = ProgramModelMatches::default();
        let mut matcher = portcullis::PortcullisMatcher::new(&set, phase);
        let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        matcher.observe_import(&index, &mut out);
        let report = matcher.finish(&mut out).expect("finishes");
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
    let file = portcullis::PortcullisFile::from_text(
        "test.ctadl",
        r#"source(F::return) :- fun(F, name = "emit");"#,
    )
    .expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Index);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index, &mut out);
    let report = matcher.finish(&mut out).expect("finishes");
    assert_eq!(report.skipped_rules(), 1);
    assert!(report.dead_rules().is_empty(), "{:?}", report.dead_rules());
    assert!(report.phase_warning().is_some());
}

/// A flowy program, whose locals keep their source names.
fn flowy_program() -> ProgramInfo {
    const PROGRAM: &str = r#"
def reader(buf):1 {
s:
  x = buf;
  y = x;
  return y;
}

def writer(dst):1 {
s:
  z = dst;
  return z;
}
"#;
    ctadl_flowy::compile_program_contents("test.tnt", PROGRAM)
        .expect("compiling flowy program")
        .program_info
}

/// The names of `function`'s locals, by `LocalIdx`.
fn local_names(program_info: &ProgramInfo, function: &str) -> Vec<String> {
    let data = program_info
        .program
        .functions
        .iter()
        .find(|f| f.name == function)
        .unwrap_or_else(|| panic!("no function {function}"));
    data.locals
        .iter_enumerated()
        .map(|(_, d)| d.name.clone())
        .collect()
}

#[test]
fn local_lists_every_local_and_a_port_names_one() {
    let program_info = flowy_program();
    let names = local_names(&program_info, "reader");

    let out = run(
        &program_info,
        r#"source(F::local(L).f) :- fun(F, name = "reader"), local(F, L, name = "x");"#,
    );
    assert_eq!(out.endpoints.len(), 1);
    let e = &out.endpoints[0];
    assert_eq!(e.selector_ty, FormalIndexTypeTag::Local);
    assert_eq!(e.index, None);
    let x = names
        .iter()
        .position(|n| n == "x")
        .expect("reader declares x");
    assert_eq!(e.local_index, Some(x as u32));
    assert_eq!(e.path.to_string(), ".f");

    // Every local of the function is a row, each a port of its own.
    let out = run(
        &program_info,
        r#"sink(F::local(L)) :- fun(F, name = "reader"), local(F, L);"#,
    );
    let mut got: Vec<u32> = out.endpoints.iter().filter_map(|e| e.local_index).collect();
    got.sort_unstable();
    assert_eq!(got, (0..names.len() as u32).collect::<Vec<_>>());

    // The name is an ordinary attribute: tested, bound, matched on.
    let out = run(
        &program_info,
        r#"sink(F::local(L)) :- fun(F), local(F, L, name = N), regex_match(N, "^[xz]$");"#,
    );
    assert_eq!(endpoint_functions(&out).len(), 2);
    assert_eq!(out.endpoints.len(), 2);
}

#[test]
fn a_local_port_is_anchored_at_its_own_function() {
    let program_info = flowy_program();
    let file = portcullis::PortcullisFile::from_text(
        "test.ctadl",
        r#"source(G::local(L)) :- fun(F, name = "reader"), local(F, L, name = "x"),
                                 fun(G, name = "writer");"#,
    )
    .expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Query);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index, &mut out);
    let err = matcher
        .finish(&mut out)
        .expect_err("a local of another function");
    let err = format!("{err:#}");
    assert!(err.contains("'L' is a local of 'reader'"), "{err}");
    assert!(out.endpoints.is_empty());
}

/// A `local` atom that ran and matched nothing is what the rule reports, with the functions it
/// was asked about; one that never ran is not.
#[test]
fn a_local_that_matches_nothing_is_reported() {
    let program_info = flowy_program();
    let file = portcullis::PortcullisFile::from_text(
        "test.ctadl",
        r#"source(F::local(L)) :- fun(F), local(F, L, name = "absent");
           source(F::local(L)) :- fun(F, name = "absent"), local(F, L);
           source(F::local(L)) :- fun(F), local(F, L, name = "x");"#,
    )
    .expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Query);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index, &mut out);
    let report = matcher.finish(&mut out).expect("finishes");
    let rules = &report.files[0].rules;
    let functions = rules[0].missed_locals[0].1;
    assert!(functions >= 2, "every function was asked: {functions}");
    assert_eq!(
        rules[0].missed_locals,
        vec![("absent".to_string(), functions)]
    );
    assert!(rules[1].missed_locals.is_empty());
    assert!(rules[2].missed_locals.is_empty());
}

#[test]
fn a_rule_that_matches_nothing_is_reported() {
    let program_info = java_program();
    let file = portcullis::PortcullisFile::from_text(
        "test.ctadl",
        r#"source(F::return) :- fun(F, name = "absent");"#,
    )
    .expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Query);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    matcher.observe_import(&index, &mut out);
    let report = matcher.finish(&mut out).expect("finishes");
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
    assert!(report.portcullis.is_some());
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

/// The `.ctadl` and `.dl` extensions are read as written; anything else is a JSON model file and
/// is migrated first, which is what keeps the files already in the wild working.
#[test]
fn the_extension_selects_the_loader() {
    use std::path::Path;
    assert!(ctadl_ascent::models::is_portcullis_path(Path::new(
        "m.ctadl"
    )));
    assert!(ctadl_ascent::models::is_portcullis_path(Path::new("m.dl")));
    assert!(!ctadl_ascent::models::is_portcullis_path(Path::new(
        "m.jsonl"
    )));
    assert!(!ctadl_ascent::models::is_portcullis_path(Path::new(
        "m.json5"
    )));
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

/// A bridge attaches *inside* a function, so a call-site anchor is a load-time error naming the
/// way to write it — not a silent degradation to the function-anchored form, which would bridge
/// every call site while looking like it bridged one.
#[test]
fn a_callsite_anchored_bridge_says_how_to_write_it() {
    let result = portcullis::PortcullisFile::from_text(
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

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// A Java call site, which is what `callsig` ranges over.
fn java_call(cls: &str, name: &str, desc: &str) -> Statement {
    Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::JavaCall {
            receiver: VariableRef::new_local_idx(LocalIdx::new(0)),
            cls: Symbol::from(cls),
            simple_name: Symbol::from(name),
            descriptor: Symbol::from(desc),
            dispatch: ctadl_ir::mir::call::JavaDispatch::Interface,
            super_start: None,
        },
        rets: Default::default(),
        args: Default::default(),
    })
}

/// A program whose one function calls `Iterator.next`, `Collection.size` and `Runnable.run`.
///
/// None of the three is *declared* here: that is the point of the relation. `Ljava/util/Iterator;`
/// has no row in any app's method table, so `fun` cannot name it and `callsig` must.
fn dispatch_program() -> ProgramInfo {
    ProgramInfo {
        vmt: test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
            methods: Vec::new(),
            hierarchy: Default::default(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        })),
        program: Program::new(Functions::new(vec![function(
            "Lc/App;->handle()V",
            1,
            vec![
                java_call("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;"),
                java_call("Ljava/util/Collection;", "size", "()I"),
                java_call("Ljava/lang/Runnable;", "run", "()V"),
            ],
        )])),
        ..Default::default()
    }
}

/// Runs `source` with the dispatch universe collected, which `callsig` needs.
fn run_dispatch(program_info: &ProgramInfo, source: &str) -> ProgramModelMatches {
    let file = match portcullis::PortcullisFile::from_text("test.ctadl", source) {
        Ok(f) => f,
        Err(e) => panic!("{e}"),
    };
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::All);
    let keys = ctadl_ascent::models::DispatchKeys::from_program(&program_info.program);
    let index =
        ProgramMatchIndex::new_with_dispatch(program_info, ImportScope::unknown(), Some(&keys));
    matcher.observe_import(&index, &mut out);
    matcher.finish(&mut out).unwrap_or_else(|e| panic!("{e}"));
    out
}

fn key(
    cls: &str,
    name: &str,
    desc: &str,
) -> (
    ctadl_ascent::facts::Str,
    ctadl_ascent::facts::Str,
    ctadl_ascent::facts::Str,
) {
    (
        ctadl_ascent::facts::Str::from(cls),
        ctadl_ascent::facts::Str::from(name),
        ctadl_ascent::facts::Str::from(desc),
    )
}

#[test]
fn callsig_names_types_the_program_never_declares() {
    let out = run_dispatch(
        &dispatch_program(),
        r#"K::dispatch(resolve = "skip") :- callsig(K, parent = "Ljava/util/Iterator;");"#,
    );
    // The VMT above declares no methods at all, so `fun` has nothing: only the call sites do.
    assert_eq!(
        out.dispatch.keys().collect::<Vec<_>>(),
        vec![&key("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;")]
    );
}

#[test]
fn a_dispatch_flow_becomes_a_model_with_its_ports() {
    use ctadl_ascent::models::matches::Disposition;
    let out = run_dispatch(
        &dispatch_program(),
        r#"K::dispatch(return <- arg(0)."[]") :- callsig(K, name = "next");"#,
    );
    let model = &out.dispatch[&key("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;")];
    let Disposition::Model(ports) = &model.disposition else {
        panic!("expected a model, got {:?}", model.disposition);
    };
    assert_eq!(ports.len(), 1);
    // `(destination, source)`: the return is written, the receiver's element read.
    assert_eq!(ports[0].0.tag, FormalIndexTypeTag::Return);
    assert_eq!(ports[0].1.tag, FormalIndexTypeTag::Index);
    assert_eq!(ports[0].1.index, Some(0));
    // The canonical spelling escapes the bracket, as `models/portcullis/tests.rs` pins elsewhere.
    assert_eq!(ports[0].1.path.to_dot_string(), r".\[]");
}

#[test]
fn closure_shaped_is_a_marker_not_a_model() {
    let out = run_dispatch(
        &dispatch_program(),
        r#"K::dispatch(closure_shaped = true) :- callsig(K, parent = "Ljava/lang/Runnable;");"#,
    );
    assert!(out.dispatch.is_empty(), "{:?}", out.dispatch);
    assert_eq!(
        out.closure_shaped.iter().collect::<Vec<_>>(),
        vec![&key("Ljava/lang/Runnable;", "run", "()V")]
    );
}

/// Dispatch heads are index-time, like a propagation: `ctadl query` keeps the source/sink half.
#[test]
fn a_dispatch_head_belongs_to_the_index_phase() {
    let source = r#"K::dispatch(resolve = "inline") :- callsig(K, name = "size");"#;
    let program_info = dispatch_program();
    let keys = ctadl_ascent::models::DispatchKeys::from_program(&program_info.program);
    for (phase, want) in [(portcullis::Phase::Index, 1), (portcullis::Phase::Query, 0)] {
        let file = portcullis::PortcullisFile::from_text("test.ctadl", source).expect("parses");
        let set = portcullis::PortcullisModelSet { files: vec![file] };
        let mut out = ProgramModelMatches::default();
        let mut matcher = portcullis::PortcullisMatcher::new(&set, phase);
        let index = ProgramMatchIndex::new_with_dispatch(
            &program_info,
            ImportScope::unknown(),
            Some(&keys),
        );
        matcher.observe_import(&index, &mut out);
        matcher.finish(&mut out).expect("finishes");
        assert_eq!(out.dispatch.len(), want, "{phase:?}");
    }
}

/// A program collected without the dispatch universe matches nothing rather than erroring: the
/// gate that decides whether to collect the keys lives in `cli`, and this is what a rule sees
/// when it is closed.
#[test]
fn callsig_is_empty_without_a_dispatch_universe() {
    let out = run(
        &dispatch_program(),
        r#"K::dispatch(resolve = "skip") :- callsig(K, name = "next");"#,
    );
    assert!(out.dispatch.is_empty());
}

// ---- facts ----

use ctadl_ascent::facts::Str;
use portcullis::relations::{Needs, ProgramFacts, SiteId};

/// A direct call CHA resolved to every one of `callees`.
fn call_any(callees: &[&str]) -> Statement {
    Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(callees.iter().map(|c| c.to_string()).collect()),
        },
        rets: Default::default(),
        args: Default::default(),
    })
}

/// A program of bodied functions with no method table, the shape flowy produces.
fn plain_program(functions: Vec<FunctionData>) -> ProgramInfo {
    ProgramInfo {
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

fn needs_of(source: &str) -> Needs {
    let program = portcullis::parse::parse_program(source).unwrap_or_else(|e| panic!("{e:?}"));
    Needs::of_program(&program)
}

/// A call site is identified by its position, so building `callsite` interns nothing per site:
/// the interner never frees, and an id per site would be a permanent cost of every import.
#[test]
fn a_call_site_id_is_never_interned() {
    // A caller no other test names, so nothing else could have interned its site ids.
    let caller = "facts-leak-caller";
    let program_info = plain_program(vec![
        function(
            caller,
            0,
            (0..50).map(|_| call("facts-leak-callee")).collect(),
        ),
        function("facts-leak-callee", 0, vec![]),
    ]);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let facts = ProgramFacts::build(&index, &Needs::all());
    assert_eq!(facts.callsites.len(), 50);
    for stmt in [0, 1, 49] {
        let id = format!("{caller}#0:{stmt}");
        assert_eq!(Str::try_get(&id), None, "{id} was interned");
        // The printed spelling still names the site.
        let site = SiteId::parse(&id).expect("parses");
        assert_eq!(site.to_string(), id);
        assert_eq!(facts.callsites_with_id(site), stmt..stmt + 1);
    }
}

/// Every built-in relation a rule body reads, under a negation and inside `&&` / `||` groups
/// too, and nothing else.
#[test]
fn needs_are_read_from_every_atom_of_every_rule() {
    assert_eq!(
        needs_of(r#"source(F::return) :- fun(F, name = "x"), param(F, 0);"#),
        Needs::default()
    );
    assert_eq!(
        needs_of(
            r#"source(F::return) :- fun(F), !(callsite(F, _, callee_string = G) && fun(G));
               sink(F::arg(0)) :- fun(F, parent = P), (subclass+(P, "a") || uses_field(F, "f"));"#
        ),
        Needs {
            callsite: true,
            uses_field: true,
            subclass: true,
            callsig: false,
            local: false,
            allocator: false,
        }
    );
    let mut needs = needs_of(r#"K::dispatch(resolve = "skip") :- callsig(K, name = "n");"#);
    assert_eq!(
        needs,
        Needs {
            callsig: true,
            ..Needs::default()
        }
    );
    needs.union(needs_of(r#"source(F::return) :- subclass*(F, F);"#));
    assert!(needs.callsig && needs.subclass && !needs.callsite);
}

/// A table no rule reads is not built; `fun` always is.
#[test]
fn only_the_needed_tables_are_built() {
    let program_info = java_program();
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let none = ProgramFacts::build(&index, &Needs::default());
    assert_eq!(none.funs.len(), 3);
    assert!(none.callsites.is_empty() && none.uses_field.is_empty());
    assert!(none.subclass.is_empty() && none.classes.is_empty());

    let all = ProgramFacts::build(&index, &Needs::all());
    assert_eq!(all.callsites.len(), 1);
    assert_eq!(all.uses_field.len(), 1);
    assert_eq!(all.subclass.len(), 1);

    let fields_only = ProgramFacts::build(
        &index,
        &Needs {
            uses_field: true,
            ..Needs::default()
        },
    );
    assert!(fields_only.callsites.is_empty());
    assert_eq!(fields_only.uses_field.len(), 1);
}

/// `subs` is `supers` inverted, including around a cycle, which an `__index` chain can form.
#[test]
fn subs_is_the_converse_of_supers_through_a_cycle() {
    let edges: &[(&str, &str)] = &[("A", "B"), ("B", "A"), ("C", "B"), ("D", "C")];
    let mut hierarchy: hashbrown::HashMap<JavaClass, smallvec::SmallVec<[JavaClass; 2]>> =
        Default::default();
    for (sub, sup) in edges {
        hierarchy
            .entry(JavaClass((*sub).into()))
            .or_default()
            .push(JavaClass((*sup).into()));
    }
    let program_info = ProgramInfo {
        vmt: test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
            methods: Vec::new(),
            hierarchy,
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        })),
        ..Default::default()
    };
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let facts = ProgramFacts::build(&index, &Needs::all());
    let s = |c: &str| Str::from(c);
    let mut direct = vec![s("A"), s("C")];
    direct.sort_unstable();
    assert_eq!(facts.subs(s("B"), false, false), direct);
    for &cls in &facts.classes {
        for (reflexive, deep) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut want: Vec<Str> = facts
                .classes
                .iter()
                .copied()
                .filter(|&c| (reflexive && c == cls) || facts.supers(c, false, deep).contains(&cls))
                .collect();
            want.sort_unstable();
            assert_eq!(
                facts.subs(cls, reflexive, deep),
                want,
                "subs({cls}, {reflexive}, {deep})"
            );
        }
    }
    // Strictly below B, through the cycle: B itself, and D two steps down.
    let below = facts.subs(s("B"), false, true);
    assert!(
        below.contains(&s("B")) && below.contains(&s("D")),
        "{below:?}"
    );
}

/// A call CHA resolved several ways is one site with one row per edge, and a bound site
/// reaches all of them.
#[test]
fn a_multi_edge_site_keeps_every_edge() {
    let program_info = plain_program(vec![
        function("caller", 0, vec![call_any(&["g", "h"])]),
        function("g", 1, vec![]),
        function("h", 1, vec![]),
    ]);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let facts = ProgramFacts::build(&index, &Needs::all());
    let site = SiteId {
        caller: Str::from("caller"),
        block: 0,
        stmt: 0,
    };
    let callees: BTreeSet<String> = facts
        .callsites_with_id(site)
        .map(|i| facts.callsites[i].callee.to_string())
        .collect();
    assert_eq!(callees, BTreeSet::from(["g".to_string(), "h".to_string()]));

    // Through a rule: the site found by its edge to `g` is the site whose edges include `h`.
    let matches = run(
        &program_info,
        r#"source(G::return) :- callsite(_, S, callee_string = "g"),
                                callsite(_, S, callee_string = G);"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["g".to_string(), "h".to_string()])
    );
}

// ---- streaming ----

/// Two native imports, a Lua one declaring `lua` and a pcode one declaring `native`.
fn lua_and_pcode(lua: &[&str], native: &[&str]) -> [(ImportScope, ProgramInfo); 2] {
    use ctadl_ascent::project::ArtifactLanguage;
    [
        (
            ImportScope::new(ArtifactLanguage::Lua, "app"),
            native_program(lua),
        ),
        (
            ImportScope::new(ArtifactLanguage::Pcode, "lib"),
            native_program(native),
        ),
    ]
}

/// Runs `source` over `imports` for `phase`, returning the matches and the report.
fn run_phase(
    imports: &[(ImportScope, ProgramInfo)],
    source: &str,
    phase: portcullis::Phase,
) -> (ProgramModelMatches, portcullis::PortcullisReport) {
    let file = portcullis::PortcullisFile::from_text("test.ctadl", source)
        .unwrap_or_else(|e| panic!("{e}"));
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut out = ProgramModelMatches::default();
    let mut matcher = portcullis::PortcullisMatcher::new(&set, phase);
    for (scope, program_info) in imports {
        let index = ProgramMatchIndex::new(program_info, scope.clone());
        matcher.observe_import(&index, &mut out);
    }
    let report = matcher.finish(&mut out).unwrap_or_else(|e| panic!("{e}"));
    (out, report)
}

/// The phase is fixed when the matcher is built, and a matcher none of whose rules have a head
/// for it has nothing to evaluate — while still counting what it skipped.
#[test]
fn a_matcher_built_for_one_phase_skips_the_others_rules() {
    let source = r#"propagation(F::return <- F::arg(0)) :- fun(F, name = "emit");"#;
    let file = portcullis::PortcullisFile::from_text("test.ctadl", source).expect("loads");
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    assert!(portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Query).is_empty());
    assert!(!portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Index).is_empty());

    let imports = [(ImportScope::unknown(), java_program())];
    let (out, report) = run_phase(&imports, source, portcullis::Phase::Query);
    assert!(out.propagations.is_empty());
    assert_eq!(report.skipped_rules(), 1);
    assert!(report.phase_warning().is_some());
    assert!(report.dead_rules().is_empty());
}

/// A one-component rule is emitted per import, so a key two imports both produce — the same
/// function name declared in each — must still be emitted once, and the report must describe
/// the run, not the last import.
#[test]
fn a_per_import_rule_emits_a_key_two_imports_share_once() {
    let (out, report) = run_phase(
        &lua_and_pcode(&["shared"], &["shared"]),
        r#"source(F::return) :- fun(F, name = "shared");"#,
        portcullis::Phase::All,
    );
    assert_eq!(out.endpoints.len(), 1, "{:?}", out.endpoints);
    let stats = &report.files[0].rules[0];
    assert_eq!(stats.sources, 1);
    assert_eq!(stats.functions, 1);
}

/// A rule that matches only in a later import is live, not dead: the stats accumulate.
#[test]
fn a_per_import_rule_matching_only_in_a_later_import_is_not_dead() {
    let (out, report) = run_phase(
        &lua_and_pcode(&["a"], &["b"]),
        r#"source(F::return) :- fun(F, name = "b");"#,
        portcullis::Phase::All,
    );
    assert_eq!(out.endpoints.len(), 1);
    assert!(report.dead_rules().is_empty(), "{:?}", report.dead_rules());
}

/// A bridging rule still fires whichever import comes first, and its head grounds over its own
/// variables only: the other component is a gate.
#[test]
fn a_bridging_rule_fires_across_imports_in_either_order() {
    let lua: &[&str] = &["luaCallNativeAdd"];
    let native: &[&str] = &["luaNativeAdd"];
    let source = r#"source(F::return) :-
             fun(F, name = "luaCallNativeAdd", language = "lua"),
             fun(G, name = "luaNativeAdd", language = "pcode");"#;
    let mut imports = lua_and_pcode(lua, native);
    for _ in 0..2 {
        let (out, report) = run_phase(&imports, source, portcullis::Phase::All);
        assert_eq!(
            endpoint_functions(&out),
            ["<EXTERNAL>::luaCallNativeAdd@00101008".to_string()].into(),
        );
        assert!(report.dead_rules().is_empty());
        imports.reverse();
    }
    // A gate that never opens kills the rule.
    let (out, report) = run_phase(
        &lua_and_pcode(lua, &["other"]),
        source,
        portcullis::Phase::All,
    );
    assert!(out.endpoints.is_empty());
    assert_eq!(report.dead_rules(), vec!["test.ctadl:0".to_string()]);
}

/// `functions` counts distinct anchors. A native function has two spellings, so `fun(F, name =
/// N)` grounds twice for one function.
#[test]
fn functions_matched_counts_functions_not_groundings() {
    let (_, report) = run_phase(
        &[(ImportScope::unknown(), native_program(&["system"]))],
        r#"source(F::return) :- fun(F, name = N), regex_match(N, "system");"#,
        portcullis::Phase::All,
    );
    let stats = &report.files[0].rules[0];
    assert_eq!(stats.functions, 1, "{stats:?}");
}

/// The join size is a product, and it saturates rather than overflowing; enumerating a head's
/// own columns never forms the product at all.
#[test]
fn grounding_counts_saturate_and_groundings_stay_lazy() {
    use ctadl_ascent::models::portcullis::eval::{RuleSolutions, Value};
    let component: BTreeSet<Vec<Value>> = (0..10_000).map(|i| vec![Value::Int(i)]).collect();
    // 10^20 groundings: past `u64::MAX`.
    let solutions = RuleSolutions {
        components: vec![component.clone(); 5],
        ..Default::default()
    };
    assert_eq!(solutions.grounding_count(), usize::MAX);
    assert_eq!(solutions.groundings(&[(2, 0)]).count(), 10_000);
    assert_eq!(solutions.groundings(&[(0, 0), (1, 0)]).take(5).count(), 5);
    // An open gate is a factor of one; a closed one empties the join.
    let gated = RuleSolutions {
        components: vec![component.clone(), [Vec::new()].into()],
        ..Default::default()
    };
    assert_eq!(gated.grounding_count(), 10_000);
    assert_eq!(gated.groundings(&[(0, 0)]).count(), 10_000);
    let closed = RuleSolutions {
        components: vec![component, BTreeSet::new()],
        ..Default::default()
    };
    assert_eq!(closed.groundings(&[(0, 0)]).count(), 0);
}

/// The model check matches the DSL across every import with one matcher, as `ctadl index` does:
/// a bridging rule satisfied only by two imports together matched something, and is not a
/// `CTADL0004`.
#[test]
fn the_model_check_does_not_call_a_cross_import_rule_dead() {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::with_suffix(".ctadl").expect("tempfile");
    write!(
        file,
        r#"source(F::return) :-
             fun(F, name = "luaCallNativeAdd", language = "lua"),
             fun(G, name = "luaNativeAdd", language = "pcode");"#
    )
    .expect("write");
    let imports = lua_and_pcode(&["luaCallNativeAdd"], &["luaNativeAdd"]);
    let outcome = ctadl_ascent::cli::check_programs(
        imports.into_iter().map(Ok),
        &[file.path().to_path_buf()],
    )
    .expect("checking");
    assert!(
        !outcome.has_file_errors(),
        "{:?}",
        outcome.check.file_errors
    );
    assert_eq!(
        outcome.dead_declarations(),
        0,
        "{:?}",
        outcome.endpoint_stats
    );
    assert_eq!(outcome.rows_matched(TaintDirection::Forward), 1);
}

// ---- planner/eval ----

/// `rec` calls itself, `a` calls `b`. No method table: `fun` comes from the lowered bodies.
fn recursive_program() -> ProgramInfo {
    ProgramInfo {
        program: Program::new(Functions::new(vec![
            function("rec", 1, vec![call("rec")]),
            function("a", 1, vec![call("b")]),
            function("b", 1, vec![]),
        ])),
        ..Default::default()
    }
}

/// A variable written twice in one atom is one variable: its first occurrence binds it and the
/// second tests the binding, so `callsite(F, S, callee_string = F)` is a recursive call.
#[test]
fn a_repeated_variable_unifies_within_one_atom() {
    let matches = run(
        &recursive_program(),
        r#"sink(F::arg(0)) :- callsite(F, S, callee_string = F);"#,
    );
    assert_eq!(
        endpoint_functions(&matches),
        BTreeSet::from(["rec".to_string()])
    );

    // No class is its own direct superclass and no function's name is a field it uses, so each
    // of these existence checks fails and takes the rule with it.
    let java = java_program();
    for body in ["subclass(C, C)", "uses_field(X, X)"] {
        let matches = run(&java, &format!("source(F::return) :- fun(F), {body};"));
        assert!(
            matches.endpoints.is_empty(),
            "{body}: {:?}",
            matches.endpoints
        );
    }
    // The reflexive closure does relate every class to itself.
    let matches = run(
        &java,
        r#"source(F::return) :- fun(F, parent = C), subclass*(C, C);"#,
    );
    assert_eq!(endpoint_functions(&matches).len(), 3);
}

/// A negated group's locals connect nothing. Each side of this bridge quantifies its own `N`, so
/// the sides stay two components and pair up across imports.
#[test]
fn negated_group_locals_do_not_join_components() {
    use ctadl_ascent::project::ArtifactLanguage;

    let lua = native_program(&["luaCallNativeAdd"]);
    let native = native_program(&["luaNativeAdd"]);
    let matches = run_scoped(
        &[
            (ImportScope::new(ArtifactLanguage::Lua, "app"), &lua),
            (ImportScope::new(ArtifactLanguage::Pcode, "lib"), &native),
        ],
        r#"bridge(F::arg(0) -> G::arg(0)) :-
             fun(F, language = "lua"), !(fun(F, name = N) && regex_match(N, "^x")),
             fun(G, language = "pcode"), !(fun(G, name = N) && regex_match(N, "^y"));"#,
    );
    assert_eq!(
        matches.resolved_bridges.len(),
        1,
        "{:?}",
        matches.resolved_bridges
    );
}

/// `in` compares a call-signature key by its id, exactly as `=` does.
#[test]
fn a_set_test_matches_a_callsig_key() {
    let id = "Ljava/util/Iterator;->next()Ljava/lang/Object;";
    for test in [format!("K = {id:?}"), format!("K in {{{id:?}, \"other\"}}")] {
        let out = run_dispatch(
            &dispatch_program(),
            &format!(r#"K::dispatch(resolve = "skip") :- callsig(K), {test};"#),
        );
        assert_eq!(
            out.dispatch.keys().collect::<Vec<_>>(),
            vec![&key("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;")],
            "{test}"
        );
    }
}

/// Solutions keep only what a head reads, so a body variable no head mentions does not
/// multiply the groundings.
#[test]
fn groundings_are_projected_onto_head_variables() {
    let file = portcullis::PortcullisFile::from_text(
        "test.ctadl",
        r#"source(F::return) :- fun(F, name = "run"), param(F, I);"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::All);
    let program_info = java_program();
    let mut out = ProgramModelMatches::default();
    matcher.observe_import(
        &ProgramMatchIndex::new(&program_info, ImportScope::unknown()),
        &mut out,
    );
    let report = matcher.finish(&mut out).unwrap_or_else(|e| panic!("{e}"));
    // `run` has three parameters; the head reads only `F`.
    assert_eq!(report.files[0].rules[0].groundings, 1);
    assert_eq!(out.endpoints.len(), 1);
}

/// The mode heads fill the maps the SSA pipeline and codegen read, and of two rules that
/// disagree on a function the one with the higher priority wins.
#[test]
fn mode_heads_mark_their_functions() {
    use ctadl_ir::ssa::{FrameWrites, FullMode};
    let program_info = native_program(&["big", "small", "unwind"]);
    let m = run(
        &program_info,
        r#"
        mem2reg_full(F), frame_store(F) :- fun(F, name = "big");
        mem2reg_window(F) :- fun(F, name = "small");
        frame_update(F) :- fun(F, name = "small");
        mem2reg_full(F, priority = 1) :- fun(F, name = "small");
        skip_analysis(F) :- fun(F, name = "unwind");
        "#,
    );
    m.check_mode_conflicts().unwrap();
    // A mode keys on a function's full name, which for a native symbol is the decorated one.
    let full = |n: &str| format!("<EXTERNAL>::{n}@00101008");
    fn named<T: Copy>(
        m: &std::collections::BTreeMap<ctadl_ascent::facts::Str, T>,
    ) -> Vec<(String, T)> {
        m.iter().map(|(f, mode)| (f.to_string(), *mode)).collect()
    }
    assert_eq!(
        named(&m.mem2reg_modes),
        [
            (full("big"), FullMode::Force),
            (full("small"), FullMode::Force)
        ]
    );
    assert_eq!(
        named(&m.frame_modes),
        [
            (full("big"), FrameWrites::Store),
            (full("small"), FrameWrites::Update)
        ]
    );
    let skipped: Vec<String> = m.skip_analysis.iter().map(|f| f.to_string()).collect();
    assert_eq!(skipped, [full("unwind")]);
}

/// The functions `skip_analysis` holds, by simple name.
fn skipped(m: &ProgramModelMatches) -> Vec<String> {
    m.skip_analysis
        .iter()
        .map(|f| {
            let f = f.to_string();
            f.trim_start_matches("<EXTERNAL>::")
                .trim_end_matches("@00101008")
                .to_string()
        })
        .collect()
}

/// The highest priority wins whichever rule comes first, and `analyze` undoes a `skip_analysis`
/// it outranks.
#[test]
fn the_highest_priority_mode_wins_in_either_order() {
    let program_info = native_program(&["f", "g"]);
    for source in [
        r#"skip_analysis(F) :- fun(F); analyze(F, priority = 5) :- fun(F, name = "f");"#,
        r#"analyze(F, priority = 5) :- fun(F, name = "f"); skip_analysis(F) :- fun(F);"#,
    ] {
        let m = run(&program_info, source);
        m.check_mode_conflicts()
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(skipped(&m), ["g"], "{source}");
    }
    // A negative priority is outranked by one left unwritten.
    let m = run(
        &program_info,
        r#"skip_analysis(F, priority = -100) :- fun(F); analyze(F) :- fun(F, name = "g");"#,
    );
    m.check_mode_conflicts().unwrap();
    assert_eq!(skipped(&m), ["f"]);
}

/// Two different modes at one priority are an error naming the function and both rules; two
/// rules claiming the same mode are not.
#[test]
fn different_modes_at_one_priority_conflict() {
    let program_info = native_program(&["f", "g"]);
    let m = run(
        &program_info,
        r#"
        skip_analysis(F) :- fun(F);
        skip_analysis(F) :- fun(F, name = "g");
        analyze(F) :- fun(F, name = "f");
        "#,
    );
    let err = m.check_mode_conflicts().unwrap_err().to_string();
    assert!(err.contains("<EXTERNAL>::f@00101008"), "{err}");
    assert!(err.contains("'skip_analysis' from test.ctadl:0"), "{err}");
    assert!(err.contains("'analyze' from test.ctadl:2"), "{err}");
    assert!(err.contains("both at priority 0"), "{err}");
    assert!(!err.contains("::g@"), "{err}");
}

/// A tie is settled by a higher claim, whether it comes before or after the tie.
#[test]
fn a_higher_priority_settles_a_tie() {
    let program_info = native_program(&["f"]);
    for source in [
        r#"skip_analysis(F) :- fun(F); analyze(F) :- fun(F); analyze(F, priority = 1) :- fun(F);"#,
        r#"analyze(F, priority = 1) :- fun(F); skip_analysis(F) :- fun(F); analyze(F) :- fun(F);"#,
    ] {
        let m = run(&program_info, source);
        m.check_mode_conflicts()
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert!(skipped(&m).is_empty(), "{source}");
    }
}

/// Modes on different axes never conflict.
#[test]
fn modes_on_different_axes_are_independent() {
    let program_info = native_program(&["f"]);
    let m = run(
        &program_info,
        r#"skip_analysis(F) :- fun(F); mem2reg_full(F) :- fun(F); frame_store(F) :- fun(F);"#,
    );
    m.check_mode_conflicts().unwrap();
    assert_eq!(skipped(&m), ["f"]);
    assert_eq!(m.mem2reg_modes.len(), 1);
    assert_eq!(m.frame_modes.len(), 1);
}

/// A user's `analyze` outranks the native defaults' allocator skip without naming a priority.
#[test]
fn a_user_analyze_overrides_a_default_skip() {
    let program_info = native_program(&["malloc", "free", "realloc"]);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut out = ProgramModelMatches::default();
    ctadl_ascent::models::try_load_default_models(&index, &mut out).unwrap();
    let file = portcullis::PortcullisFile::from_text(
        "user.ctadl",
        r#"analyze(F) :- fun(F, name = "malloc");"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::All);
    matcher.observe_import(&index, &mut out);
    matcher.finish(&mut out).unwrap_or_else(|e| panic!("{e}"));
    out.check_mode_conflicts().unwrap();
    assert_eq!(skipped(&out), ["free", "realloc"]);
}

// ---------------------------------------------------------------------------
// allocator
// ---------------------------------------------------------------------------

/// The `allocator` rows `source` selects, as `name kind by` lines. Each rule's head is
/// `skip_analysis(F)`, and the `kind` and `by` it reads come back through a second pass.
fn allocators(program_info: &ProgramInfo, select: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for kind in ["malloc", "realloc", "free", "internal"] {
        for by in ["name", "shape"] {
            let m = run(
                program_info,
                &format!(
                    r#"skip_analysis(F) :- allocator(F, kind = "{kind}", by = "{by}"), {select};"#
                ),
            );
            rows.extend(skipped(&m).into_iter().map(|f| format!("{f} {kind} {by}")));
        }
    }
    rows.sort();
    rows
}

/// Every allocator name is a row, with its kind; other functions are not.
#[test]
fn allocator_lists_named_entry_points_by_kind() {
    let program_info = native_program(&["malloc", "realloc", "free", "mallopt", "strcpy"]);
    assert_eq!(
        allocators(&program_info, "fun(F)"),
        [
            "free free name",
            "malloc malloc name",
            "mallopt internal name",
            "realloc realloc name",
        ]
    );
}

/// A Java method named like an allocator is not one.
#[test]
fn allocator_is_native_only() {
    let mut program_info = java_program();
    program_info.vmt = test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
        methods: vec![(
            JavaClass("Lc/Pool;".into()),
            JavaSimpleName("free".into()),
            JavaSignature("()V".into()),
            JavaMethod("Lc/Pool;->free()V".into()),
        )],
        hierarchy: Default::default(),
        interfaces: Vec::new(),
        abstract_methods: Vec::new(),
        natives: Vec::new(),
    }));
    assert!(allocators(&program_info, "fun(F)").is_empty());
}

/// A stripped program with dlmalloc's shape: `free` and `realloc` read the header word before
/// their pointer and write the heap state, which `malloc` writes too. `FUN_peek` reads a header
/// word but writes nothing, `FUN_stats` writes the heap state but has no allocator signature,
/// and `FUN_lonely` is shaped like `malloc` but has a single caller. `main` calls them.
fn stripped_allocator() -> ProgramInfo {
    use ctadl_ir::mir::{Exp, OffsetAccess, ParameterIdx, ReturnType};
    const HEAP: [i64; 4] = [0x10003010, 0x10003014, 0x10003040, 0x10003044];
    let heap_stores = |n: usize| -> Vec<Statement> {
        (0..n)
            .map(|i| {
                Statement::new_kind(StatementKind::Store {
                    dest: AccessPath::new(
                        VariableRef::new_global(),
                        [OffsetAccess::Offset(ctadl_ir::mir::Offset(
                            HEAP[i % HEAP.len()],
                        ))],
                    ),
                    field: FieldRef::symbol("deref"),
                    value: Exp::Int(i as i64),
                })
            })
            .collect()
    };
    let header_load = Statement::new_kind(StatementKind::Load {
        dest: VariableRef::new_local_idx(LocalIdx::new(0)),
        source: AccessPath::new(
            VariableRef::new_parameter(ParameterIdx::new(0)),
            [OffsetAccess::Offset(ctadl_ir::mir::Offset(0xffff_fffc))],
        ),
        field: FieldRef::symbol("deref"),
    });
    let with = |name: &str, params: usize, returns: bool, body: Vec<Statement>| {
        let mut f = function(name, params, body);
        f.return_type = ReturnType {
            arity: u8::from(returns),
        };
        f
    };
    let functions = vec![
        with(
            "FUN_free",
            1,
            false,
            [vec![header_load.clone()], heap_stores(120)].concat(),
        ),
        with(
            "FUN_realloc",
            2,
            true,
            [vec![header_load.clone()], heap_stores(160)].concat(),
        ),
        with("FUN_malloc", 1, true, heap_stores(700)),
        with("FUN_peek", 1, true, vec![header_load.clone()]),
        with("FUN_stats", 3, true, heap_stores(200)),
        with("FUN_lonely", 1, true, heap_stores(200)),
        with(
            "main",
            0,
            false,
            [
                "FUN_malloc",
                "FUN_malloc",
                "FUN_malloc",
                "FUN_free",
                "FUN_realloc",
                "FUN_peek",
                "FUN_stats",
                "FUN_lonely",
            ]
            .into_iter()
            .map(call)
            .collect(),
        ),
    ];
    let names: Vec<String> = functions.iter().map(|f| f.name.clone()).collect();
    ProgramInfo {
        vmt: test_vmt::native(
            names
                .iter()
                .map(|n| (n.as_str(), "", n.as_str(), n.as_str())),
        ),
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

#[test]
fn allocator_finds_a_stripped_dlmalloc_by_shape() {
    assert_eq!(
        allocators(&stripped_allocator(), "fun(F)"),
        [
            "FUN_free free shape",
            "FUN_malloc malloc shape",
            "FUN_realloc realloc shape",
        ]
    );
}

/// A global store sharing a call's statement is the frontend's copy of what the callee may
/// change, not the function's own write, so a program whose heap stores all sit at calls has
/// no heap state and no allocator.
#[test]
fn stores_at_a_call_are_not_the_functions_own() {
    let mut program_info = stripped_allocator();
    for f in program_info.program.functions.functions.iter_mut() {
        if f.name != "main" {
            let block = f.blocks.blocks_mut().iter_mut().next().unwrap();
            block.extend([call("main")]);
        }
    }
    assert!(allocators(&program_info, "fun(F)").is_empty());
}
