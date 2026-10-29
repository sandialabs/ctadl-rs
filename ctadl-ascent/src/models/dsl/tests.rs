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

/// A known value only makes an atom cheap when that column or attribute has an index.
#[test]
fn the_cost_model_counts_only_indexed_positions() {
    for (rule, want) in [
        // `param` and `uses_field` index their first column only, so a literal in the second
        // is still a scan, and the keyed `fun` goes first.
        (
            r#"sink(F::arg(0)) :- param(F, 0), fun(F, name = "x");"#,
            vec![1, 0],
        ),
        (
            r#"sink(F::arg(0)) :- uses_field(F, "token"), fun(F, name = "x");"#,
            vec![1, 0],
        ),
        // `callee_string` is indexed.
        (
            r#"sink(S::arg(0)) :- fun(C), callsite(C, S, callee_string = "x");"#,
            vec![1, 0],
        ),
        // A known super is a `subs` lookup.
        (
            r#"source(F::return) :- fun(F, parent = P), subclass+(P, "Lc/Base;");"#,
            vec![1, 0],
        ),
    ] {
        let file = check_ok(rule);
        assert_eq!(file.plans[0].components[0].steps, want, "{rule}");
    }
}

/// Two negated groups reusing a local's name are unrelated: neither binds anything the other
/// sees, so they neither need it bound nor fuse the components they sit in.
#[test]
fn negated_group_locals_neither_bind_nor_connect() {
    let file = check_ok(
        r#"bridge(F::arg(0) -> G::arg(0)) :-
             fun(F, language = "lua"), !(fun(F, name = N) && regex_match(N, "^x")),
             fun(G, language = "pcode"), !(fun(G, name = N) && regex_match(N, "^y"));"#,
    );
    let components = &file.plans[0].components;
    assert_eq!(components.len(), 2);
    assert_eq!(components[0].vars, vec!["F".to_string()]);
    assert_eq!(components[1].vars, vec!["G".to_string()]);
}

/// A component's tuple holds the head variables and nothing else; one no head reads is an
/// existence check with an empty tuple.
#[test]
fn components_keep_only_head_variables() {
    let file = check_ok(
        r#"source(F::return) :- fun(F, name = N), param(F, I), !(fun(F, parent = P) && P = "x"),
                                fun(G, name = "other");"#,
    );
    let vars: Vec<&Vec<String>> = file.plans[0].components.iter().map(|c| &c.vars).collect();
    assert_eq!(vars, vec![&vec!["F".to_string()], &Vec::<String>::new()]);
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
        .unwrap_or_else(|e| panic!("migrating {json}: {e:?}"))
}

/// The errors a migration fails with, rendered. Panics if it succeeds.
fn migration_errors(json: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(json).expect("valid JSON fixture");
    match migrate::migrate_generators(std::iter::once(&value), None) {
        Ok((text, _)) => panic!("expected {json} not to migrate, got:\n{text}"),
        Err(crate::error::Error::JsonModel(errors)) => {
            errors.iter().map(ToString::to_string).collect()
        }
        Err(e) => panic!("expected JSON model errors, got {e}"),
    }
}

/// What the JSON loader says about the same generator, rendered, so a test can require the
/// migration to fail exactly as loading it would.
fn loader_errors(json: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(json).expect("valid JSON fixture");
    let program = ctadl_ir::mir::ProgramInfo::default();
    let index =
        crate::models::ProgramMatchIndex::new(&program, crate::models::ImportScope::unknown());
    let mut out = crate::models::ProgramModelMatches::default();
    match crate::models::try_load_models_from_values(&index, std::iter::once(Ok(value)), &mut out) {
        Ok(_) => Vec::new(),
        Err(crate::error::Error::Context { source, .. }) => match source.downcast_ref() {
            Some(crate::error::Error::JsonModel(errors)) => {
                errors.iter().map(ToString::to_string).collect()
            }
            _ => panic!("expected JSON model errors, got {source}"),
        },
        Err(e) => panic!("expected JSON model errors, got {e}"),
    }
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

/// Keys the JSON loader accepts and does nothing with are warnings, and the rest of the generator
/// is translated as if they were not there: nothing is dropped from the body.
#[test]
fn inert_keys_warn_and_change_nothing() {
    let (text, report) = migrate_one(
        r#"{"find":"methods","where":[{"constraint":"signature_match","name":"x"}],
            "on-unmatched":"ignore",
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}],
                     "taint":[{"anything":"goes"}],
                     "modes":[],
                     "forward_self":{"where":[]}}}"#,
    );
    assert_eq!(report.warnings.len(), 4, "{:?}", report.warnings);
    for key in ["'taint'", "'modes'", "'forward_self'", "'on-unmatched'"] {
        assert!(
            report.warnings.iter().any(|w| w.contains(key)),
            "{key}: {:?}",
            report.warnings
        );
    }
    assert_eq!(report.rules, 1, "{text}");
    assert!(
        text.contains("sink(F::arg(0), kind = \"k\") :-\n  fun(F, name = \"x\");"),
        "{text}"
    );
    check_ok(&text);
}

