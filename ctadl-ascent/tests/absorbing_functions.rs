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
        vertex: ctadl_ascent::facts::FlowVertex(
            FlowVariable::formal_index(0i16.into()),
            Path::empty(),
        ),
        label: Label("Net".into()),
        direction: TaintDirection::Forward,
    };

    // Taint the formal 0 of main
    facts.taint.push((
        main_id,
        TaintState::Free,
        FlowVariable::formal_index(0i16.into()),
        Path::empty(),
        endpoint.clone(),
    ));

    // actual_param: main calls ExternalFunc(main.formal(0))
    // we need to show that the call site has a tainted argument.
    // In compute_taint_results, it uses:
    // taint(_, _, v, _, src),
    // if let FlowVariable::CallArg { id, formal: f } = v,
    // call(id, target),
    // external_function(target);

    let call_arg = FlowVariable::call_arg_packed(
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

    let result = compute_taint_results(&facts);

    assert!(
        result
            .absorbing_functions
            .iter()
            .any(|(f, _, _)| *f == ext_id),
        "Taint should have reached the external function endpoint"
    );
}
