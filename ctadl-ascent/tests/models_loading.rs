use ctadl_ascent::models::try_load_models;
use ctadl_ir::mir::ProgramInfo;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_load_models_json() {
    let program_info = ProgramInfo::default();
    let mut file = NamedTempFile::new().unwrap();
    let json_content = r#"{
        "model_generators": [
            {
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "test"}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
            }
        ]
    }"#;
    writeln!(file, "{}", json_content).unwrap();

    let result = try_load_models(&program_info, file.path());
    assert!(
        result.is_ok(),
        "Failed to load JSON models: {:?}",
        result.err()
    );
}

#[test]
fn test_load_models_jsonl() {
    let program_info = ProgramInfo::default();
    // Use .jsonl extension
    let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
    let jsonl_content = r#"{"find": "methods", "where": [{"constraint": "signature_match", "name": "test"}], "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}}"#;
    writeln!(file, "{}", jsonl_content).unwrap();

    let result = try_load_models(&program_info, file.path());
    assert!(
        result.is_ok(),
        "Failed to load JSONL models: {:?}",
        result.err()
    );
}

#[test]
fn test_load_models_json5() {
    let program_info = ProgramInfo::default();
    // Use .json5 extension
    let mut file = NamedTempFile::with_suffix(".json5").unwrap();
    let json5_content = r#"{
        // This is a comment, allowed in JSON5
        model_generators: [ // Unquoted keys allowed in JSON5
            {
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "test"}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
            },
        ] // Trailing commas allowed in JSON5
    }"#;
    writeln!(file, "{}", json5_content).unwrap();

    let result = try_load_models(&program_info, file.path());
    assert!(
        result.is_ok(),
        "Failed to load JSON5 models: {:?}",
        result.err()
    );
}