/// A generator the JSON loader rejects fails to migrate with the loader's own errors, rather
/// than a warning and a rule missing the part that was wrong. The old behavior for every one of
/// these was a warning and a rule matching every function the rest of the `where` allowed.
#[test]
fn what_the_json_loader_rejects_does_not_migrate() {
    let cases = [
        // An unknown constraint, even under `not`.
        r#"{"find":"methods","where":[{"constraint":"not","inner":{"constraint":"bogus"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"not"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"name"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"name","pattern":"["}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"signature","name":".*x.*"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"signature_match"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"signature_match","name":"x","extends":"Y"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"has_code"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"uses_field"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"any_of"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"number_parameters",
                                       "inner":{"constraint":">","value":"2"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"parent","inner":{"constraint":"name"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"in_function",
                                       "inner":{"constraint":"name","pattern":"x"}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"lambdas","model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"where":[],"model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","wehre":[{"constraint":"name","pattern":"x"}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":{"constraint":"name","pattern":"x"},
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","model":{"sinks":[{"kind":"k","port":"Argument(x)"}]}}"#,
        r#"{"find":"methods","model":{"sinks":[{"kind":"k"}]}}"#,
        r#"{"find":"methods","model":{"sinks":[{"port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","model":{"sources":[{"kind":"k","port":"Return","wildcard":false}]}}"#,
        r#"{"find":"methods","model":{"propagation":[{"input":"Argument(0)"}]}}"#,
        r#"{"find":"methods","model":{"propagations":[]}}"#,
        r#"{"find":"methods","model":{"access_paths":[7]}}"#,
        r#"{"find":"methods","model":{"modes":["skip_analysis"]}}"#,
        r#"{"find":"callsites","model":{"propagation":[{"input":"Argument(0)","output":"Return"}]}}"#,
        r#"{"find":"callsites","model":{"sinks":[{"kind":"k","port":"Variable(x)"}]}}"#,
        r#"{"find":"dispatch","model":{}}"#,
        r#"{"find":"dispatch","model":{"resolve":"cha"}}"#,
    ];
    for json in cases {
        let loader = loader_errors(json);
        assert!(!loader.is_empty(), "the JSON loader accepts {json}");
        assert_eq!(migration_errors(json), loader, "{json}");
    }
}

/// A bridge is scanned before any import is loaded, and the migration runs that scan too.
#[test]
fn a_bridge_the_json_scan_rejects_does_not_migrate() {
    let cases = [
        (
            r#""find":"callsites""#,
            r#""from":"Argument(0)","to":"Argument(0)""#,
        ),
        (
            r#""find":"methods""#,
            r#""from":"Argument(*)","to":"Argument(0)""#,
        ),
        (
            r#""find":"methods""#,
            r#""from":"Argument(0)","to":"Argument(0)","direction":"up""#,
        ),
        (
            r#""find":"methods""#,
            r#""from":"Variable(x)","to":"Argument(0)""#,
        ),
    ];
    for (find, pair) in cases {
        let json = format!(
            r#"{{{find},"where":[{{"constraint":"signature_match","name":"a"}}],
                "model":{{"bridge":{{"to":{{"where":[{{"constraint":"signature_match","name":"b"}}]}},
                                     "arguments":[{{{pair}}}]}}}}}}"#
        );
        let errors = migration_errors(&json);
        assert!(
            errors.iter().all(|e| !e.starts_with("cannot migrate")),
            "{json}: {errors:?}"
        );
    }
    // Side B's `where` is checked as the generator's own is.
    let errors = migration_errors(
        r#"{"find":"methods","where":[{"constraint":"signature_match","name":"a"}],
            "model":{"bridge":{"to":{"where":[{"constraint":"bogus"}]},
                               "arguments":[{"from":"Argument(0)","to":"Argument(0)"}]}}}"#,
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("'bogus'"), "{errors:?}");
}

