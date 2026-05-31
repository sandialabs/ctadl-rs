use ctadl_ascent::facts::{
    FlowVariable, FunctionId, InsnId, InsnSiteId, Label, PackedInsnSiteId, Path, TaintDirection,
    TaintState,
};
use ctadl_ascent::query_engine::QueryEndpoint;
use ctadl_ascent::query_engine::formatter::{FormatFacts, compute_taint_results};

#[test]
fn test_absorbing_functions() {
    let mut facts = FormatFacts::default();

    let main_id = FunctionId::new(0);
    let ext_id = FunctionId::new(1);

    let site_id = InsnSiteId::new(main_id, InsnId::new(0));
    let packed_site = PackedInsnSiteId::try_from(site_id).unwrap();

    let endpoint = QueryEndpoint {
        infunc: main_id,
        vertex: ctadl_ascent::facts::FlowVertex(FlowVariable::Formal(0i16.into()), Path::empty()),
        label: Label("test".into()),
        direction: TaintDirection::Forward,
    };

    let endpoint2 = QueryEndpoint {
        infunc: main_id,
        vertex: ctadl_ascent::facts::FlowVertex(FlowVariable::Formal(0i16.into()), Path::empty()),
        label: Label("test2".into()),
        direction: TaintDirection::Forward,
    };

    // Taint the formal 0 of main
    facts.taint.push((
        main_id,
        TaintState::Free,
        FlowVariable::Formal(0i16.into()),
        Path::empty(),
        endpoint.clone(),
    ));
    facts.taint.push((
        main_id,
        TaintState::Free,
        FlowVariable::Formal(0i16.into()),
        Path::empty(),
        endpoint2.clone(),
    ));

    // actual_param: main calls ExternalFunc(main.formal(0))
    // we need to show that the call site has a tainted argument.
    // In compute_taint_results, it uses:
    // taint(_, _, v, _, src),
    // if let FlowVariable::CallArg { id, formal: f } = v,
    // call(id, target),
    // external_function(target);

    let call_arg = FlowVariable::CallArg(
        ctadl_ascent::facts::PackedCallArg::try_from_parts(site_id.insn_id, 0i16.into()).unwrap(),
    );

    // Propagate taint from formal to CallArg
    facts.taint.push((
        main_id,
        TaintState::Free,
        call_arg.clone(),
        Path::empty(),
        endpoint.clone(),
    ));

    // Call site info
    facts.call.push((packed_site, ext_id));

    // External function info
    facts.external_function.push((ext_id,));

    let results = compute_taint_results(&facts);

    assert_eq!(results.absorbing_functions.len(), 1);
    let (fid, qe, formal) = &results.absorbing_functions[0];
    assert_eq!(*fid, ext_id);
    assert_eq!(&*qe.label.0, "test");
    assert_eq!(**formal, 0);
}
