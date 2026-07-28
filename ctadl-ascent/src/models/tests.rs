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
        None,
        &[PathSegment::symbol("field1"), PathSegment::symbol("sub")],
        "lbl1",
        TaintDirection::Forward,
        false,
        false,
        None,
        false,
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        None,
        &[],
        "lbl2",
        TaintDirection::Backward,
        true,
        false,
        None,
        false,
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
        "saturating",
        "in_function",
        "callsite_scoped",
        "local_index",
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
        None,
        &[PathSegment::symbol("fieldA")],
        "lbl1",
        TaintDirection::Forward,
        false,
        false,
        None,
        false,
    );
    // Second endpoint with an empty access path and no index
    builder.append(
        "func2",
        (FormalIndexTypeTag::Global, None),
        None,
        &[],
        "lbl2",
        TaintDirection::Backward,
        true,
        false,
        Some("caller_fn"),
        true,
    );
    let batch = builder.finish().expect("finish failed");
    let endpoints: Vec<_> = batch.iter_endpoints().collect();
    assert_eq!(endpoints.len(), 2);
    assert_eq!(
        endpoints[0],
        EndpointRow {
            function: "func1",
            selector_ty: FormalIndexTypeTag::Return,
            index: Some(RETURN_INDEX),
            path_id: 0u64,
            label: "lbl1",
            direction: TaintDirection::Forward,
            wildcard: false,
            saturating: false,
            in_function: None,
            callsite_scoped: false,
            local_index: None,
        },
    );
    assert_eq!(
        endpoints[1],
        EndpointRow {
            function: "func2",
            selector_ty: FormalIndexTypeTag::Global,
            index: None,
            path_id: 1u64,
            label: "lbl2",
            direction: TaintDirection::Backward,
            wildcard: true,
            saturating: false,
            in_function: Some("caller_fn"),
            callsite_scoped: true,
            local_index: None,
        },
    );
}

#[test]
fn endpoint_builder_local_selector_roundtrip() {
    let mut builder = EndpointBuilder::new();
    // A `Variable(name)`-style port carries its base LocalIdx out-of-band in `local_index`.
    builder.append(
        "func1",
        (FormalIndexTypeTag::Local, None),
        Some(7),
        &[PathSegment::symbol("headers")],
        "lbl1",
        TaintDirection::Forward,
        false,
        false,
        None,
        false,
    );
    let batch = builder.finish().expect("finish failed");
    let endpoints: Vec<_> = batch.iter_endpoints().collect();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].selector_ty, FormalIndexTypeTag::Local);
    assert_eq!(endpoints[0].index, None);
    assert_eq!(endpoints[0].local_index, Some(7));
}

// Tests for UniverseSet set difference (backs the `not` combinator).
mod universe_set_diff {
    use crate::models::universe_set::UniverseSet;
    use std::collections::BTreeSet;

    fn explicit<'a>(items: &[&'a str]) -> UniverseSet<&'a str> {
        items.iter().copied().collect()
    }

    fn as_set<'a>(u: &UniverseSet<&'a str>) -> BTreeSet<&'a str> {
        match u {
            UniverseSet::Explicit(s) => s.clone(),
            UniverseSet::All => panic!("expected Explicit, got All"),
        }
    }

    #[test]
    fn difference_removes_members() {
        // {a,b,c} \ {b} == {a,c}
        let mut a = explicit(&["a", "b", "c"]);
        a.difference_with(explicit(&["b"]));
        assert_eq!(as_set(&a), BTreeSet::from(["a", "c"]));
    }

    #[test]
    fn difference_with_all_is_empty() {
        // {a} \ All == {}
        let mut a = explicit(&["a"]);
        a.difference_with(UniverseSet::all());
        assert!(as_set(&a).is_empty());
    }
}