/// What the JSON loader accepts and the DSL cannot say fails to migrate, naming the generator and
/// the construct, rather than migrating to something that means less or more.
#[test]
fn what_the_dsl_cannot_say_does_not_migrate() {
    let bridge = |extra: &str| {
        format!(
            r#"{{"find":"methods","where":[{{"constraint":"signature_match","name":"a"}}],
                "model":{{"bridge":{{"to":{{"where":[{{"constraint":"signature_match","name":"b"}}]}}{extra}}}}}}}"#
        )
    };
    let cases: Vec<(String, &str)> = vec![
        (
            r#"{"find":"methods","model":{"sources":[{"kind":"k","port":"Variable(buf)"}]}}"#
                .to_string(),
            "port \"Variable(buf)\"",
        ),
        (
            r#"{"find":"methods","model":{"modes":["skip-analysis"]}}"#.to_string(),
            "'modes'",
        ),
        (
            r#"{"find":"dispatch","model":{"resolve":"inline",
                "propagation":[{"input":"Argument(0)","output":"Return"}]}}"#
                .to_string(),
            "'resolve' with 'propagation'",
        ),
        (
            r#"{"find":"callsites","where":[{"constraint":"any_of","inners":[
                {"constraint":"in_function","inner":{"constraint":"name","pattern":"x"}}]}],
                "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#
                .to_string(),
            "'in_function'",
        ),
        (
            r#"{"find":"callsites","where":[{"constraint":"not","inner":
                {"constraint":"in_function","inner":{"constraint":"name","pattern":"x"}}}],
                "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#
                .to_string(),
            "'in_function'",
        ),
        (
            r#"{"find":"methods","where":[{"constraint":"parent","inner":{"constraint":"not",
                "inner":{"constraint":"signature_match","name":"LA;"}}}],
                "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#
                .to_string(),
            "'not' as a class predicate",
        ),
        (bridge(""), "'bridge'"),
        (bridge(r#","arguments":[]"#), "'bridge'"),
        (
            bridge(
                r#","arguments":[{"from":"Argument(0)","to":"Argument(0)"}],"on-ambiguous":"error""#,
            ),
            "'on-ambiguous': \"error\"",
        ),
    ];
    for (json, what) in cases {
        if !json.contains("bridge") {
            assert!(
                loader_errors(&json).is_empty(),
                "the JSON loader rejects {json}"
            );
        }
        let errors = migration_errors(&json);
        assert_eq!(errors.len(), 1, "{json}: {errors:?}");
        let want = format!("cannot migrate {what} in model generator at index 0: ");
        assert!(errors[0].starts_with(&want), "{json}: {errors:?}");
    }
}

/// Errors accumulate across generators, each naming its own index, and one bad generator does
/// not hide another.
#[test]
fn migration_errors_name_every_bad_generator() {
    let values: Vec<serde_json::Value> = [
        r#"{"find":"methods","where":[{"constraint":"name","pattern":"ok"}],"model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
        r#"{"find":"methods","where":[{"constraint":"bogus"}],"model":{}}"#,
        r#"{"find":"methods","model":{"sources":[{"kind":"k","port":"Variable(v)"}]}}"#,
    ]
    .iter()
    .map(|l| serde_json::from_str(l).expect("valid json"))
    .collect();
    let Err(crate::error::Error::JsonModel(errors)) =
        migrate::migrate_generators(values.iter(), None)
    else {
        panic!("expected the migration to fail");
    };
    let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].contains("'bogus'") && errors[0].contains("index 1"),
        "{errors:?}"
    );
    assert!(
        errors[1].contains("Variable") && errors[1].contains("index 2"),
        "{errors:?}"
    );
}

