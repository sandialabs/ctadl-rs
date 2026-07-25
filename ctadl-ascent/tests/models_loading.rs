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

/// Loading is batched in chunks of 1024 (`try_load_models_from_values`). Every generator must
/// survive that batching, and every generator's index must keep counting across batch
/// boundaries: the index names the generator in JSON error messages and in the `CTADL0004`
/// SARIF notification, and it keys `endpoint_stats`, so a per-batch index both misnames
/// generators and collides their match counts.
#[test]
fn test_load_models_across_batch_boundary() {
    const COUNT: usize = 1030;
    let program_info = ProgramInfo::default();
    let mut file = NamedTempFile::new().unwrap();
    let generators: Vec<serde_json::Value> = (0..COUNT)
        .map(|i| {
            serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "name", "pattern": format!("^f{i}$")}],
                "model": {"sources": [{"port": "Argument(0)", "kind": "K"}]}
            })
        })
        .collect();
    write!(
        file,
        "{}",
        serde_json::json!({ "model_generators": generators })
    )
    .unwrap();

    let batch = try_load_models(&program_info, file.path()).expect("loading models");
    let indices: Vec<usize> = batch.endpoint_stats.keys().map(|(i, _)| *i).collect();
    assert_eq!(
        indices.len(),
        COUNT,
        "every generator should be accounted for, including #1025"
    );
    assert_eq!(indices.first().copied(), Some(0));
    assert_eq!(indices.last().copied(), Some(COUNT - 1));
}
