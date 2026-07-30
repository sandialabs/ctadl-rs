use ctadl_ascent::error::{Error, JsonModelError};
use ctadl_ascent::models::ModelBuilders;
use ctadl_ascent::models::json::ModelGeneratorIngest;
use ctadl_ascent::models::{ImportScope, ProgramMatchIndex};
use ctadl_ir::mir::ProgramInfo;
use serde_json::json;

#[test]
fn test_missing_field_error() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    // Test individual model generator (this is what encode_models expects)
    let malformed_json = json!({
        "find": "methods",
        "model": {
            "propagation": [
                {
                    // Missing "output" field
                    "input": "Argument(0)"
                }
            ]
        }
    });

    let result = ingest.encode_models(vec![malformed_json]);
    match result {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            if let JsonModelError::MissingField { index, field_name } = &errors[0] {
                assert_eq!(*index, 0);
                assert_eq!(field_name, "output");
            } else {
                panic!("Expected MissingField error, got: {:?}", errors[0]);
            }
        }
        Ok(_) => panic!("Expected error for missing field, but got success"),
        Err(e) => panic!("Expected JsonModel error, but got: {}", e),
    }
}

#[test]
fn test_invalid_regex_error() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let malformed_json = json!({
        "find": "methods",
        "where": [
            {
                "constraint": "signature",
                "pattern": "[", // Invalid regex
            }
        ],
        "model": {}
    });

    let result = ingest.encode_models(vec![malformed_json]);
    match result {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            if let JsonModelError::InvalidRegex { index, pattern, .. } = &errors[0] {
                assert_eq!(*index, 0);
                assert_eq!(pattern, "[");
            } else {
                panic!("Expected InvalidRegex error, got: {:?}", errors[0]);
            }
        }
        Ok(_) => panic!("Expected error for invalid regex, but got success"),
        Err(e) => panic!("Expected JsonModel error, but got: {}", e),
    }
}

#[test]
fn test_field_not_string_error() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let malformed_json = json!({
        "find": 123, // Should be a string
        "model": {}
    });

    let result = ingest.encode_models(vec![malformed_json]);
    match result {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            if let JsonModelError::FieldNotString { index, field_name } = &errors[0] {
                assert_eq!(*index, 0);
                assert_eq!(field_name, "find");
            } else {
                panic!("Expected FieldNotString error, got: {:?}", errors[0]);
            }
        }
        Ok(_) => panic!("Expected error for field type mismatch, but got success"),
        Err(e) => panic!("Expected JsonModel error, but got: {}", e),
    }
}

#[test]
fn test_valid_json_still_works() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    // This should not produce errors, just not match any methods
    let valid_json = json!({
        "find": "methods",
        "model": {
            "sources": [
                {
                    "kind": "test",
                    "port": "Argument(0)"
                }
            ]
        }
    });

    let result = ingest.encode_models(vec![valid_json]);
    assert!(result.is_ok(), "Valid JSON should not produce errors");
}

/// `find: callsites` matches the callee via `signature_match` and the containing (caller)
/// function via `in_function`, emitting one callsite-scoped endpoint carrying both.
#[test]
fn test_callsites_matches_callee_and_caller() {
    use ctadl_ir::mir::call::{
        NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };

    let method = |n: &str| {
        (
            NativeSimpleName(n.into()),
            NativeSignature(n.into()),
            NativeFunction(n.into()),
            NativeQualifiedName(n.into()),
        )
    };
    let program_info = ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: vec![method("get"), method("read_http_data"), method("unrelated")],
        },
        ..Default::default()
    };
    let mut model_builders = ModelBuilders::new();
    {
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
        let model = json!({
            "find": "callsites",
            "where": [
                {"constraint": "signature_match", "name": "get"},
                {"constraint": "in_function",
                 "inner": {"constraint": "signature_match", "name": "read_http_data"}}
            ],
            "model": {"sinks": [{"kind": "TestSink", "port": "Argument(0)"}]}
        });
        ingest
            .encode_models(vec![model])
            .expect("callsites model should load");
    }

    let batch = model_builders.endpoint.finish().expect("finish endpoints");
    let rows: Vec<_> = batch.iter_endpoints().collect();
    assert_eq!(rows.len(), 1, "expected exactly one callsite endpoint row");
    let row = rows[0];
    assert_eq!(row.function, "get", "endpoint callee");
    assert_eq!(
        row.in_function,
        Some("read_http_data"),
        "endpoint containing/caller function"
    );
    assert!(row.callsite_scoped, "endpoint should be callsite-scoped");
    assert_eq!(row.label, "TestSink");
}