/// A field that is present and names nothing narrows to nothing in the JSON loader. These used
/// to be "skipped", which kept the rest of the rule and so matched what it alone allowed.
#[test]
fn a_where_that_can_never_match_writes_no_rule() {
    for where_ in [
        r#"[{"constraint":"signature_match","name":"a","parents":[]}]"#,
        r#"[{"constraint":"uses_field","names":[]}]"#,
        r#"[{"constraint":"any_of","inners":[]}]"#,
        r#"[{"constraint":"not","inner":{"constraint":"all_of","inners":[]}}]"#,
        r#"[{"constraint":"number_parameters","inner":{"constraint":"any_of","inners":[]}}]"#,
        r#"[{"constraint":"not","inner":{"constraint":"number_parameters",
             "inner":{"constraint":"all_of","inners":[]}}}]"#,
    ] {
        let (text, report) = migrate_one(&format!(
            r#"{{"find":"methods","where":{where_},
                "model":{{"sinks":[{{"kind":"k","port":"Argument(0)"}}]}}}}"#
        ));
        assert_eq!(report.rules, 0, "{where_}:\n{text}");
        assert!(text.contains("can never match"), "{where_}:\n{text}");
    }
    // The other constant, for contrast: it matches everything and keeps the rule.
    let text = migrate_and_load(
        r#"{"find":"methods","where":[{"constraint":"not","inner":{"constraint":"any_of","inners":[]}}],
            "model":{"sinks":[{"kind":"k","port":"Argument(0)"}]}}"#,
    );
    assert!(text.contains(":-\n  fun(F);"), "{text}");
}

/// A disjunction in a bridge's `to` block is kept inside the one rule, so a function matching
/// two alternatives is still bridged once, and every port of the map lands in that rule so the
/// engine makes one bridge of them.
#[test]
fn a_bridge_is_one_rule_whatever_its_disjunctions() {
    let (text, report) = migrate_one(
        r#"{"find":"methods",
            "where":[{"constraint":"any_of","inners":[
              {"constraint":"signature_match","name":"a1"},
              {"constraint":"name","pattern":"^a2"}]}],
            "model":{"bridge":{
              "to":{"where":[{"constraint":"any_of","inners":[
                {"constraint":"signature_match","name":"b1"},
                {"constraint":"name","pattern":"^b2"}]}]},
              "arguments":[{"from":"Argument(0)","to":"Argument(1)"},
                           {"from":"Return","to":"Return","direction":"out"}]}}}"#,
    );
    check_ok(&text);
    assert_eq!(report.rules, 1, "{text}");
    assert!(
        text.contains(
            "fun(F),\n  !(!fun(F, name = \"a1\") && !(fun(F, name = N0) && regex_match(N0, \"^a2\")))"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "fun(G),\n  !(!fun(G, name = \"b1\") && !(fun(G, name = N1) && regex_match(N1, \"^b2\")))"
        ),
        "{text}"
    );
    // An absent direction is both ways, as it is in the JSON loader.
    assert!(text.contains("bridge(F::arg(0) <-> G::arg(1))"), "{text}");
    assert!(text.contains("bridge(F::return <- G::return)"), "{text}");
}

