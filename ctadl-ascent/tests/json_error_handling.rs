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