/// Propagation is a function-level fact and is not supported for `find: callsites`.
#[test]
fn test_callsites_propagation_rejected() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let model = json!({
        "find": "callsites",
        "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
    });

    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => {
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    JsonModelError::UnexpectedField { field_name, .. } if field_name == "propagation"
                )),
                "expected a propagation-not-supported error, got: {errors:?}"
            );
        }
        other => panic!("expected propagation to be rejected, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Where-constraint evaluation: combinators + leaf constraints.
//
// These build real ProgramInfos and assert on the set of endpoint functions a
// generator emits for, which is exactly the set of functions its `where`
// constraints match.
// ---------------------------------------------------------------------------

use ctadl_ir::mir::call::{
    JavaClass, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction, NativeQualifiedName,
    NativeSignature, NativeSimpleName, VirtualMethodTable,
};
use ctadl_ir::mir::{
    AccessPath, BasicBlockData, Exp, FieldPath, FunctionData, Functions, ParameterIdx,
    ParameterType, Program, Statement, StatementKind, VariableRef,
};
use std::collections::BTreeSet;

/// Builds a `FunctionData` named `name` with `num_params` parameters and, if any `fields` are
/// given (or `force_body`), a single basic block writing each named field.
fn make_function(name: &str, num_params: usize, force_body: bool, fields: &[&str]) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    for _ in 0..num_params {
        f.params.parameters.push(ParameterType::ByVal);
    }
    if force_body || !fields.is_empty() {
        let blocks = f.blocks.blocks_mut();
        let body = blocks.push(BasicBlockData::new(None));
        let p: VariableRef = VariableRef::new_parameter(ParameterIdx::from(0u16));
        let mut stmts: Vec<Statement> = fields
            .iter()
            .map(|field| {
                Statement::new_kind(StatementKind::store(
                    AccessPath::without_fields(p.clone()),
                    FieldPath::symbol(*field),
                    Exp::from(AccessPath::without_fields(p.clone())),
                ))
            })
            .collect();
        if stmts.is_empty() {
            stmts.push(Statement::new_kind(StatementKind::Nop));
        }
        blocks[body].extend(stmts);
    }
    f
}

/// A native (binary-frontend) program with functions `a`, `b`, `c`:
/// - `a`: 1 parameter, a body that writes the field `secret`
/// - `b`, `c`: 0 parameters, no body
fn native_program() -> ProgramInfo {
    let methods = |n: &str| {
        (
            NativeSimpleName(n.into()),
            NativeSignature(n.into()),
            NativeFunction(n.into()),
            NativeQualifiedName(n.into()),
        )
    };
    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: vec![methods("a"), methods("b"), methods("c")],
        },
        program: Program::new(Functions::new([
            make_function("a", 1, true, &["secret"]),
            make_function("b", 0, false, &[]),
            make_function("c", 0, false, &[]),
        ])),
        ..Default::default()
    }
}