/// A native program of one-parameter functions with bodies, one per name.
fn native_program(names: &[&str]) -> ctadl_ir::mir::ProgramInfo {
    use ctadl_ir::mir::call::{
        NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };
    use ctadl_ir::mir::{
        BasicBlockData, FunctionData, Functions, ParameterType, Program, ProgramInfo, Statement,
        StatementKind,
    };

    let functions = names.iter().map(|name| {
        let mut f = FunctionData::default();
        f.set_name(name.to_string());
        f.params.parameters.push(ParameterType::SharedFields);
        let blocks = f.blocks.blocks_mut();
        let body = blocks.push(BasicBlockData::new(None));
        blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
        f
    });
    let methods = names
        .iter()
        .map(|n| {
            (
                NativeSimpleName((*n).into()),
                NativeSignature(format!("{n}()").into()),
                NativeFunction((*n).into()),
                NativeQualifiedName((*n).into()),
            )
        })
        .collect();
    ProgramInfo {
        vmt: VirtualMethodTable::Native { methods },
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

/// Migrates `json` and runs the result over one import, as an index would.
fn run_migrated(
    json: &serde_json::Value,
    index: &crate::models::ProgramMatchIndex<'_>,
) -> (String, crate::models::ProgramModelMatches) {
    let (text, _) = migrate::migrate_generators(std::iter::once(json), None)
        .unwrap_or_else(|e| panic!("{json}: {e:?}"));
    let set = DslModelSet {
        files: vec![check_ok(&text)],
    };
    let mut out = crate::models::ProgramModelMatches::default();
    let mut matcher = DslMatcher::new(&set, Phase::All);
    matcher.observe_import(index, &mut out);
    matcher
        .finish(&mut out)
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    (text, out)
}

/// The engine-level half of [`a_where_that_can_never_match_writes_no_rule`]: on a real program
/// the JSON loader and the migrated rules select the same functions, for the shapes that used to
/// migrate wider than the original.
#[test]
fn constructs_that_used_to_widen_match_what_the_json_matched() {
    use crate::models::{ImportScope, ProgramMatchIndex, ProgramModelMatches};
    use std::collections::BTreeSet;

    let program = native_program(&["a", "b", "c"]);
    let index = ProgramMatchIndex::new(&program, ImportScope::unknown());
    let summarized = |m: &ProgramModelMatches| -> BTreeSet<String> {
        m.propagations
            .iter()
            .map(|p| p.function.to_string())
            .collect()
    };

    for where_ in [
        r#"[{"constraint":"signature_match","name":"a","parents":[]}]"#,
        r#"[{"constraint":"signature_match","name":"a","qualified-ids":[1]}]"#,
        r#"[{"constraint":"uses_field","names":[]}]"#,
        r#"[{"constraint":"not","inner":{"constraint":"number_parameters",
             "inner":{"constraint":"all_of","inners":[]}}}]"#,
        r#"[{"constraint":"not","inner":{"constraint":"any_of","inners":[]}}]"#,
        r#"[{"constraint":"number_parameters","inner":{"constraint":"any_of","inners":[
             {"constraint":"all_of","inners":[]},{"constraint":"<","value":0}]}}]"#,
    ] {
        let json: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"find":"methods","where":{where_},
                "model":{{"propagation":[{{"input":"Argument(0)","output":"Return"}}]}}}}"#
        ))
        .expect("valid json");
        let mut from_json = ProgramModelMatches::default();
        crate::models::try_load_models_from_values(
            &index,
            std::iter::once(Ok(json.clone())),
            &mut from_json,
        )
        .unwrap_or_else(|e| panic!("{where_}: {e}"));
        let (text, from_dsl) = run_migrated(&json, &index);
        assert_eq!(
            summarized(&from_json),
            summarized(&from_dsl),
            "{where_}:\n{text}"
        );
    }
}

/// [`a_bridge_is_one_rule_whatever_its_disjunctions`] on a program: the JSON loader bridges the
/// cross product of the two sides' match *sets*, once per pair, with the whole port map. A
/// function satisfying both alternatives of a side is in the set once.
#[test]
fn a_migrated_bridge_pairs_what_the_json_paired() {
    use crate::models::{ImportScope, ProgramMatchIndex};
    use std::collections::BTreeMap;

    let program = native_program(&["a1", "a2", "b1", "b2", "c"]);
    let index = ProgramMatchIndex::new(&program, ImportScope::unknown());
    let json: serde_json::Value = serde_json::from_str(
        r#"{"find":"methods",
            "where":[{"constraint":"any_of","inners":[
              {"constraint":"signature_match","name":"a1"},
              {"constraint":"name","pattern":"^a"}]}],
            "model":{"bridge":{
              "to":{"where":[{"constraint":"any_of","inners":[
                {"constraint":"signature_match","name":"b1"},
                {"constraint":"name","pattern":"^b"}]}]},
              "arguments":[{"from":"Argument(0)","to":"Argument(0)"},
                           {"from":"Return","to":"Return","direction":"out"}]}}}"#,
    )
    .expect("valid json");
    let (text, out) = run_migrated(&json, &index);
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    for bridge in &out.resolved_bridges {
        assert_eq!(bridge.ports.len(), 2, "{text}\n{bridge:?}");
        *pairs
            .entry((bridge.from.to_string(), bridge.to.to_string()))
            .or_default() += 1;
    }
    let want: BTreeMap<(String, String), usize> =
        [("a1", "b1"), ("a1", "b2"), ("a2", "b1"), ("a2", "b2")]
            .into_iter()
            .map(|(a, b)| ((a.to_string(), b.to_string()), 1))
            .collect();
    assert_eq!(pairs, want, "{text}");
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

