use super::*;

use hashbrown::HashSet;

use super::GLOBALS_INDEX;
use crate::facts as fx;
use crate::facts::{FlowVariable, FlowVertex, TaintEndpoint};
use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{IndexFacts, taint_index};
use crate::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::builder::FunctionBuilder;
use ctadl_ir::ssa;

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct SourceSinkQuery {
    pub source: TaintEndpoint,
    pub sink: TaintEndpoint,
}

#[test]
fn test_basic2_f() {
    let f = function_f();
    let mut f_ssa = f.clone();
    log::trace!("f before transform: {f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("f after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("result: {:#?}", result);
    assert!(!result.summary.is_empty());
}

#[test]
fn test_basic2_j() {
    let f = function_j();
    let mut f_ssa = f.clone();
    log::trace!("{f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("{f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    assert!(!result.summary.is_empty());
}

// A test with a call
#[test]
fn test_basic3() {
    let mut program = Program::default();
    program.functions.push(function_f());
    program.functions.push(function_g());
    let program_info = ProgramInfo {
        program,
        source_info: Default::default(),
        vmt: Default::default(),
    };
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        &Default::default(),
    );
    let f_id = source_info
        .sites
        .get_function_id(fx::Function("F".into()))
        .unwrap();
    let g_id = source_info
        .sites
        .get_function_id(fx::Function("G".into()))
        .unwrap();
    assert!(
        facts
            .call
            .iter()
            .find(|(_, callee)| *callee == f_id)
            .is_some()
    );
    let result = taint_index(facts);
    assert!(result.summary.iter().find(|t| t.0 == f_id).is_some());
    assert!(result.summary.iter().find(|t| t.0 == g_id).is_some());
    assert!(result.summary.len() >= 3);
}

/// `modes: ["skip-analysis"]`, at the layer that implements it. `test_basic3` above is the
/// control: same two functions, G calling F, nothing skipped.
///
/// What a skipped function keeps is its *signature*, because that is what the model's own
/// `summary` rows are written against and what `compute_num_params` reports. What it loses is
/// everything its blocks would have produced -- including, and this is the part no fact-level
/// guard could do, the `call` edge out of it.
#[test]
fn a_skipped_body_contributes_no_facts() {
    let mut program = Program::default();
    program.functions.push(function_f());
    program.functions.push(function_g());
    let program_info = ProgramInfo {
        program,
        source_info: Default::default(),
        vmt: Default::default(),
    };
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    let skipped = codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        &[Str::from("G")].into_iter().collect(),
    );
    assert_eq!(skipped, 1, "exactly one body was named and lowered");

    let f_id = source_info
        .sites
        .get_function_id(fx::Function("F".into()))
        .unwrap();
    let g_id = source_info
        .sites
        .get_function_id(fx::Function("G".into()))
        .unwrap();

    // The signature survives: one declared parameter plus the globals and return auxiliaries.
    assert_eq!(
        facts
            .formal_param
            .iter()
            .filter(|(f, ..)| *f == g_id)
            .count(),
        3
    );
    // The body does not. G's call to F was the program's only call site.
    assert!(
        facts.call.is_empty(),
        "the skipped body's call edge must not be in the fact base"
    );
    for (site, ..) in &facts.assign {
        let fx::InsnSiteId { func_id, .. } = fx::InsnSiteId::try_from(*site).unwrap();
        assert_ne!(func_id, g_id, "the skipped body produced an assign row");
    }

    // F is untouched, and G derives nothing -- with no model loaded here, it has no summary at
    // all, which is the "this function moves nothing" case.
    let result = taint_index(facts);
    assert!(result.summary.iter().any(|t| t.0 == f_id));
    assert!(!result.summary.iter().any(|t| t.0 == g_id));
}

#[test]
fn test_basic2_source_sink() {
    let (h, ss) = function_h();
    let mut h_ssa = h.clone();
    log::trace!("{h}");
    ssa::transform(&mut h_ssa, false);
    log::trace!("{h_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&h_ssa, &mut facts, &mut source_info);
    let index_result = taint_index(facts.clone());
    let h_id = source_info
        .sites
        .get_function_id(fx::Function("H".into()))
        .unwrap();
    let qfacts = QueryFacts {
        formal_param: facts.formal_param,
        actual_param: facts.actual_param,
        call: facts.call,
        assign: index_result.assign_like,
        paths: facts.paths,
        external_function: index_result.external_function,
        endpoints: [ss.source.clone(), ss.sink.clone()]
            .into_iter()
            .map(|e| (QueryEndpoint::from_taint_endpoint(&source_info.sites, e),))
            .collect(),
    };
    let query_result = taint_analysis(qfacts, None);
    assert!(
        query_result
            .taint
            .iter()
            .find(|r| r.0 == h_id
                && r.4.clone().to_taint_endpoint(&source_info.sites) == ss.source
                && r.2 == ss.sink.vertex.0
                && r.3 == ss.sink.vertex.1)
            .is_some()
    );
    assert!(
        query_result
            .taint
            .iter()
            .find(|r| r.0 == h_id
                && r.4.clone().to_taint_endpoint(&source_info.sites) == ss.sink
                && r.2 == ss.source.vertex.0
                && r.3 == ss.source.vertex.1)
            .is_some()
    );

    // The taint graph is oriented in execution / data-flow order, so a forward walk over
    // `taint_edge` from the source vertex must reach the sink vertex. This only holds if
    // backward (sink-seeded) edges were reversed into execution order.
    assert!(!query_result.taint_edge.is_empty());
    let mut adj: std::collections::BTreeMap<
        (fx::FunctionId, fx::FlowVariable, fx::Path),
        Vec<(fx::FunctionId, fx::FlowVariable, fx::Path)>,
    > = std::collections::BTreeMap::new();
    for (_edge, sf, sv, sp, df, dv, dp) in &query_result.taint_edge {
        adj.entry((*sf, *sv, *sp))
            .or_default()
            .push((*df, *dv, *dp));
    }
    let start = (h_id, ss.source.vertex.0, ss.source.vertex.1);
    let goal = (h_id, ss.sink.vertex.0, ss.sink.vertex.1);
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([start]);
    seen.insert(start);
    let mut reached_sink = false;
    while let Some(node) = queue.pop_front() {
        if node == goal {
            reached_sink = true;
            break;
        }
        for next in adj.get(&node).into_iter().flatten() {
            if seen.insert(*next) {
                queue.push_back(*next);
            }
        }
    }
    assert!(
        reached_sink,
        "forward walk over taint_edge should reach the sink vertex"
    );
}

// Test Phi instruction with control flow
#[test]
fn test_phi_instruction() {
    let f = function_with_phi();
    let mut f_ssa = f.clone();
    log::trace!("Phi function before transform: {f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("Phi function after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("Phi result: {:#?}", result);
    assert!(!result.summary.is_empty());
}

// Test Update instruction with field access
#[test]
fn test_update_instruction() {
    let f = function_with_update();
    let mut f_ssa = f.clone();
    log::trace!("Update function before transform: {f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("Update function after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("Update result: {:#?}", result);
    assert!(!result.summary.is_empty());
}

// Exercises `Update` codegen end-to-end: `q = update(p0, .field := p1); return q`. Unlike a
// `Store`, an `Update` copies the whole `source` aggregate into `dest` in addition to writing the
// field, so the summary must contain BOTH the field write (p1 -> ret.field) and the
// whole-aggregate copy (p0 -> ret), the latter being unique to `Update`.
#[test]
fn test_real_update_instruction() {
    let f = function_with_real_update();
    let mut f_ssa = f.clone();
    log::trace!("Real update before transform: {f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("Real update after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("Real update summary: {:#?}", result.summary);

    let f_id = source_info
        .sites
        .get_function_id(fx::Function("real_update".into()))
        .unwrap();

    // The field write p1 -> q.field, returned in q: (ret, .field) <- (formal 1, empty).
    let has_field_flow = result.summary.iter().any(|(fid, dst_i, dst_p, src_i, src_p)| {
        *fid == f_id && **dst_i == -1 && !dst_p.is_empty() && **src_i == 1 && src_p.is_empty()
    });
    assert!(
        has_field_flow,
        "expected p1 to flow to the returned aggregate's field"
    );

    // The whole-aggregate copy p0 -> q, returned in q: (ret, empty) <- (formal 0, empty). This flow
    // exists only because `Update` copies the entire source aggregate; a `Store` would not.
    let has_whole_copy = result.summary.iter().any(|(fid, dst_i, dst_p, src_i, src_p)| {
        *fid == f_id && **dst_i == -1 && dst_p.is_empty() && **src_i == 0 && src_p.is_empty()
    });
    assert!(
        has_whole_copy,
        "expected the whole source aggregate p0 to flow to the returned aggregate (Update-specific)"
    );
}

// Test that local variables flow into fields of globals, not globals index itself
#[test]
fn test_local_to_global_field() {
    let f = function_with_param_to_global_field();
    let mut f_ssa = f.clone();
    log::trace!("Local to global field function before transform: {f}");
    ssa::transform(&mut f_ssa, false);
    log::trace!("Local to global field function after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("Local to global field result: {:#?}", result);

    // Check that local variable flows to global field, not globals index
    let f_id = source_info
        .sites
        .get_function_id(fx::Function("param_to_global_field".into()))
        .unwrap();

    // The correct behavior is that param flows to global field
    let has_bad_flow =
        result
            .summary
            .iter()
            .any(|(func_id, dst_index, dst_path, src_index, src_path)| {
                *func_id == f_id
                    && **src_index == 0
                    && src_path.is_empty()
                    && **dst_index == GLOBALS_INDEX
                    && dst_path.is_empty()
            });

    assert!(
        !has_bad_flow,
        "Local variable should flow to a field of globals, not the globals index itself"
    );
}

// def F(p, q)
// {
//   a = q;
//   p = a;
//   return p;
// }
// The intended flow is from q -> a -> p -> return
fn function_f() -> FunctionData {
    let mut f = FunctionData {
        name: "F".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };

    let mut fb = FunctionBuilder::new(&mut f);
    fb.add_param(ParameterType::ByVal);
    fb.add_param(ParameterType::ByVal);

    let entry = fb.add_block();
    fb.at_block(entry).create_goto(vec![BasicBlockIdx::new(1)]);

    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let a = b.new_local_var("a");
    let p = b.new_param_var(ParameterIdx::new(0));
    let q = b.new_param_var(ParameterIdx::new(1));

    b.create_assign_or_store(a.clone(), None, q);
    b.create_assign_or_store(p.clone(), None, a);
    b.create_ret(vec![p.into()]);

    f.verify().expect("Function doesn't verify");
    f
}

// def J(p, q)
// {
//   a = q + b;
//   p = a;
//   return p;
// }
fn function_j() -> FunctionData {
    let mut f = FunctionData {
        name: "F".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };

    let mut fb = FunctionBuilder::new(&mut f);
    fb.add_param(ParameterType::ByVal);
    fb.add_param(ParameterType::ByVal);

    let entry = fb.add_block();
    fb.at_block(entry).create_goto(vec![BasicBlockIdx::new(1)]);

    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let a = b.new_local_var("a");
    let param_b = b.new_local_var("b");
    let p = b.new_param_var(ParameterIdx::new(0));
    let q = b.new_param_var(ParameterIdx::new(1));

    b.create_assign(a.clone(), vec![q.into(), param_b.into()]);
    b.create_assign_or_store(p.clone(), None, a);
    b.create_ret(vec![p.into()]);

    f.verify().expect("Function doesn't verify");
    f
}

//def G(b) {
//  c = F(a, b);
//  return c;
//}
fn function_g() -> FunctionData {
    let mut f = FunctionData {
        name: "G".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };

    let mut fb = FunctionBuilder::new(&mut f);
    fb.add_param(ParameterType::ByVal);

    let body = fb.add_block();
    let mut b = fb.at_block(body);

    let a = b.new_local_var("a");
    let param_b = b.new_param_var(ParameterIdx::new(0));
    let c = b.new_local_var("c");

    let call_edges = CallEdges::Explicit(ctadl_ir::thin_vec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };

    b.create_call(style, vec![c.clone()], vec![a.into(), param_b.into()]);
    b.create_ret(vec![c.into()]);

    f.verify().expect("Function doesn't verify");
    f
}

// def H(p, q)
// {
//   q = source(Net);
//   a = q;
//   p = a;
//   sink(p, Net);
//   return p;
// }
fn function_h() -> (FunctionData, SourceSinkQuery) {
    let mut f = FunctionData {
        name: "H".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };

    let mut fb = FunctionBuilder::new(&mut f);
    fb.add_param(ParameterType::ByVal);
    fb.add_param(ParameterType::ByVal);

    let entry = fb.add_block();
    fb.at_block(entry).create_goto(vec![BasicBlockIdx::new(1)]);

    let body = fb.add_block();
    let mut b = fb.at_block(body);

    let a = b.new_local_var("a");
    let p = b.new_param_var(ParameterIdx::new(0));
    let q = b.new_param_var(ParameterIdx::new(1));

    b.create_assign_or_store(a.clone(), None, q);
    b.create_assign_or_store(p.clone(), None, a);
    b.create_ret(vec![p.into()]);

    f.verify().expect("Function doesn't verify");

    let ss = SourceSinkQuery {
        source: TaintEndpoint {
            infunc: fx::Function(f.name.clone().into()),
            vertex: FlowVertex(FlowVariable::formal_index(1i8.into()), fx::Path::empty()),
            label: fx::Label("Net".into()),
            direction: fx::TaintDirection::Forward,
        },
        sink: TaintEndpoint {
            infunc: fx::Function(f.name.clone().into()),
            vertex: FlowVertex(FlowVariable::formal_index(0i8.into()), fx::Path::empty()),
            label: fx::Label("Net".into()),
            direction: fx::TaintDirection::Backward,
        },
    };

    (f, ss)
}

// def phi_func(cond, a, b)
// {
//   if (cond) {
//     x = a;
//   } else {
//     x = b;
//   }
//   return x;
// }
fn function_with_phi() -> FunctionData {
    use ctadl_ir::mir::builder::BasicBlockBuilder;

    let mut f = FunctionData {
        name: "phi_func".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    f.params.push(ParameterType::ByVal); // cond
    f.params.push(ParameterType::ByVal); // a
    f.params.push(ParameterType::ByVal); // b

    let blocks = f.blocks.blocks_mut();

    // Entry block with conditional branch
    let _entry = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1), BasicBlockIdx::new(2)].into(),
        },
    ))));

    // True branch
    let true_branch = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(3)].into(),
        },
    ))));

    // False branch
    let false_branch = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(3)].into(),
        },
    ))));

    // Merge block
    let merge = blocks.push(BasicBlockData::new(None));

    let _cond = VariableRef::new_parameter(ParameterIdx::new(0));
    let a = VariableRef::new_parameter(ParameterIdx::new(1));
    let b = VariableRef::new_parameter(ParameterIdx::new(2));
    let x = VariableRef::new_local_idx(f.locals.get_or_intern("x"));

    // True branch: x = a (using builder API)
    let mut true_builder = BasicBlockBuilder::new(&mut f.blocks[true_branch], &mut f.locals);
    true_builder.create_assign_or_store(x.clone(), None, Exp::Variable(a));

    // False branch: x = b (using builder API)
    let mut false_builder = BasicBlockBuilder::new(&mut f.blocks[false_branch], &mut f.locals);
    false_builder.create_assign_or_store(x.clone(), None, Exp::Variable(b));

    // Merge block will get phi node during SSA conversion (using builder API)
    let mut merge_builder = BasicBlockBuilder::new(&mut f.blocks[merge], &mut f.locals);
    merge_builder.create_ret(vec![Exp::Variable(x)]);

    f.verify().expect("doesn't verify");
    f
}