/// A Java program with methods `Child.m` (class `Child extends Base`) and `Other.n` (class
/// `Other`, no recorded hierarchy).
fn java_program() -> ProgramInfo {
    ProgramInfo {
        vmt: VirtualMethodTable::Java {
            methods: vec![
                (
                    JavaClass("Child".into()),
                    JavaSimpleName("m".into()),
                    JavaSignature("()V".into()),
                    JavaMethod("Child.m".into()),
                ),
                (
                    JavaClass("Other".into()),
                    JavaSimpleName("n".into()),
                    JavaSignature("()V".into()),
                    JavaMethod("Other.n".into()),
                ),
            ],
            hierarchy: [(
                JavaClass("Child".into()),
                [JavaClass("Base".into())].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            natives: Vec::new(),
        },
        ..Default::default()
    }
}

/// Runs a `find: methods` generator whose `where` is `where_c` and a source model, returning the
/// set of functions endpoints were emitted for (== the functions the `where` matched).
fn matched_functions(program_info: &ProgramInfo, where_c: serde_json::Value) -> BTreeSet<String> {
    let mut model_builders = ModelBuilders::new();
    {
        let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
        let model = json!({
            "find": "methods",
            "where": where_c,
            "model": {"sources": [{"kind": "K", "port": "Argument(0)"}]},
        });
        ingest
            .encode_models(vec![model])
            .expect("model should load");
    }
    let batch = model_builders.endpoint.finish().expect("finish endpoints");
    batch
        .iter_endpoints()
        .map(|r| r.function.to_string())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn any_of_is_union() {
    // Regression: `any_of` used to intersect (AND) its inners and collapse to empty.
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "any_of", "inners": [
            {"constraint": "name", "pattern": "^a$"},
            {"constraint": "name", "pattern": "^b$"},
        ]}]),
    );
    assert_eq!(got, set(&["a", "b"]));
}

#[test]
fn all_of_is_intersection() {
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "all_of", "inners": [
            {"constraint": "name", "pattern": "a|b"},
            {"constraint": "name", "pattern": "b|c"},
        ]}]),
    );
    assert_eq!(got, set(&["b"]));
}

#[test]
fn not_complements_against_universe() {
    // Top-level `not` materializes the whole function universe, then subtracts.
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "not", "inner": {"constraint": "name", "pattern": "^a$"}}]),
    );
    assert_eq!(got, set(&["b", "c"]));
}

#[test]
fn nested_not_in_any_of_composes() {
    // any_of[ not(a), a ] covers the whole universe (scratch composition sanity).
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "any_of", "inners": [
            {"constraint": "not", "inner": {"constraint": "name", "pattern": "^a$"}},
            {"constraint": "name", "pattern": "^a$"},
        ]}]),
    );
    assert_eq!(got, set(&["a", "b", "c"]));
}

#[test]
fn name_matches_regex() {
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "name", "pattern": "^a$"}]),
    );
    assert_eq!(got, set(&["a"]));
}

#[test]
fn number_parameters_compares_arity() {
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "number_parameters",
                "inner": {"constraint": ">=", "value": 1}}]),
    );
    assert_eq!(got, set(&["a"]));
}

#[test]
fn has_code_matches_bodied_functions() {
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "has_code", "value": true}]),
    );
    assert_eq!(got, set(&["a"]));
}

#[test]
fn uses_field_matches_load_store() {
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "uses_field", "name": "secret"}]),
    );
    assert_eq!(got, set(&["a"]));
}

#[test]
fn parent_matches_owning_class_java() {
    let got = matched_functions(
        &java_program(),
        json!([{"constraint": "parent",
                "inner": {"constraint": "name", "pattern": "^Child$"}}]),
    );
    assert_eq!(got, set(&["Child.m"]));
}

#[test]
fn extends_matches_superclass_java() {
    let got = matched_functions(
        &java_program(),
        json!([{"constraint": "extends",
                "inner": {"constraint": "name", "pattern": "^Base$"}}]),
    );
    assert_eq!(got, set(&["Child.m"]));
}

#[test]
fn extends_on_non_java_matches_nothing() {
    // `extends` is Java-only: on a native program it warns and matches nothing (not an error).
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "extends",
                "inner": {"constraint": "name", "pattern": "^Base$"}}]),
    );
    assert!(got.is_empty(), "expected no matches, got: {got:?}");
}

