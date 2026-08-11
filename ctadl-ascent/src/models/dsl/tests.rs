/*! Unit tests for the DSL: syntax, checking, and the migrator.

Engine behaviour that needs a program lives in `tests/model_dsl.rs`, which builds one.
*/

use super::*;
use crate::models::dsl::ast::*;

fn parse(text: &str) -> Program {
    match parse::parse_program(text) {
        Ok(p) => p,
        Err(e) => panic!("{}", e.render(std::path::Path::new("test.ctadl"), text)),
    }
}

fn check_err(text: &str) -> String {
    match DslFile::from_text("test.ctadl", text) {
        Ok(_) => panic!("expected an error from:\n{text}"),
        Err(e) => e.to_string(),
    }
}

fn check_ok(text: &str) -> DslFile {
    match DslFile::from_text("test.ctadl", text) {
        Ok(f) => f,
        Err(e) => panic!("{e}"),
    }
}

// ---------------------------------------------------------------------------
// Syntax
// ---------------------------------------------------------------------------

#[test]
fn parses_the_design_examples() {
    let program = parse(
        r#"
        // Flows java.net.URL.openConnection's "this" argument to return and sets return source
        source(F::return), propagation(F::return <- F::arg(0)) :-
          fun(F, name = "openConnection", parent = "Ljava/net/URL;");

        propagation(F::return <- F::arg(0)) :-
          fun(F,
            name in {"getAbsolutePath","getAbsoluteFile","getCanonicalFile"},
            parent = "Ljava/io/File;");

        sink(F::arg(I)) :- fun(F), param(F, I);

        access_paths(".next.next.next");
        "#,
    );
    assert_eq!(program.rules.len(), 4);
    assert_eq!(program.rules[0].heads.len(), 2);
    assert!(program.rules[3].body.is_empty(), "a bare head is a fact");
}

#[test]
fn parses_ports_and_paths() {
    let program = parse(
        r#"sink(F::arg(0).foo[2]), sink(F::arg(4)."weird.field[0]"), sink(F::arg(_)) :- fun(F);"#,
    );
    let heads = &program.rules[0].heads;
    let path_of = |i: usize| match &heads[i].kind {
        HeadKind::Sink { port, .. } => port.port.path.clone(),
        other => panic!("expected a sink, got {other:?}"),
    };
    use ctadl_ir::mir::{Offset, PathSegment, Symbol};
    assert_eq!(
        path_of(0),
        vec![
            PathSegment::Symbol(Symbol::from("foo")),
            PathSegment::Offset(Offset(2))
        ]
    );
    assert_eq!(
        path_of(1),
        vec![PathSegment::Symbol(Symbol::from("weird.field[0]"))]
    );
    match &heads[2].kind {
        HeadKind::Sink { port, .. } => assert!(matches!(port.port.base, PortBase::AnyArg)),
        other => panic!("expected a sink, got {other:?}"),
    }
}

#[test]
fn flow_arrows_keep_their_operands() {
    let program = parse(
        r#"propagation(F::arg(1) -> F::arg(0)), propagation(F::arg(2).foo <-> F::arg(0).bar)
             :- fun(F);"#,
    );
    let flow = |i: usize| match &program.rules[0].heads[i].kind {
        HeadKind::Propagation { flow } => flow.clone(),
        other => panic!("expected a propagation, got {other:?}"),
    };
    let f0 = flow(0);
    assert_eq!(f0.op, FlowOp::ToRight);
    // `arg(1) -> arg(0)`: data leaves the left operand and arrives at the right.
    assert!(matches!(f0.src().port.base, PortBase::Arg(1)));
    assert!(matches!(f0.dst().port.base, PortBase::Arg(0)));
    assert_eq!(flow(1).op, FlowOp::Both);
}

#[test]
fn comments_are_skipped() {
    let program = parse(
        r#"
        // a line comment
        /* and a
           block one */
        access_paths(".foo"); // trailing
        "#,
    );
    assert_eq!(program.rules.len(), 1);
}