/// The shipped default generators the DSL cannot yet say, as `(file, generator index, what the
/// error names)`. Each is a model the checked-in `.ctadl` does not carry.
///
/// - `skip-analysis` has no DSL head, so an index loading the `.ctadl` lowers the unwinders'
///   bodies after all.
/// - `resolve: "inline"` with a `propagation` list inlines *and* summarizes, and the DSL's
///   `resolve = "inline"` head carries no flows, so the `.ctadl` inlines without the summary.
const KNOWN_UNTRANSLATABLE_DEFAULTS: &[(&str, usize, &str)] = &[
    ("native-index.jsonl", 28, "'modes'"),
    ("cil-index.jsonl", 194, "'resolve' with 'propagation'"),
    ("cil-index.jsonl", 195, "'resolve' with 'propagation'"),
];

/// Every shipped default file must migrate and load, but for the generators
/// [`KNOWN_UNTRANSLATABLE_DEFAULTS`] lists, which must fail exactly as listed. This is the drift
/// guard: a keyword added to the JSON loader and used in a default has to be understood here
/// too, and a gap that closes has to come off the list.
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
        let known: Vec<(usize, &str)> = KNOWN_UNTRANSLATABLE_DEFAULTS
            .iter()
            .filter(|(file, _, _)| file == name)
            .map(|&(_, index, what)| (index, what))
            .collect();
        let errors: Vec<String> = match migrate::migrate_generators(values.iter(), Some(name)) {
            Ok(_) => Vec::new(),
            Err(crate::error::Error::JsonModel(errors)) => {
                errors.iter().map(ToString::to_string).collect()
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let expected: Vec<String> = known
            .iter()
            .map(|(index, what)| {
                format!("cannot migrate {what} in model generator at index {index}")
            })
            .collect();
        assert_eq!(errors.len(), expected.len(), "{name}: {errors:#?}");
        for (error, expected) in errors.iter().zip(&expected) {
            assert!(error.starts_with(expected), "{name}: {error}");
        }
        // The rest must translate and load.
        let rest: Vec<&serde_json::Value> = values
            .iter()
            .enumerate()
            .filter(|(i, _)| !known.iter().any(|(k, _)| k == i))
            .map(|(_, v)| v)
            .collect();
        let (dsl, report) = migrate::migrate_generators(rest.iter().copied(), Some(name))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        // The only warnings a shipped default may raise are about keys the JSON loader does
        // not act on either, so nothing is lost by not translating them. Anything else means a
        // construct the defaults use and the migrator does not understand.
        for warning in &report.warnings {
            assert!(
                ["'taint'", "'forward_self'"]
                    .iter()
                    .any(|k| warning.contains(k)),
                "{name}: {warning}"
            );
        }
        assert_eq!(report.generators, rest.len(), "{name}");
        if let Err(e) = DslFile::from_text(*name, dsl.clone()) {
            panic!("migrated {name} does not load: {e}\n---\n{dsl}");
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[test]
fn a_dispatch_head_takes_a_flow_or_one_attribute() {
    check_ok(r#"K::dispatch(return <- arg(0)) :- callsig(K, name = "toString");"#);
    check_ok(r#"K::dispatch(resolve = "inline") :- callsig(K, name = "close");"#);
    check_ok(r#"K::dispatch(resolve = "skip") :- callsig(K, name = "size");"#);
    check_ok(
        r#"K::dispatch(closure_shaped = true) :- callsig(K, parent = "Ljava/lang/Runnable;");"#,
    );
}

/// The three forms are alternatives, not a bag: a signature that is modelled is not also
/// skipped. This is the DSL's counterpart of the JSON loader's "exactly one of 'propagation',
/// 'resolve' and 'closure_shaped'".
#[test]
fn a_dispatch_head_says_exactly_one_thing() {
    let err =
        check_err(r#"K::dispatch(return <- arg(0), resolve = "skip") :- callsig(K, name = "x");"#);
    assert!(err.contains("never both"), "{err}");

    let err = check_err(
        r#"K::dispatch(resolve = "inline", closure_shaped = true) :- callsig(K, name = "x");"#,
    );
    assert!(err.contains("exactly one thing"), "{err}");

    let err = check_err(r#"K::dispatch() :- callsig(K, name = "x");"#);
    assert!(err.contains("needs a flow"), "{err}");
}

#[test]
fn unknown_dispatch_values_are_named() {
    let err = check_err(r#"K::dispatch(resolve = "cha") :- callsig(K, name = "x");"#);
    assert!(err.contains("expected \"inline\" or \"skip\""), "{err}");
}

/// A dispatch model attaches to a call *signature*, which is a different universe from `fun`:
/// half an app's interface call sites name a type that has no method-table row at all.
#[test]
fn a_dispatch_head_anchors_at_a_callsig_not_a_function() {
    let err = check_err(r#"F::dispatch(resolve = "skip") :- fun(F, name = "x");"#);
    assert!(err.contains("cannot anchor a 'dispatch' head"), "{err}");
}

#[test]
fn a_dispatch_model_is_one_signatures_summary() {
    let err = check_err(r#"dispatch(K::return <- J::arg(0)) :- callsig(K), callsig(J);"#);
    assert!(
        err.contains("both ports must be anchored at the same 'callsig' key"),
        "{err}"
    );
}

/// `callsig` is an input relation and `dispatch` an output one, so neither can stand in for the
/// other. Getting this wrong is the likeliest typo in a hand-written dispatch rule.
#[test]
fn callsig_and_dispatch_do_not_swap() {
    let err = check_err(r#"K::dispatch(resolve = "skip") :- dispatch(K);"#);
    assert!(err.contains("can only appear in a rule head"), "{err}");

    let err = check_err(r#"K::callsig(resolve = "skip") :- callsig(K);"#);
    assert!(err.contains("expected output relation"), "{err}");
}

#[test]
fn callsig_has_no_body_dependent_attributes() {
    let err = check_err(r#"K::dispatch(resolve = "skip") :- callsig(K, has_code = true);"#);
    assert!(err.contains("not an attribute of 'callsig'"), "{err}");
}

/// The migrator's three dispatch shapes, including the empty `propagation` list that the JSON
/// format spells a discard with and this language spells `resolve = "skip"`.
#[test]
fn dispatch_generators_migrate() {
    let values: Vec<serde_json::Value> = [
        r#"{"find":"dispatch","where":[{"constraint":"signature_match","name":"toString"}],"model":{"propagation":[{"input":"Argument(*)","output":"Return"}]}}"#,
        r#"{"find":"dispatch","where":[{"constraint":"signature_match","name":"size"}],"model":{"propagation":[]}}"#,
        r#"{"find":"dispatch","where":[{"constraint":"signature_match","name":"close"}],"model":{"resolve":"inline"}}"#,
        r#"{"find":"dispatch","where":[{"constraint":"signature_match","parents":["Ljava/lang/Runnable;"]}],"model":{"closure_shaped":true}}"#,
    ]
    .iter()
    .map(|l| serde_json::from_str(l).expect("valid json"))
    .collect();
    let (dsl, report) =
        migrate::migrate_generators(values.iter(), None).expect("dispatch generators migrate");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(dsl.contains(r#"K::dispatch(return <- arg(_))"#), "{dsl}");
    assert!(dsl.contains(r#"K::dispatch(resolve = "skip")"#), "{dsl}");
    assert!(dsl.contains(r#"K::dispatch(resolve = "inline")"#), "{dsl}");
    assert!(
        dsl.contains(r#"K::dispatch(closure_shaped = true)"#),
        "{dsl}"
    );
    assert!(dsl.contains("callsig(K, "), "{dsl}");
    check_ok(&dsl);
}

/// `in` narrows by language and import, and a signature key is composed from a call site rather
/// than read out of an import's method table, so it carries neither. Dropping the scope would
/// apply the model to every import, so the migration refuses.
#[test]
fn a_scoped_dispatch_generator_does_not_migrate() {
    let errors = migration_errors(
        r#"{"find":"dispatch","in":{"language":"dex"},"where":[{"constraint":"signature_match","name":"toString"}],"model":{"resolve":"inline"}}"#,
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("cannot migrate 'in'"), "{errors:?}");
    assert!(errors[0].contains("index 0"), "{errors:?}");
}