/// Asserts that loading a generator with the given `where` fails with an `UnexpectedConstraint`.
fn assert_unexpected_constraint(where_c: serde_json::Value) {
    let program_info = native_program();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({"find": "methods", "where": where_c, "model": {}});
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => assert!(
            errors
                .iter()
                .any(|e| matches!(e, JsonModelError::UnexpectedConstraint { .. })),
            "expected UnexpectedConstraint, got: {errors:?}"
        ),
        other => panic!("expected UnexpectedConstraint error, got: {other:?}"),
    }
}

#[test]
fn removed_parameter_constraint_is_hard_error() {
    assert_unexpected_constraint(json!([{"constraint": "parameter", "idx": 0,
        "inner": {"constraint": "name", "pattern": "x"}}]));
}

#[test]
fn removed_any_parameter_constraint_is_hard_error() {
    assert_unexpected_constraint(json!([{"constraint": "any_parameter", "start_idx": 0,
        "inner": {"constraint": "name", "pattern": "x"}}]));
}

#[test]
fn unknown_constraint_is_hard_error() {
    assert_unexpected_constraint(json!([{"constraint": "totally_bogus"}]));
}

#[test]
fn top_level_integer_compare_is_hard_error() {
    assert_unexpected_constraint(json!([{"constraint": "==", "value": 1}]));
}

// ---------------------------------------------------------------------------
// Fail-open `where` constraints are hard errors.
//
// A constraint the loader could not act on used to be a silent no-op, and because
// the working set starts as `UniverseSet::all()` a no-op constraint left the
// generator matching *every* function in the program. `CTADL0004` only reports
// generators that matched nothing, so nothing caught it.
// ---------------------------------------------------------------------------

/// Loads a `find: methods` generator with the given `where` against `program_info`, returning
/// the collected model errors. Panics if the load *succeeded* — for these cases that is the
/// bug under test.
fn load_errors_with(program_info: &ProgramInfo, where_c: serde_json::Value) -> Vec<String> {
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({"find": "methods", "where": where_c, "model": {}});
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => errors.iter().map(|e| e.to_string()).collect(),
        other => panic!("expected the model to fail loading, got: {other:?}"),
    }
}

/// Asserts that loading a generator with the given `where` fails with an `UnexpectedField`
/// naming `field_name`.
fn assert_unexpected_field(where_c: serde_json::Value, field_name: &str) {
    let program_info = native_program();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({"find": "methods", "where": where_c, "model": {}});
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => assert!(
            errors.iter().any(|e| matches!(
                e,
                JsonModelError::UnexpectedField { field_name: f, .. } if f == field_name
            )),
            "expected UnexpectedField {field_name:?}, got: {errors:?}"
        ),
        other => panic!("expected UnexpectedField {field_name:?}, got: {other:?}"),
    }
}

/// Asserts that loading a generator with the given `where` fails with a `MissingField`.
fn assert_missing_field(where_c: serde_json::Value) {
    let program_info = native_program();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({"find": "methods", "where": where_c, "model": {}});
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => assert!(
            errors
                .iter()
                .any(|e| matches!(e, JsonModelError::MissingField { .. })),
            "expected MissingField, got: {errors:?}"
        ),
        other => panic!("expected MissingField error, got: {other:?}"),
    }
}

#[test]
fn signature_constraint_keyed_on_name_is_hard_error() {
    // The real typo this whole class of bug was found through: `name` where `pattern` was
    // meant. It used to match every function in the program.
    assert_missing_field(json!([{"constraint": "signature", "name": ".*sink.*"}]));
    assert_unexpected_field(
        json!([{"constraint": "signature", "name": ".*sink.*"}]),
        "name",
    );
}

#[test]
fn signature_match_with_no_selector_is_hard_error() {
    assert_missing_field(json!([{"constraint": "signature_match"}]));
}

#[test]
fn signature_match_with_unknown_key_is_hard_error() {
    // `extends` was documented on `signature_match` but never implemented, so it was
    // silently dropped — leaving the generator matching on `name` alone.
    assert_unexpected_field(
        json!([{"constraint": "signature_match", "name": "a", "extends": "Y"}]),
        "extends",
    );
}