// def update_func(s)
// {
//   s.field = new_value;
//   return s;
// }
fn function_with_update() -> FunctionData {
    use ctadl_ir::mir::builder::BasicBlockBuilder;

    let mut f = FunctionData {
        name: "update_func".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    f.params.push(ParameterType::ByVal);

    let blocks = f.blocks.blocks_mut();

    // Entry block with goto to body
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));

    // Body block
    let body = blocks.push(BasicBlockData::new(None));
    let mut builder = BasicBlockBuilder::new(&mut f.blocks[body], &mut f.locals);

    // Create variables using builder helpers
    let s_var = builder.new_param_var(ParameterIdx::new(0));
    let new_value = builder.new_local_var("new_value");

    // Create update statement using builder API: s.field = new_value
    builder.create_store(
        s_var.clone(),
        ctadl_ir::mir::FieldPath::symbol("field"),
        Exp::Variable(new_value.clone()),
    );

    // Create return statement using builder API
    builder.create_ret(vec![Exp::Variable(s_var)]);

    f.verify().expect("doesn't verify");
    f
}

// def real_update(p0, p1) {
//   q = update(p0, .field := p1);   // q is p0 with q.field set to p1
//   return q;
// }
//
// Exercises the restored `Update` instruction (a functional update: `dest` is a fresh copy of
// `source` with one field overwritten), as opposed to `function_with_update`'s in-place `Store`.
fn function_with_real_update() -> FunctionData {
    use ctadl_ir::mir::builder::BasicBlockBuilder;

    let mut f = FunctionData {
        name: "real_update".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    f.params.push(ParameterType::ByVal);
    f.params.push(ParameterType::ByVal);

    let blocks = f.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));

    let body = blocks.push(BasicBlockData::new(None));
    let mut builder = BasicBlockBuilder::new(&mut f.blocks[body], &mut f.locals);

    let p0 = builder.new_param_var(ParameterIdx::new(0));
    let p1 = builder.new_param_var(ParameterIdx::new(1));
    let q = builder.new_local_var("q");

    // q = update(p0, .field := p1)
    builder.create_update(
        q.clone(),
        p0,
        ctadl_ir::mir::FieldPath::symbol("field"),
        Exp::Variable(p1),
    );
    builder.create_ret(vec![Exp::Variable(q)]);

    f.verify().expect("doesn't verify");
    f
}

