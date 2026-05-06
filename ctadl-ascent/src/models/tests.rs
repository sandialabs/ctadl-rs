// Tests for EndpointBuilder
use super::*;
use crate::codegen::RETURN_INDEX;

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
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        &[],
        "lbl2",
        TaintDirection::Backward,
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
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        &[],
        "lbl2",
        TaintDirection::Backward,
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
        ),
    );
}