#[test]
fn dropped_unqualified_id_is_hard_error() {
    // `unqualified-id` was a pure alias for `name` on `uses_field` and unimplemented on
    // `signature_match`. Dropping it from the honored key sets makes both loud.
    assert_unexpected_field(
        json!([{"constraint": "signature_match", "unqualified-id": "a"}]),
        "unqualified-id",
    );
    assert_unexpected_field(
        json!([{"constraint": "uses_field", "unqualified-id": "secret"}]),
        "unqualified-id",
    );
}

#[test]
fn uses_field_with_no_selector_is_hard_error() {
    assert_missing_field(json!([{"constraint": "uses_field"}]));
}

#[test]
fn in_function_under_find_methods_is_hard_error() {
    // There is no caller set to narrow under `find: methods`, so this used to vanish.
    assert_unexpected_field(
        json!([{"constraint": "in_function",
                "inner": {"constraint": "name", "pattern": "^a$"}}]),
        "in_function",
    );
}

#[test]
fn in_function_with_no_inner_is_hard_error() {
    assert_missing_field(json!([{"constraint": "in_function"}]));
}

#[test]
fn fail_open_constraint_no_longer_matches_everything() {
    // The assertion that would have caught the original bug. Each of these `where` clauses
    // was a silent no-op, so `matched_functions` returned all of {a, b, c} — a model meant to
    // mark one method as a source became a global source. They must now fail to load.
    for where_c in [
        json!([{"constraint": "signature", "name": ".*sink.*"}]),
        json!([{"constraint": "signature_match"}]),
        json!([{"constraint": "uses_field"}]),
        json!([{"constraint": "in_function",
                "inner": {"constraint": "name", "pattern": "^a$"}}]),
    ] {
        let errors = load_errors_with(&native_program(), where_c.clone());
        assert!(
            !errors.is_empty(),
            "expected {where_c} to be rejected, but it loaded"
        );
    }
}

// ---------------------------------------------------------------------------
// `qualified-id`: exact, whole-string match on a method's fully-qualified id.
// ---------------------------------------------------------------------------

/// A native C++-style program with two same-named methods in different namespaces:
/// `Foo::bar` and `Baz::bar`, both with simple name `bar`, plus a namespace-less `main`.
/// This is the disambiguation `qualified-id` exists for — `name` cannot express it, and
/// `parent` is populated only for the Java VMT.
fn native_cpp_program() -> ProgramInfo {
    let method = |simple: &str, qualified: &str, fq: &str| {
        (
            NativeSimpleName(simple.into()),
            NativeSignature("()".into()),
            NativeFunction(fq.into()),
            NativeQualifiedName(qualified.into()),
        )
    };
    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: vec![
                method("bar", "Foo::bar", "Foo::bar@00101000"),
                method("bar", "Baz::bar", "Baz::bar@00102000"),
                method("main", "main", "main"),
            ],
        },
        program: Program::new(Functions::new([
            make_function("Foo::bar@00101000", 0, false, &[]),
            make_function("Baz::bar@00102000", 0, false, &[]),
            make_function("main", 0, false, &[]),
        ])),
        ..Default::default()
    }
}

#[test]
fn qualified_id_disambiguates_same_named_natives() {
    let program = native_cpp_program();
    // `name` cannot tell the two `bar`s apart...
    assert_eq!(
        matched_functions(
            &program,
            json!([{"constraint": "signature_match", "name": "bar"}])
        ),
        set(&["Foo::bar@00101000", "Baz::bar@00102000"]),
    );
    // ...but `qualified-id` selects exactly one.
    assert_eq!(
        matched_functions(
            &program,
            json!([{"constraint": "signature_match", "qualified-id": "Foo::bar"}])
        ),
        set(&["Foo::bar@00101000"]),
    );
}