// def param_to_global_field(p0) {
//   globals.field = p0;
//   return;
//}
fn function_with_param_to_global_field() -> FunctionData {
    use ctadl_ir::mir::builder::BasicBlockBuilder;

    let mut f = FunctionData {
        name: "param_to_global_field".to_string(),
        return_type: ReturnType { arity: 0 },
        ..Default::default()
    };

    f.params.push(ParameterType::ByVal);

    let blocks = f.blocks.blocks_mut();

    // Body block
    let body = blocks.push(BasicBlockData::new(None));
    let mut builder = BasicBlockBuilder::new(&mut f.blocks[body], &mut f.locals);

    // Create local variable and assign it a value
    let local_var = builder.new_param_var(ParameterIdx::new(0));

    // Create globals access and update its field with local_var
    let globals_var = builder.new_global_var();

    // This is the key assignment: globals.field = local_var
    builder.create_store(
        globals_var.clone(),
        ctadl_ir::mir::FieldPath::symbol("field"),
        Exp::Variable(local_var.clone()),
    );

    // Return globals
    builder.create_ret(vec![]);

    f.verify().expect("doesn't verify");
    f
}

#[test]
fn test_cap_algorithm() {
    use ctadl_ir::mir::builder::BasicBlockBuilder;

    let mut f = FunctionData {
        name: "cap_test".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    f.params.push(ParameterType::ByVal);

    let blocks = f.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(None));
    let mut builder = BasicBlockBuilder::new(&mut f.blocks[body], &mut f.locals);

    // x = p0
    let x = builder.new_param_var(ParameterIdx::new(0));

    // t1 = load x.foo
    let t1 = builder.new_local_var("t1");
    builder.create_load(t1.clone(), x.clone(), "foo");

    // t2 = load t1.bar
    let t2 = builder.new_local_var("t2");
    builder.create_load(t2.clone(), t1.clone(), "bar");

    // t3 = load t2.baz
    let t3 = builder.new_local_var("t3");
    builder.create_load(t3.clone(), t2.clone(), "baz");

    builder.create_ret(vec![Exp::Variable(t3)]);

    f.verify().expect("doesn't verify");

    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f, &mut facts, &mut source_info);

    // Verify that the paths were computed and added to paths_dedup (and thus facts.paths)
    let path_strings: HashSet<String> = facts.paths.iter().map(|(p,)| p.to_dot_string()).collect();

    assert!(path_strings.contains(".foo"));
    assert!(path_strings.contains(".foo.bar"));
    assert!(path_strings.contains(".foo.bar.baz"));
}