/// The design's parsing note: `arity < -1` and `arity <- 1` differ by one space, maximal munch
/// takes the arrow, and the parser must say so rather than pointing one token further on.
#[test]
fn arrow_in_attribute_position_is_named() {
    let err = check_err(r#"sink(F::arg(0)) :- fun(F, arity <- 1);"#);
    assert!(
        err.contains("'<-' is a flow arrow and is not allowed here"),
        "{err}"
    );
    // The spaced form is a perfectly ordinary comparison.
    check_ok(r#"sink(F::arg(0)) :- fun(F, arity < -1);"#);
}

// ---------------------------------------------------------------------------
// Checking
// ---------------------------------------------------------------------------

/// The design's worked example: an operator before the atom that binds its variable is a
/// load-time error, and the same two atoms the other way round are fine.
#[test]
fn modes_are_checked_left_to_right() {
    let err = check_err(r#"sink(F::arg(0)) :- regex_match(F, ".foo.*"), fun(F);"#);
    assert!(err.contains("'F' is not bound at this point"), "{err}");
    check_ok(r#"sink(F::arg(0)) :- fun(F), regex_match(F, ".foo.*");"#);
}

#[test]
fn head_variables_must_be_bound() {
    let err = check_err(r#"sink(G::arg(0)) :- fun(F);"#);
    assert!(err.contains("'G' appears in the head"), "{err}");
}

#[test]
fn a_port_needs_an_anchor() {
    let err = check_err(r#"sink(arg(0)) :- fun(F);"#);
    assert!(err.contains("no anchor"), "{err}");
}

#[test]
fn an_atom_level_anchor_reaches_its_ports() {
    // `S::bridge(arg(1) -> G::arg(0))`: the left port has no anchor of its own and takes the
    // atom's.
    check_ok(
        r#"bridge(F::arg(1).baz -> G::arg(0).stack[2]) :-
             fun(F, name = "luaCallNativeAdd"), fun(G, name = "luaNativeAdd");"#,
    );
}

#[test]
fn a_propagation_may_not_span_two_functions() {
    let err = check_err(r#"propagation(F::return <- G::arg(0)) :- fun(F), fun(G);"#);
    assert!(
        err.contains("both ports must be anchored at the same function"),
        "{err}"
    );
}

#[test]
fn a_propagation_may_not_depend_on_a_callsite() {
    let err = check_err(r#"propagation(S::return <- S::arg(0)) :- callsite(C, S);"#);
    assert!(err.contains("call site"), "{err}");
}

#[test]
fn a_bridge_over_one_function_is_a_propagation() {
    let err = check_err(r#"bridge(F::return <- F::arg(0)) :- fun(F);"#);
    assert!(err.contains("Write 'propagation' instead"), "{err}");
}

#[test]
fn unknown_relations_are_typos_not_user_relations() {
    let err = check_err(r#"sink(F::arg(0)) :- funn(F);"#);
    assert!(err.contains("is not a built-in relation"), "{err}");
    assert!(err.contains("Did you mean 'fun'?"), "{err}");
}

#[test]
fn output_relations_are_rejected_in_a_body() {
    let err = check_err(r#"sink(F::arg(0)) :- fun(F), source(F);"#);
    assert!(err.contains("can only appear in a rule head"), "{err}");
}

#[test]
fn unknown_attributes_are_rejected() {
    let err = check_err(r#"sink(F::arg(0)) :- fun(F, nmae = "x");"#);
    assert!(err.contains("is not an attribute of 'fun'"), "{err}");
}

#[test]
fn attribute_types_are_checked() {
    let err = check_err(r#"sink(F::arg(0)) :- fun(F, arity = "three");"#);
    assert!(err.contains("expected integer"), "{err}");
}

#[test]
fn source_and_sink_attributes_do_not_cross() {
    let err = check_err(r#"source(F::return, wildcard = false) :- fun(F);"#);
    assert!(err.contains("'wildcard' is a sink attribute"), "{err}");
    let err = check_err(r#"sink(F::arg(0), saturating = true) :- fun(F);"#);
    assert!(err.contains("'saturating' is a source attribute"), "{err}");
}

#[test]
fn a_negated_atom_needs_its_variables_bound() {
    let err = check_err(r#"sink(F::arg(0)) :- fun(F), !fun(G, name = "x");"#);
    assert!(err.contains("'G' is not bound at this point"), "{err}");
    // `_` is the way to say "any value at all", so this one is fine.
    check_ok(r#"sink(F::arg(0)) :- fun(F), !fun(F, parent = _);"#);
}

/// A negated *group* existentially quantifies the variables local to it. Without that, "no name
/// of F matches this pattern" cannot be written at all.
#[test]
fn a_negated_group_may_bind_its_own_locals() {
    check_ok(r#"sink(F::arg(0)) :- fun(F), !(fun(F, name = N) && regex_match(N, "^get"));"#);
}

#[test]
fn an_argument_index_variable_must_be_bound() {
    check_ok(r#"sink(F::arg(I)) :- fun(F), param(F, I);"#);
    let err = check_err(r#"sink(F::arg(I)) :- fun(F);"#);
    assert!(err.contains("'I' indexes an argument"), "{err}");
}

#[test]
fn a_body_splits_into_components_on_shared_variables() {
    let file = check_ok(
        r#"bridge(F::arg(0) -> G::arg(0).stack[1]) :-
             fun(F, name = "a", language = "lua"),
             fun(G, name = "b", language = "pcode");"#,
    );
    // Two programs, two components: this is what lets each side match in its own import.
    assert_eq!(file.plans[0].components.len(), 2);
    let file = check_ok(r#"sink(F::arg(I)) :- fun(F), param(F, I);"#);
    assert_eq!(file.plans[0].components.len(), 1);
}

#[test]
fn the_planner_runs_filters_as_soon_as_they_are_bound() {
    let file = check_ok(
        r#"sink(F::arg(0)) :- fun(F, name = N), regex_match(N, "^get"), fun(F, has_code = true);"#,
    );
    let steps = &file.plans[0].components[0].steps;
    // The regex filter is scheduled right after the atom that binds `N`, ahead of the second
    // (unindexed, whole-program) `fun` scan.
    assert_eq!(steps, &vec![0, 1, 2], "unexpected plan {steps:?}");
}

#[test]
fn phases_are_read_off_the_heads() {
    let file = check_ok(
        r#"source(F::return) :- fun(F);
           propagation(F::return <- F::arg(0)) :- fun(F);
           source(F::return), propagation(F::return <- F::arg(0)) :- fun(F);"#,
    );
    assert_eq!(file.program.rules[0].phases(), (false, true));
    assert_eq!(file.program.rules[1].phases(), (true, false));
    assert_eq!(file.program.rules[2].phases(), (true, true));
}

// ---------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------

fn migrate_one(json: &str) -> (String, migrate::MigrationReport) {
    let value: serde_json::Value = serde_json::from_str(json).expect("valid JSON fixture");
    migrate::migrate_generators(std::iter::once(&value), None)
}

/// Everything the migrator writes has to load. This is the property that makes the migrator
/// usable for backward compatibility at all, so every migration test ends here.
fn migrate_and_load(json: &str) -> String {
    let (text, report) = migrate_one(json);
    check_ok(&text);
    assert!(report.rules > 0, "no rule written for:\n{json}\n{text}");
    text
}

#[test]
fn migrates_a_signature_match_with_a_parent() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"signature_match","name":"openConnection",
                      "parent":"Ljava/net/URL;"}],
            "model":{"sources":[{"kind":"UserInput","port":"Return"}],
                     "propagation":[{"input":"Argument(0)","output":"Return"}]}}"#,
    );
    assert!(
        text.contains(r#"fun(F, name = "openConnection", parent = "Ljava/net/URL;")"#),
        "{text}"
    );
    assert!(
        text.contains(r#"source(F::return, kind = "UserInput")"#),
        "{text}"
    );
    assert!(
        text.contains("propagation(F::return <- F::arg(0))"),
        "{text}"
    );
}

#[test]
fn migrates_a_names_list_to_a_set() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"signature_match","names":["strcpy","strncpy"]}],
            "model":{"sinks":[{"kind":"buffer_overflow","port":"Argument(1).deref"}]}}"#,
    );
    assert!(text.contains(r#"name in {"strcpy", "strncpy"}"#), "{text}");
    assert!(text.contains("sink(F::arg(1).deref"), "{text}");
}

#[test]
fn migrates_a_name_regex_through_a_bound_variable() {
    let text = migrate_and_load(
        r#"{"find":"methods","where":[{"constraint":"name","pattern":"^get.*"}],
            "model":{"sources":[{"kind":"k","port":"Return"}]}}"#,
    );
    assert!(text.contains("fun(F, name = N0)"), "{text}");
    assert!(text.contains(r#"regex_match(N0, "^get.*")"#), "{text}");
}

/// `any_of` is a union over the matched set, and a union is what several rules with one head
/// mean in Datalog. The translation is exact, not an approximation.
#[test]
fn any_of_becomes_several_rules() {
    let (text, report) = migrate_one(
        r#"{"find":"methods",
            "where":[{"constraint":"any_of","inners":[
                {"constraint":"signature_match","name":"a"},
                {"constraint":"signature_match","name":"b"}]}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    check_ok(&text);
    assert_eq!(report.rules, 2, "{text}");
    assert!(
        text.contains(r#"name = "a""#) && text.contains(r#"name = "b""#),
        "{text}"
    );
}

#[test]
fn not_of_a_leaf_becomes_a_negated_atom() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"signature_match","name":"x"},
                     {"constraint":"not","inner":{"constraint":"has_code","value":true}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("!fun(F, has_code = true)"), "{text}");
}

/// De Morgan, and the reason a negated group is parenthesized: negating the two atoms of a
/// `name` regex separately would lose the variable they share.
#[test]
fn not_of_a_multi_atom_leaf_is_parenthesized() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"signature_match","name":"x"},
                     {"constraint":"not","inner":{"constraint":"name","pattern":"^get"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(
        text.contains("!(fun(F, name = N0) && regex_match(N0, \"^get\"))"),
        "{text}"
    );
}

#[test]
fn migrates_number_parameters_to_an_arity_attribute() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"number_parameters","inner":{"constraint":">","value":2}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("fun(F, arity > 2)"), "{text}");
}

/// A negative bound is where the `<-` collision bites: the migrator must write the space.
#[test]
fn a_negative_arity_bound_keeps_its_space() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"number_parameters","inner":{"constraint":"<","value":-1}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("arity < -1"), "{text}");
    assert!(!text.contains("arity <-1"), "{text}");
}

#[test]
fn migrates_uses_field() {
    let text = migrate_and_load(
        r#"{"find":"methods","where":[{"constraint":"uses_field","names":["a","b"]}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("uses_field(F, Fld0)"), "{text}");
    assert!(text.contains(r#"Fld0 in {"a", "b"}"#), "{text}");
}

#[test]
fn migrates_extends_through_subclass() {
    let text = migrate_and_load(
        r#"{"find":"methods",
            "where":[{"constraint":"extends",
                      "inner":{"constraint":"signature_match","name":"Lb/Base;"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("subclass("), "{text}");
    assert!(text.contains(r#"= "Lb/Base;""#), "{text}");
}

#[test]
fn migrates_callsites_through_the_callsite_relation() {
    let text = migrate_and_load(
        r#"{"find":"callsites",
            "where":[{"constraint":"signature_match","name":"emit"},
                     {"constraint":"in_function",
                      "inner":{"constraint":"name","pattern":"handleRequest"}}],
            "model":{"sinks":[{"kind":"TaintedData","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains("callsite(C, S, callee_string = F)"), "{text}");
    assert!(text.contains("fun(C, name = N0)"), "{text}");
    // The port hangs off the *site*, which is what makes the endpoint call-site scoped.
    assert!(text.contains("sink(S::arg(0)"), "{text}");
}

#[test]
fn migrates_the_in_scope_to_attributes() {
    let text = migrate_and_load(
        r#"{"find":"methods","in":{"languages":["dex","apk"]},
            "where":[{"constraint":"signature_match","name":"x"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains(r#"language in {"dex", "apk"}"#), "{text}");
}

#[test]
fn migrates_a_bridge_with_its_port_map() {
    let text = migrate_and_load(
        r#"{"find":"methods","in":{"language":"lua"},
            "where":[{"constraint":"signature_match","name":"mylib.add"}],
            "model":{"bridge":{
              "to":{"in":{"language":"pcode"},
                    "where":[{"constraint":"signature_match","name":"l_add"}]},
              "arguments":[
                {"from":"Argument(0)","to":"Argument(0).stack.[1]","direction":"in"},
                {"from":"Return","to":"Argument(0).stack.[-1]","direction":"out"}]}}}"#,
    );
    assert!(
        text.contains("bridge(F::arg(0) -> G::arg(0).stack.[1])"),
        "{text}"
    );
    assert!(
        text.contains("bridge(F::return <- G::arg(0).stack.[-1])"),
        "{text}"
    );
    // Side B's scope folds into the `fun` atom its own `where` produced.
    assert!(
        text.contains(r#"fun(G, language = "pcode", name = "l_add")"#),
        "{text}"
    );
}

#[test]
fn reports_the_constructs_that_do_not_translate() {
    let (_, report) = migrate_one(
        r#"{"find":"methods","where":[{"constraint":"signature_match","name":"x"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}],
                     "modes":["skip-analysis"],
                     "forward_self":{"where":[]}}}"#,
    );
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(report.warnings.iter().any(|w| w.contains("'modes'")));
    assert!(report.warnings.iter().any(|w| w.contains("'forward_self'")));
}

#[test]
fn a_java_array_element_segment_survives_the_round_trip() {
    // JSON writes the array-element field as `\[]`; the DSL quotes it instead.
    let text = migrate_and_load(
        r#"{"find":"methods","where":[{"constraint":"signature_match","name":"x"}],
            "model":{"propagation":[{"input":"Argument(0).\\[]","output":"Return"}]}}"#,
    );
    assert!(text.contains(r#"arg(0)."[]""#), "{text}");
    let program = parse(&text);
    let segments = match &program.rules[0].heads[0].kind {
        HeadKind::Propagation { flow } => flow.src().port.path.clone(),
        other => panic!("expected a propagation, got {other:?}"),
    };
    use ctadl_ir::mir::{PathSegment, Symbol};
    assert_eq!(segments, vec![PathSegment::Symbol(Symbol::from("[]"))]);
}

/// Every shipped default file must migrate cleanly and load. This is the drift guard: a
/// keyword added to the JSON loader and used in a default has to be understood here too.
#[test]
fn every_shipped_default_migrates_and_loads() {
    for (name, contents) in crate::models::DEFAULT_MODEL_FILES {
        let text = std::str::from_utf8(contents).expect("defaults are utf-8");
        let values: Vec<serde_json::Value> = text
            .lines()
            .map(str::trim_start)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}: {e}")))
            .collect();
        let (dsl, report) = migrate::migrate_generators(values.iter(), Some(name));
        // The only warnings a shipped default may raise are about keys the JSON loader does
        // not act on either, so nothing is lost by not translating them. Anything else means a
        // construct the defaults use and the migrator does not understand.
        for warning in &report.warnings {
            assert!(
                ["'taint'", "'modes'", "'forward_self'"]
                    .iter()
                    .any(|k| warning.contains(k)),
                "{name}: {warning}"
            );
        }
        assert_eq!(report.generators, values.len(), "{name}");
        if let Err(e) = DslFile::from_text(*name, dsl.clone()) {
            panic!("migrated {name} does not load: {e}\n---\n{dsl}");
        }
    }
}
