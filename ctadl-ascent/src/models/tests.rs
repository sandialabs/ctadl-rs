// Tests for EndpointBuilder
use super::*;
use crate::codegen::RETURN_INDEX;
use std::path::Path;

#[test]
fn endpoint_builder_basic() {
    let mut builder = EndpointBuilder::new();
    // First endpoint with a non‑empty access path
    builder.append(
        "func1",
        (FormalIndexTypeTag::Return, Some(RETURN_INDEX)),
        &["field1", "sub"],
        "lbl1",
        TaintDirection::Forward,
        false,
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        &[],
        "lbl2",
        TaintDirection::Backward,
        true,
    );
    assert_eq!(builder.len(), 2);
    let batch = builder.finish().expect("finish failed");
    // Verify schema fields order
    let expected = [
        "function",
        "selector_ty",
        "index",
        "path_id",
        "label",
        "direction",
        "wildcard",
    ];
    let actual: Vec<_> = batch
        .endpoints
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    assert_eq!(actual, expected);
    // Records should have two rows
    assert_eq!(batch.endpoints.num_rows(), 2);
    // Access‑path tables should have one entry per distinct path (two entries here)
    assert_eq!(batch.aps.ap_len.num_rows(), 2);
}

#[test]
fn endpoint_batch_iter_endpoints() {
    let mut builder = EndpointBuilder::new();
    // First endpoint with a non‑empty access path
    builder.append(
        "func1",
        (FormalIndexTypeTag::Return, Some(RETURN_INDEX)),
        &["fieldA"],
        "lbl1",
        TaintDirection::Forward,
        false,
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        &[],
        "lbl2",
        TaintDirection::Backward,
        true,
    );
    let batch = builder.finish().expect("finish failed");
    let endpoints: Vec<_> = batch.iter_endpoints().collect();
    assert_eq!(endpoints.len(), 2);
    assert_eq!(
        endpoints[0],
        (
            "func1",
            FormalIndexTypeTag::Return,
            Some(RETURN_INDEX),
            0u64,
            "lbl1",
            TaintDirection::Forward,
            false,
        ),
    );
    assert_eq!(
        endpoints[1],
        (
            "func2",
            FormalIndexTypeTag::Global,
            None,
            1u64,
            "lbl2",
            TaintDirection::Backward,
            true,
        ),
    );
}

#[test]
fn php_dvwa_models_include_sink_endpoints() {
    let source = r#"
        <?php
        if (isset($_POST['Submit'])) {
            $ip = $_REQUEST['ip'];
            $cmd = shell_exec('ping ' . $ip);
            $query = "SELECT * FROM users WHERE id = '" . $_REQUEST['id'] . "'";
            mysqli_query($db, $query);
            echo $cmd;
        }
    "#;

    let program_info = php_reader::lower_php(source, "mini.php").expect("PHP lowering failed");
    let model_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../php-reader/models/dvwa_taint_models.json");
    let batch = try_load_models(&program_info, model_path).expect("model load failed");

    let endpoints: Vec<_> = batch.endpoint.iter_endpoints().collect();
    assert!(
        endpoints
            .iter()
            .any(|(func, _, index, _, _, direction, _)| {
                *func == "shell_exec" && *index == Some(0) && *direction == TaintDirection::Backward
            })
    );
    assert!(
        endpoints
            .iter()
            .any(|(func, _, index, _, _, direction, _)| {
                *func == "mysqli_query"
                    && *index == Some(1)
                    && *direction == TaintDirection::Backward
            })
    );
    assert!(
        endpoints
            .iter()
            .any(|(func, selector, _, _, _, direction, _)| {
                *func == "__php_main__::mini.php"
                    && *selector == FormalIndexTypeTag::Global
                    && *direction == TaintDirection::Forward
            })
    );
}
