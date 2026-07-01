use ctadl_ascent::facts::{
    FlowVariable, FlowVertex, FunctionId, InsnId, InsnSiteId, Label, PackedInsnSiteId, Path,
    TaintDirection,
};
use ctadl_ascent::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};

#[test]
fn test_absorbing_functions() {
    let main_id = FunctionId::new(0);
    let ext_id = FunctionId::new(1);

    let site_id = InsnSiteId::new(main_id, InsnId::new(0));
    let packed_site = PackedInsnSiteId::try_from(site_id).unwrap();

    // main calls ExternalFunc(arg), and `arg` (formal 0 of the call) is tainted.
    // Seeding the source directly on the call-arg vertex means the single taint
    // pass taints it, and the `absorbing_functions` rule (now part of
    // `taint_analysis`) should report that the external function absorbs it.
    let call_arg = FlowVariable::call_arg_packed(
        ctadl_ascent::facts::PackedCallArg::try_from_parts(site_id.insn_id, 0i16.into()).unwrap(),
    );

    let endpoint = QueryEndpoint {
        infunc: main_id,
        vertex: FlowVertex(call_arg, Path::empty()),
        label: Label("Net".into()),
        direction: TaintDirection::Forward,
        call_site: None,
    };

    let facts = QueryFacts {
        // Call site info: the call at `packed_site` targets the external function.
        call: vec![(packed_site, ext_id)],
        // External function info.
        external_function: vec![(ext_id,)],
        endpoints: vec![(endpoint,)],
        ..Default::default()
    };

    let result = taint_analysis(facts, None);

    assert!(
        result
            .absorbing_functions
            .iter()
            .any(|(f, _, _)| *f == ext_id),
        "Taint should have reached the external function endpoint"
    );
}
