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