#[test]
fn qualified_ids_ors_within_itself() {
    assert_eq!(
        matched_functions(
            &native_cpp_program(),
            json!([{"constraint": "signature_match",
                    "qualified-ids": ["Foo::bar", "main"]}])
        ),
        set(&["Foo::bar@00101000", "main"]),
    );
}

#[test]
fn qualified_id_is_exact_not_regex() {
    // A regex that would match under `name` selects nothing here.
    for id in ["bar", "Foo::.*", "^Foo::bar$", "Foo::ba"] {
        let got = matched_functions(
            &native_cpp_program(),
            json!([{"constraint": "signature_match", "qualified-id": id}]),
        );
        assert!(
            got.is_empty(),
            "expected {id:?} to match nothing, got {got:?}"
        );
    }
}

#[test]
fn qualified_id_accepts_the_verbatim_ir_id() {
    // Models that spell the decorated id out still resolve, as they do for `name`.
    assert_eq!(
        matched_functions(
            &native_cpp_program(),
            json!([{"constraint": "signature_match", "qualified-id": "Foo::bar@00101000"}])
        ),
        set(&["Foo::bar@00101000"]),
    );
}

#[test]
fn qualified_id_for_absent_function_matches_nothing() {
    // Fail-closed: naming a function the program does not have must not match everything.
    let got = matched_functions(
        &native_program(),
        json!([{"constraint": "signature_match", "qualified-id": "nosuchfunction"}]),
    );
    assert!(got.is_empty(), "expected no matches, got: {got:?}");
}

#[test]
fn qualified_id_ands_with_name() {
    // Both keys narrow the same set, so a contradictory pair matches nothing.
    let got = matched_functions(
        &native_cpp_program(),
        json!([{"constraint": "signature_match", "name": "main", "qualified-id": "Foo::bar"}]),
    );
    assert!(got.is_empty(), "expected no matches, got: {got:?}");
}

#[test]
fn qualified_id_matches_every_function_sharing_a_qualified_name() {
    // On pcode the qualified name is address-free, so imported thunks for the same symbol
    // share one: Ghidra's `getName(true)` is `<EXTERNAL>::system` for all of them and only
    // the entry point differs. `qualified-id` therefore selects the whole group, not one
    // function. That is the intended granularity — they are the same logical callee — but it
    // means `qualified-id` does not guarantee a single match on native frontends.
    let method = |qualified: &str, fq: &str| {
        (
            NativeSimpleName("system".into()),
            NativeSignature("()".into()),
            NativeFunction(fq.into()),
            NativeQualifiedName(qualified.into()),
        )
    };
    let program = ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: vec![
                method("<EXTERNAL>::system", "<EXTERNAL>::system@00008d90"),
                method("<EXTERNAL>::system", "<EXTERNAL>::system@00008da4"),
                method("<EXTERNAL>::system", "<EXTERNAL>::system@00008db8"),
            ],
        },
        program: Program::new(Functions::new([
            make_function("<EXTERNAL>::system@00008d90", 0, false, &[]),
            make_function("<EXTERNAL>::system@00008da4", 0, false, &[]),
            make_function("<EXTERNAL>::system@00008db8", 0, false, &[]),
        ])),
        ..Default::default()
    };
    assert_eq!(
        matched_functions(
            &program,
            json!([{"constraint": "signature_match", "qualified-id": "<EXTERNAL>::system"}])
        ),
        set(&[
            "<EXTERNAL>::system@00008d90",
            "<EXTERNAL>::system@00008da4",
            "<EXTERNAL>::system@00008db8",
        ]),
    );
    // Spelling the decorated id out picks exactly one out of the group.
    assert_eq!(
        matched_functions(
            &program,
            json!([{"constraint": "signature_match",
                    "qualified-id": "<EXTERNAL>::system@00008da4"}])
        ),
        set(&["<EXTERNAL>::system@00008da4"]),
    );
}

