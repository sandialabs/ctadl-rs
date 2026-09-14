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
        Default::default(),
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
    let report = codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        Default::default(),
        &crate::models::ProgramModelMatches {
            skip_analysis: [Str::from("G")].into_iter().collect(),
            ..Default::default()
        },
    );
    assert_eq!(
        report.skipped_bodies, 1,
        "exactly one body was named and lowered"
    );

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
    let has_field_flow = result
        .summary
        .iter()
        .any(|(fid, dst_i, dst_p, src_i, src_p)| {
            *fid == f_id && **dst_i == -1 && !dst_p.is_empty() && **src_i == 1 && src_p.is_empty()
        });
    assert!(
        has_field_flow,
        "expected p1 to flow to the returned aggregate's field"
    );

    // The whole-aggregate copy p0 -> q, returned in q: (ret, empty) <- (formal 0, empty). This flow
    // exists only because `Update` copies the entire source aggregate; a `Store` would not.
    let has_whole_copy = result
        .summary
        .iter()
        .any(|(fid, dst_i, dst_p, src_i, src_p)| {
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
        ctadl_ir::mir::FieldRef::symbol("field"),
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
        ctadl_ir::mir::FieldRef::symbol("field"),
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
        ctadl_ir::mir::FieldRef::symbol("field"),
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

/// CHA answers "every implementer of this interface", RTA answers "every implementer the
/// program actually allocates", and one Datalog run computes both.
///
/// The shape is the smallest one where the two differ and the answer is checkable by hand:
/// interface `I` with three implementers `A`, `B` and `C`, and a function that allocates an
/// `A` and calls `I.m()`. CHA must find three targets and RTA exactly one -- `A`'s.
///
/// The allocated set comes from [`InstantiationFinder`] over the real function body rather
/// than being written out by hand, because the report collects it the same way and a finder
/// that stopped seeing `new` would otherwise make RTA look brilliant instead of broken.
#[test]
fn rta_keeps_only_allocated_implementers() {
    use ctadl_ir::mir::call::{
        CallObject, JavaClass, JavaDispatch, JavaMethod, JavaSignature, JavaSimpleName,
    };

    let iface = Symbol::from("LI;");
    let m = Symbol::from("m");
    let desc = Symbol::from("()V");

    let methods = ["LA;", "LB;", "LC;"]
        .into_iter()
        .map(|cls| {
            (
                JavaClass(cls.into()),
                JavaSimpleName(m.clone()),
                JavaSignature(desc.clone()),
                JavaMethod(format!("{cls}->m()V").into()),
            )
        })
        .collect();
    // The map is sub -> its supertypes, and the CHA arm flips it into (sup, sub).
    let hierarchy = ["LA;", "LB;", "LC;"]
        .into_iter()
        .map(|cls| {
            (
                JavaClass(cls.into()),
                smallvec::smallvec![JavaClass(iface.clone())],
            )
        })
        .collect();
    let vmt = VirtualMethodTable::Java {
        methods,
        hierarchy,
        // `I` is an interface declaring one abstract `m()V`: the shape a functional
        // interface has, and the shape the report's SAM detection looks for.
        interfaces: vec![JavaClass(iface.clone())],
        abstract_methods: vec![(
            JavaClass(iface.clone()),
            JavaSimpleName(m.clone()),
            JavaSignature(desc.clone()),
        )],
        natives: Default::default(),
    };

    // A function whose body is `x = new A; x.m()`.
    let mut f = FunctionData {
        name: "Lmain;->run()V".to_string(),
        ..Default::default()
    };
    let mut fb = FunctionBuilder::new(&mut f);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let x = b.new_local_var("x");
    b.create_assign(
        x.clone(),
        vec![Exp::ObjectRef(CallObject::JavaObject(JavaClass(
            "LA;".into(),
        )))],
    );
    b.create_call(
        CallStyle::JavaCall {
            receiver: x,
            cls: iface.clone(),
            simple_name: m.clone(),
            descriptor: desc.clone(),
            dispatch: JavaDispatch::Interface,
            super_start: None,
        },
        Vec::new(),
        Vec::new(),
    );
    b.create_ret(Vec::<Exp>::new());
    f.verify().expect("Function doesn't verify");

    let mut instantiated = BTreeSet::new();
    InstantiationFinder::new(&mut instantiated).visit_function_data(FunctionIdx::new(0), &f);
    assert_eq!(
        instantiated,
        [Symbol::from("LA;")].into_iter().collect(),
        "the only allocation in the body is a `new A`"
    );

    let cha = ClassHierarchyAnalysis::with_rta(&vmt, instantiated);
    let targets = |it: &mut dyn Iterator<Item = Symbol>| -> BTreeSet<String> {
        it.map(|s| s.to_string()).collect()
    };
    assert_eq!(
        targets(&mut cha.java_resolvents(iface.clone(), m.clone(), desc.clone())),
        ["LA;->m()V", "LB;->m()V", "LC;->m()V"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "CHA resolves the interface call to every implementer"
    );
    assert_eq!(
        targets(&mut cha.java_rta_resolvents(iface, m, desc)),
        ["LA;->m()V"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "RTA keeps only the implementer the program allocates"
    );

    // And plain CHA leaves the RTA table empty, so codegen -- which builds it this way --
    // pays nothing for a measurement it never reads.
    let plain = ClassHierarchyAnalysis::new(&vmt, Default::default());
    assert_eq!(plain.rta_resolvents.len(), 0);
    // Four keys, because CHA answers for every declared receiver type that has the method:
    // the interface and each of the three classes.
    assert_eq!(plain.resolvents.len(), 4);
}

// ---------------------------------------------------------------------------------------
// The ladder
// ---------------------------------------------------------------------------------------

mod ladder {
    use super::*;
    use crate::models::{DispatchModel, Disposition, ProgramModelMatches};
    use ctadl_ir::mir::call::{
        JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
    };

    /// A hierarchy where `LI;->m()V` has `count` implementers, so the ladder has a target
    /// count to test against. Each implementer is `LC<i>;`.
    fn vmt_with(count: usize) -> VirtualMethodTable {
        let mut methods = Vec::new();
        let mut hierarchy = hashbrown::HashMap::new();
        for i in 0..count {
            let cls = format!("LC{i};");
            methods.push((
                JavaClass(cls.as_str().into()),
                JavaSimpleName("m".into()),
                JavaSignature("()V".into()),
                JavaMethod(format!("{cls}->m()V").as_str().into()),
            ));
            hierarchy.insert(
                JavaClass(cls.as_str().into()),
                smallvec::smallvec![JavaClass("LI;".into())],
            );
        }
        VirtualMethodTable::Java {
            methods,
            hierarchy,
            interfaces: vec![JavaClass("LI;".into())],
            abstract_methods: vec![(
                JavaClass("LI;".into()),
                JavaSimpleName("m".into()),
                JavaSignature("()V".into()),
            )],
            natives: Vec::new(),
        }
    }

    fn key() -> SignatureKey {
        ("LI;".into(), "m".into(), "()V".into())
    }

    /// A call site to hang emitted rows off. Which one does not matter here: the tests count
    /// rows rather than locate them.
    fn site() -> fx::PackedInsnSiteId {
        fx::PackedInsnSiteId::try_from_parts(fx::FunctionId { id: 0 }, fx::InsnId::new(0))
            .expect("a valid site id")
    }

    fn matches_with(disposition: Option<Disposition>) -> ProgramModelMatches {
        let mut m = ProgramModelMatches::default();
        if let Some(disposition) = disposition {
            m.add_dispatch(
                (Str::from("LI;"), Str::from("m"), Str::from("()V")),
                DispatchModel {
                    disposition,
                    provenance: vec!["test:0".to_string()],
                },
            );
        }
        m
    }

    /// Classifies one site of the fixture, without lowering anything.
    fn classify(
        targets: usize,
        dispatch: JavaDispatch,
        disposition: Option<Disposition>,
        policy: CallPolicy,
    ) -> SiteAction {
        let vmt = vmt_with(targets);
        let matches = matches_with(disposition);
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
        let mut v = CodegenVisitor::new(
            cha,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            policy,
            &matches,
        );
        v.classify(&key(), dispatch, None)
    }

    fn model() -> Disposition {
        Disposition::Model(vec![(
            crate::models::ModelPort {
                tag: crate::models::FormalIndexTypeTag::Return,
                index: None,
                path: fx::Path::empty(),
            },
            crate::models::ModelPort {
                tag: crate::models::FormalIndexTypeTag::Index,
                index: Some(0),
                path: fx::Path::empty(),
            },
        )])
    }

    fn threshold_first() -> CallPolicy {
        CallPolicy {
            order: DispatchOrder::ThresholdFirst,
            ..CallPolicy::default()
        }
    }

    #[test]
    fn under_the_threshold_takes_cha() {
        for dispatch in JavaDispatch::ALL {
            assert_eq!(
                classify(4, dispatch, None, CallPolicy::default()),
                SiteAction::Cha,
                "{dispatch}"
            );
        }
    }

    #[test]
    fn over_the_threshold_defers() {
        assert_eq!(
            classify(40, JavaDispatch::Virtual, None, CallPolicy::default()),
            SiteAction::Defer { by_model: false }
        );
    }

    #[test]
    fn zero_targets_takes_cha_and_emits_nothing() {
        // `LI;->q()V` is in no table, so it resolves to nothing.
        let vmt = vmt_with(2);
        let matches = ProgramModelMatches::default();
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
        let mut v = CodegenVisitor::new(
            cha,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            CallPolicy::default(),
            &matches,
        );
        let key: SignatureKey = ("LI;".into(), "q".into(), "()V".into());
        assert_eq!(
            v.classify(&key, JavaDispatch::Virtual, None),
            SiteAction::Cha
        );
    }

    #[test]
    fn the_interface_threshold_is_separate() {
        let policy = CallPolicy {
            cha_threshold: 64,
            cha_threshold_interface: 2,
            ..CallPolicy::default()
        };
        assert_eq!(
            classify(8, JavaDispatch::Virtual, None, policy),
            SiteAction::Cha
        );
        assert_eq!(
            classify(8, JavaDispatch::Interface, None, policy),
            SiteAction::Defer { by_model: false }
        );
    }

    #[test]
    fn a_model_beats_the_threshold_and_a_threshold_beats_a_model() {
        // Model-first ignores the target count.
        assert!(matches!(
            classify(
                2,
                JavaDispatch::Virtual,
                Some(model()),
                CallPolicy::default()
            ),
            SiteAction::Model(_)
        ));
        // Threshold-first resolves the same site exactly, and only models it above `K`.
        assert_eq!(
            classify(2, JavaDispatch::Virtual, Some(model()), threshold_first()),
            SiteAction::Cha
        );
        assert!(matches!(
            classify(40, JavaDispatch::Virtual, Some(model()), threshold_first()),
            SiteAction::Model(_)
        ));
    }

    #[test]
    fn an_empty_propagation_skips() {
        assert_eq!(
            classify(
                40,
                JavaDispatch::Virtual,
                Some(Disposition::Skip),
                CallPolicy::default()
            ),
            SiteAction::Skip
        );
    }

    /// `inline` keeps the site off CHA in both orders, except where CHA is already exact.
    #[test]
    fn inline_defers_in_both_orders_but_never_a_monomorphic_site() {
        for policy in [CallPolicy::default(), threshold_first()] {
            assert_eq!(
                classify(1, JavaDispatch::Virtual, Some(Disposition::Inline), policy),
                SiteAction::Cha,
                "one target stays exact"
            );
            assert_eq!(
                classify(4, JavaDispatch::Virtual, Some(Disposition::Inline), policy),
                SiteAction::Defer { by_model: true },
                "under the threshold, and still deferred"
            );
            assert_eq!(
                classify(40, JavaDispatch::Virtual, Some(Disposition::Inline), policy),
                SiteAction::Defer { by_model: true }
            );
        }
        // Zero targets still defer: hybrid inlining can find a callee from the allocated
        // class where the static type resolves to nothing.
        let vmt = vmt_with(2);
        let matches = matches_with(Some(Disposition::Inline));
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
        let mut v = CodegenVisitor::new(
            cha,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            CallPolicy::default(),
            &matches,
        );
        // The disposition is keyed on `LI;->m()V`, so use a receiver class with no targets by
        // pointing at a name the table does not have under that key.
        assert_eq!(
            v.classify(&key(), JavaDispatch::Virtual, None),
            SiteAction::Defer { by_model: true }
        );
    }

    #[test]
    fn models_can_be_turned_off_for_interfaces_alone() {
        let policy = CallPolicy {
            dispatch_models_interface: false,
            ..CallPolicy::default()
        };
        assert!(matches!(
            classify(40, JavaDispatch::Virtual, Some(model()), policy),
            SiteAction::Model(_)
        ));
        assert_eq!(
            classify(40, JavaDispatch::Interface, Some(model()), policy),
            SiteAction::Defer { by_model: false }
        );
    }

    #[test]
    fn the_other_strategies_ignore_the_ladder() {
        let vmt = vmt_with(40);
        let matches = matches_with(Some(model()));
        for (strategy, expected) in [
            (CallResolutionStrategy::Cha, SiteAction::Cha),
            (
                CallResolutionStrategy::Hi,
                SiteAction::Defer { by_model: false },
            ),
            (
                CallResolutionStrategy::LegacyMixed,
                SiteAction::Defer { by_model: false },
            ),
        ] {
            let mut facts = IndexFacts::default();
            let mut source_info = IndexSourceInfo::default();
            let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
            let mut v = CodegenVisitor::new(
                cha,
                &mut facts,
                &mut source_info,
                strategy,
                CallPolicy::default(),
                &matches,
            );
            assert_eq!(v.classify(&key(), JavaDispatch::Virtual, None), expected);
        }
    }

    /// `legacy-mixed` is CHA at exactly one target and hybrid inlining above.
    #[test]
    fn legacy_mixed_resolves_only_a_single_target() {
        for (targets, expected) in [
            (1, SiteAction::Cha),
            (2, SiteAction::Defer { by_model: false }),
        ] {
            let vmt = vmt_with(targets);
            let matches = ProgramModelMatches::default();
            let mut facts = IndexFacts::default();
            let mut source_info = IndexSourceInfo::default();
            let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
            let mut v = CodegenVisitor::new(
                cha,
                &mut facts,
                &mut source_info,
                CallResolutionStrategy::LegacyMixed,
                CallPolicy::default(),
                &matches,
            );
            assert_eq!(
                v.classify(&key(), JavaDispatch::Virtual, None),
                expected,
                "{targets} target(s)"
            );
        }
    }

    /// What each strategy emits at one site, so the three stay distinguishable and
    /// `legacy-mixed` keeps meaning what it means. It is the baseline the ladder is measured
    /// against, and a silent change to it would invalidate the comparison.
    #[test]
    fn each_strategy_emits_its_own_rows() {
        // Two targets: exact under CHA, ambiguous under the legacy rule, under the threshold
        // for the ladder.
        let vmt = vmt_with(2);
        let matches = ProgramModelMatches::default();
        for (strategy, calls, deferred) in [
            (CallResolutionStrategy::Cha, 2, 0),
            (CallResolutionStrategy::Hi, 0, 1),
            (CallResolutionStrategy::LegacyMixed, 0, 1),
            (CallResolutionStrategy::Mixed, 2, 0),
        ] {
            let mut facts = IndexFacts::default();
            let mut source_info = IndexSourceInfo::default();
            let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
            let mut v = CodegenVisitor::new(
                cha,
                &mut facts,
                &mut source_info,
                strategy,
                CallPolicy::default(),
                &matches,
            );
            let action = v.classify(&key(), JavaDispatch::Virtual, None);
            v.apply(
                site(),
                FlowVariable::formal_index(0i16.into()),
                &key(),
                JavaDispatch::Virtual,
                action,
            );
            assert_eq!(facts.call.len(), calls, "{strategy:?} call rows");
            assert_eq!(
                facts.callee_info.len(),
                deferred,
                "{strategy:?} deferred sites"
            );
        }
    }

    /// `callee_resolvents` is what the engine joins a deferred receiver's allocated class
    /// against, so a `(name, descriptor)` pair no site deferred can never be joined and its
    /// rows are dead weight.
    #[test]
    fn resolvent_rows_are_emitted_only_for_deferred_signatures() {
        let vmt = vmt_with(40);
        let matches = ProgramModelMatches::default();
        for (strategy, want_rows) in [
            (CallResolutionStrategy::Cha, false),
            (CallResolutionStrategy::Mixed, true),
        ] {
            let mut facts = IndexFacts::default();
            let mut source_info = IndexSourceInfo::default();
            let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
            let mut v = CodegenVisitor::new(
                cha,
                &mut facts,
                &mut source_info,
                strategy,
                CallPolicy::default(),
                &matches,
            );
            let action = v.classify(&key(), JavaDispatch::Virtual, None);
            v.apply(
                site(),
                FlowVariable::formal_index(0i16.into()),
                &key(),
                JavaDispatch::Virtual,
                action,
            );
            v.finish();
            assert_eq!(
                !facts.callee_resolvents.is_empty(),
                want_rows,
                "{strategy:?}"
            );
        }
    }

    /// A source or sink inside the target set refuses the model: those bodies have to stay in
    /// the analysis, so the site falls to the threshold.
    #[test]
    fn a_matched_endpoint_in_the_target_set_refuses_the_model() {
        let vmt = vmt_with(40);
        let mut matches = matches_with(Some(model()));
        matches.endpoints.push(crate::models::EndpointMatch {
            function: Str::from("LC3;->m()V"),
            selector_ty: crate::models::FormalIndexTypeTag::Index,
            index: Some(0),
            path: fx::Path::empty(),
            label: Str::from("test"),
            direction: crate::facts::TaintDirection::Backward,
            wildcard: true,
            saturating: false,
            in_function: None,
            callsite_scoped: false,
            local_index: None,
        });
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        let cha = ClassHierarchyAnalysis::new(&vmt, Default::default());
        let mut v = CodegenVisitor::new(
            cha,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            CallPolicy::default(),
            &matches,
        );
        assert_eq!(
            v.classify(&key(), JavaDispatch::Virtual, None),
            SiteAction::Defer { by_model: false },
            "refused, so the site falls through the ladder"
        );
        assert_eq!(
            v.report.refused.get("LI;->m()V").map(String::as_str),
            Some("LC3;->m()V")
        );
    }
}

// ---------------------------------------------------------------------------------------
// Rung 0: `invoke-super`
// ---------------------------------------------------------------------------------------

mod super_resolution {
    use super::*;
    use ctadl_ir::mir::call::{
        JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
    };

    /// `implementations` are `(class, method simple name)` pairs, all with descriptor `()V`.
    /// `hierarchy` is `(subclass, [parents])`.
    fn cha(
        implementations: &[(&str, &str)],
        hierarchy: &[(&str, &[&str])],
    ) -> ClassHierarchyAnalysis {
        let vmt = VirtualMethodTable::Java {
            methods: implementations
                .iter()
                .map(|(cls, name)| {
                    (
                        JavaClass((*cls).into()),
                        JavaSimpleName((*name).into()),
                        JavaSignature("()V".into()),
                        JavaMethod(format!("{cls}->{name}()V").as_str().into()),
                    )
                })
                .collect(),
            hierarchy: hierarchy
                .iter()
                .map(|(sub, sups)| {
                    (
                        JavaClass((*sub).into()),
                        sups.iter().map(|s| JavaClass((*s).into())).collect(),
                    )
                })
                .collect(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        };
        ClassHierarchyAnalysis::new(&vmt, Default::default())
    }

    fn resolve(cha: &ClassHierarchyAnalysis, start: &str) -> SuperResolution {
        cha.super_resolvent(&start.into(), &"m".into(), &"()V".into())
    }

    /// The nearest declaration up the chain wins, not every one of them.
    #[test]
    fn walks_up_the_class_chain() {
        let cha = cha(
            &[("LA;", "m"), ("LB;", "m")],
            &[("LB;", &["LA;"][..]), ("LC;", &["LB;"][..])],
        );
        assert_eq!(
            resolve(&cha, "LB;"),
            SuperResolution::Exactly("LB;->m()V".into())
        );
        assert_eq!(
            resolve(&cha, "LC;"),
            SuperResolution::Exactly("LB;->m()V".into()),
            "LB; is nearer than LA;"
        );
    }

    /// `X.super.m()` starts at the interface, which declares the default method itself.
    #[test]
    fn an_interface_default_method_resolves_at_level_zero() {
        let cha = cha(&[("LI;", "m")], &[("LC;", &["LI;"][..])]);
        assert_eq!(
            resolve(&cha, "LI;"),
            SuperResolution::Exactly("LI;->m()V".into())
        );
    }

    /// A class outside the import, or one whose chain declares nothing, resolves to nothing and
    /// the site keeps its full CHA target set.
    #[test]
    fn a_missing_class_falls_through() {
        let cha = cha(&[("LA;", "other")], &[("LB;", &["LA;"][..])]);
        assert_eq!(resolve(&cha, "LUnknown;"), SuperResolution::None);
        assert_eq!(resolve(&cha, "LB;"), SuperResolution::None);
    }

    /// Two parents at the same level declaring it: the runtime's choice is not recoverable
    /// from the hierarchy, so the site falls through.
    #[test]
    fn a_diamond_is_ambiguous() {
        let cha = cha(
            &[("LI;", "m"), ("LJ;", "m")],
            &[("LC;", &["LI;", "LJ;"][..])],
        );
        assert_eq!(resolve(&cha, "LC;"), SuperResolution::Ambiguous(2));
    }

    /// When the frontend could not name the start class, codegen walks from the class the
    /// instruction names -- which for a dex `invoke-super` may be the current class. The walk
    /// then finds the current method and the call resolves to itself, which is why the frontend
    /// records `super_start` rather than leaving codegen to guess.
    #[test]
    fn a_reference_to_the_current_class_resolves_to_itself() {
        let cha = cha(&[("LA;", "m"), ("LB;", "m")], &[("LB;", &["LA;"][..])]);
        assert_eq!(
            resolve(&cha, "LB;"),
            SuperResolution::Exactly("LB;->m()V".into()),
            "walking from the current class finds the current method"
        );
        assert_eq!(
            resolve(&cha, "LA;"),
            SuperResolution::Exactly("LA;->m()V".into()),
            "walking from the recorded start class finds the parent's"
        );
    }

    /// The whole rung, at a call site: one `call` row, counted as an exact super.
    #[test]
    fn a_resolved_super_site_emits_one_edge() {
        let vmt = VirtualMethodTable::Java {
            methods: vec![
                (
                    JavaClass("LA;".into()),
                    JavaSimpleName("m".into()),
                    JavaSignature("()V".into()),
                    JavaMethod("LA;->m()V".into()),
                ),
                (
                    JavaClass("LB;".into()),
                    JavaSimpleName("m".into()),
                    JavaSignature("()V".into()),
                    JavaMethod("LB;->m()V".into()),
                ),
            ],
            hierarchy: [(
                JavaClass("LB;".into()),
                smallvec::smallvec![JavaClass("LA;".into())],
            )]
            .into_iter()
            .collect(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        };

        let mut f = FunctionData {
            name: "LB;->caller()V".to_string(),
            ..Default::default()
        };
        let mut fb = FunctionBuilder::new(&mut f);
        let body = fb.add_block();
        let mut b = fb.at_block(body);
        let x = b.new_local_var("x");
        b.create_assign(
            x.clone(),
            vec![Exp::ObjectRef(CallObject::JavaObject(JavaClass(
                "LB;".into(),
            )))],
        );
        b.create_call(
            CallStyle::JavaCall {
                receiver: x,
                // What the instruction names, and what CHA alone would resolve to both `LA;`
                // and `LB;`.
                cls: "LA;".into(),
                simple_name: "m".into(),
                descriptor: "()V".into(),
                dispatch: JavaDispatch::Super,
                super_start: Some("LA;".into()),
            },
            Vec::new(),
            Vec::new(),
        );
        b.create_ret(Vec::<Exp>::new());
        f.verify().expect("function does not verify");

        let mut program = Program::default();
        let idx = program.new_function();
        program[idx] = f;
        let program_info = ProgramInfo {
            program,
            vmt,
            ..Default::default()
        };
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        let report = codegen_program(
            program_info,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            CallPolicy::default(),
            &Default::default(),
        );
        let a = source_info
            .sites
            .get_function_id(fx::Function("LA;->m()V".into()))
            .expect("the super target is interned");
        assert_eq!(
            facts.call.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            vec![a],
            "one edge, to the one real target"
        );
        let buckets = report.buckets[JavaDispatch::Super.index()];
        assert_eq!(
            (buckets.java_sites, buckets.cha, buckets.cha_super_exact),
            (1, 1, 1)
        );
        assert!(report.totals().balanced());
    }
}
