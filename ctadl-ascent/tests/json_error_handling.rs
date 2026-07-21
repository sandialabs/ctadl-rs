use ctadl_ascent::error::{Error, JsonModelError};
use ctadl_ascent::models::ModelBuilders;
use ctadl_ascent::models::json::ModelGeneratorIngest;
use ctadl_ir::mir::ProgramInfo;
use serde_json::json;

#[test]
fn test_missing_field_error() {
    let program_info = ProgramInfo::default();
    let mut model_builders = ModelBuilders::new();
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);

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
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);

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
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);

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
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);

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
        NativeFunction, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };

    let program_info = ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: vec![
                (
                    NativeSimpleName("get".into()),
                    NativeSignature("get".into()),
                    NativeFunction("get".into()),
                ),
                (
                    NativeSimpleName("read_http_data".into()),
                    NativeSignature("read_http_data".into()),
                    NativeFunction("read_http_data".into()),
                ),
                (
                    NativeSimpleName("unrelated".into()),
                    NativeSignature("unrelated".into()),
                    NativeFunction("unrelated".into()),
                ),
            ],
        },
        ..Default::default()
    };
    let mut model_builders = ModelBuilders::new();
    {
        let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);
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
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);

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
    JavaClass, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction, NativeSignature,
    NativeSimpleName, VirtualMethodTable,
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
        },
        ..Default::default()
    }
}

/// Runs a `find: methods` generator whose `where` is `where_c` and a source model, returning the
/// set of functions endpoints were emitted for (== the functions the `where` matched).
fn matched_functions(program_info: &ProgramInfo, where_c: serde_json::Value) -> BTreeSet<String> {
    let mut model_builders = ModelBuilders::new();
    {
        let mut ingest = ModelGeneratorIngest::new(program_info, &mut model_builders);
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
    let mut ingest = ModelGeneratorIngest::new(&program_info, &mut model_builders);
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