#[test]
fn qualified_id_matches_java_method_id() {
    // On jvm/dex the id is the `JavaMethod`, which until now was only ever a lookup *value*.
    assert_eq!(
        matched_functions(
            &java_program(),
            json!([{"constraint": "signature_match", "qualified-id": "Child.m"}])
        ),
        set(&["Child.m"]),
    );
}

// ---------------------------------------------------------------------------
// Predicate validation is hoisted out of the per-candidate evaluation loop.
// ---------------------------------------------------------------------------

#[test]
fn invalid_predicate_is_reported_exactly_once() {
    // `eval_predicate` runs once per candidate class, so reporting from inside it emitted one
    // copy of the same error per method in the program.
    let errors = load_errors_with(
        &java_program(),
        json!([{"constraint": "parent", "inner": {"constraint": "totally_bogus"}}]),
    );
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error, got: {errors:?}"
    );
}

#[test]
fn invalid_predicate_is_reported_on_non_java_frontends() {
    // `parent` matches nothing on a native VMT and used to return before evaluating, so a
    // broken predicate was reported zero times — the same model file loaded cleanly or failed
    // depending on which artifact it was run against.
    let errors = load_errors_with(
        &native_program(),
        json!([{"constraint": "parent", "inner": {"constraint": "totally_bogus"}}]),
    );
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error, got: {errors:?}"
    );
}

#[test]
fn nested_signature_match_rejects_keys_it_cannot_honor() {
    // The subject of a nested `signature_match` is already a class name, so `parent` is
    // meaningless there and was silently dropped.
    assert_unexpected_field(
        json!([{"constraint": "parent",
                "inner": {"constraint": "signature_match", "name": "Child", "parent": "X"}}]),
        "parent",
    );
}

#[test]
fn variable_port_on_propagation_is_rejected() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let malformed_json = json!({
        "find": "methods",
        "model": {
            "propagation": [
                {
                    "input": "Variable(x)",
                    "output": "Argument(0)"
                }
            ]
        }
    });

    let result = ingest.encode_models(vec![malformed_json]);
    match result {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            match &errors[0] {
                JsonModelError::UnexpectedField { message, .. } => {
                    assert!(
                        message.contains("Variable(...)"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected UnexpectedField error, got: {other:?}"),
            }
        }
        Ok(_) => panic!("expected error for Variable(...) on propagation"),
        Err(e) => panic!("expected JsonModel error, got: {e}"),
    }
}

#[test]
fn variable_port_with_find_callsites_is_rejected() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let malformed_json = json!({
        "find": "callsites",
        "model": {
            "sources": [
                {
                    "kind": "K",
                    "port": "Variable(x)"
                }
            ]
        }
    });

    let result = ingest.encode_models(vec![malformed_json]);
    match result {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            match &errors[0] {
                JsonModelError::UnexpectedField { message, .. } => {
                    assert!(
                        message.contains("find: callsites"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected UnexpectedField error, got: {other:?}"),
            }
        }
        Ok(_) => panic!("expected error for Variable(...) with find: callsites"),
        Err(e) => panic!("expected JsonModel error, got: {e}"),
    }
}

/// `find` is optional as far as the JSON goes, so a generator that omits it must be reported
/// as a missing field. It used to leave a hole in a positional table and panic the *next*
/// generator with "insertion index should be <= len".
#[test]
fn test_missing_find_field_error() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);

    let no_find = json!({
        "where": [{"constraint": "name", "pattern": "^f$"}],
        "model": {"sources": [{"port": "Argument(0)", "kind": "K"}]}
    });
    let with_find = json!({
        "find": "methods",
        "where": [{"constraint": "name", "pattern": "^g$"}],
        "model": {"sinks": [{"port": "Argument(0)", "kind": "K"}]}
    });

    match ingest.encode_models(vec![no_find, with_find]) {
        Err(Error::JsonModel(errors)) => {
            assert_eq!(errors.len(), 1);
            match &errors[0] {
                JsonModelError::MissingField { index, field_name } => {
                    assert_eq!(*index, 0);
                    assert_eq!(field_name, "find");
                }
                other => panic!("expected MissingField error, got: {other:?}"),
            }
        }
        Ok(_) => panic!("expected error for a generator with no 'find'"),
        Err(e) => panic!("expected JsonModel error, got: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Malformed access paths in ports are hard errors.
//
// A port's trailing access path used to be split on '.' with every segment made a
// `Symbol`, so nothing was ever malformed: `Argument(0).[*]` became `Symbol("[*]")`
// (a field no frontend emits, so the generator silently matched nothing),
// `Argument(0).a..b` became `Symbol("")`, and `Argument(0).[8]` became
// `Symbol("[8]")` rather than the offset it reads as. Ports now parse with the one
// canonical grammar and a violation names the port.
// ---------------------------------------------------------------------------

/// Loads a generator with `port` as a source and returns the collected errors.
/// Panics if the load succeeded — for these cases that is the bug under test.
fn access_path_errors_for(port: &str) -> Vec<String> {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({
        "find": "methods",
        "where": [{"constraint": "name", "pattern": "^f$"}],
        "model": {"sources": [{"port": port, "kind": "K"}]}
    });
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => errors.iter().map(|e| format!("{e:?}|{e}")).collect(),
        Ok(_) => panic!("expected a load error for port {port:?}"),
        Err(e) => panic!("expected JsonModel error for port {port:?}, got: {e}"),
    }
}

fn assert_invalid_access_path(port: &str) {
    let errors = access_path_errors_for(port);
    assert!(
        errors.iter().any(|e| e.starts_with("InvalidAccessPath")),
        "expected InvalidAccessPath for port {port:?}, got: {errors:?}"
    );
}

#[test]
fn wildcard_offset_port_is_hard_error() {
    assert_invalid_access_path("Argument(0).[*]");
}

#[test]
fn empty_access_path_segment_is_hard_error() {
    assert_invalid_access_path("Argument(0).a..b");
}

#[test]
fn trailing_dot_in_port_is_hard_error() {
    assert_invalid_access_path("Argument(0).a.");
}

/// A bracketed *field name* must be escaped. Unescaped it reads as an offset position
/// and fails, rather than quietly becoming `Symbol("[_elem_]")` — which is what the
/// lua and tree-sitter C frontends actually emit, so this is the mistake a user will
/// make. The message names the fix.
#[test]
fn unescaped_bracketed_field_name_is_hard_error() {
    assert_invalid_access_path("Argument(0).[_elem_]");
    let errors = access_path_errors_for("Argument(0).[_elem_]");
    assert!(
        errors.iter().any(|e| e.contains(r"\[_elem_]")),
        "message should name the escape: {errors:?}"
    );
}

/// The port regexes are anchored, so junk around a selector is rejected rather than
/// silently accepted. Unanchored, `Return(.*)?` matched "MyReturnType" and produced a
/// `Return` port with access path `Type`.
#[test]
fn unanchored_port_text_is_hard_error() {
    let errors = access_path_errors_for("MyReturnType");
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("InvalidArgumentFormat")),
        "expected InvalidArgumentFormat, got: {errors:?}"
    );
}

/// Errors accumulate rather than the first one hiding the rest.
#[test]
fn multiple_bad_ports_all_reported() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut model_builders);
    let model = json!({
        "find": "methods",
        "where": [{"constraint": "name", "pattern": "^f$"}],
        "model": {"sources": [
            {"port": "Argument(0).[*]", "kind": "K"},
            {"port": "Argument(1).a..b", "kind": "K"},
        ]}
    });
    match ingest.encode_models(vec![model]) {
        Err(Error::JsonModel(errors)) => {
            let n = errors
                .iter()
                .filter(|e| matches!(e, JsonModelError::InvalidAccessPath { .. }))
                .count();
            assert_eq!(n, 2, "both bad ports should be reported, got: {errors:?}");
        }
        other => panic!("expected two InvalidAccessPath errors, got: {other:?}"),
    }
}
