use super::*;

use smallvec::smallvec;

use super::GLOBALS_INDEX;
use crate::facts as fx;
use crate::facts::{FlowVariable, FlowVertex, TaintEndpoint};
use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{IndexFacts, taint_index};
use crate::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};
use ctadl_ir::index::{idx::Idx, index_vec::IndexVec};
use ctadl_ir::indexvec;
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
    ssa::transform(&mut f_ssa);
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
    ssa::transform(&mut f_ssa);
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
    assert_eq!(result.summary.len(), 3);
}

#[test]
fn test_basic2_source_sink() {
    let (h, ss) = function_h();
    let mut h_ssa = h.clone();
    log::trace!("{h}");
    ssa::transform(&mut h_ssa);
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
        endpoints: [ss.source.clone(), ss.sink.clone()]
            .into_iter()
            .map(|e| (QueryEndpoint::from_taint_endpoint(&source_info.sites, e),))
            .collect(),
    };
    let query_result = taint_analysis(qfacts);
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
}

// Test Phi instruction with control flow
#[test]
fn test_phi_instruction() {
    let f = function_with_phi();
    let mut f_ssa = f.clone();
    log::trace!("Phi function before transform: {f}");
    ssa::transform(&mut f_ssa);
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
    ssa::transform(&mut f_ssa);
    log::trace!("Update function after transform: {f_ssa}");
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_function(&f_ssa, &mut facts, &mut source_info);
    let result = taint_index(facts);
    log::trace!("Update result: {:#?}", result);
    assert!(!result.summary.is_empty());
}

// Test that local variables flow into fields of globals, not globals index itself
#[test]
fn test_local_to_global_field() {
    let f = function_with_param_to_global_field();
    let mut f_ssa = f.clone();
    log::trace!("Local to global field function before transform: {f}");
    ssa::transform(&mut f_ssa);
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
    f.params.push(ParameterType::ByVal);
    f.params.push(ParameterType::ByVal);
    let blocks = f.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let a = AccessPath {
            variable_ref: VariableRef::new_local("a".to_string()),
            path: Default::default(),
        };
        let p = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
            path: Default::default(),
        };
        let q = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(1)),
            path: Default::default(),
        };
        let stmts: IndexVec<StatementIdx, _> = indexvec![
            Statement::new_kind(StatementKind::assign_or_update(
                a.clone(),
                Exp::AccessPath(q)
            )),
            Statement::new_kind(StatementKind::assign_or_update(
                p.clone(),
                Exp::AccessPath(a.clone())
            ))
        ];
        let body_block = &mut f[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::AccessPath(p)],
        }));
    }
    f.verify().expect("doesn't verify");
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
    f.params.push(ParameterType::ByVal);
    f.params.push(ParameterType::ByVal);
    let blocks = f.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let a = AccessPath {
            variable_ref: VariableRef::new_local("a".to_string()),
            path: Default::default(),
        };
        let b = AccessPath {
            variable_ref: VariableRef::new_local("b".to_string()),
            path: Default::default(),
        };
        let p = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
            path: Default::default(),
        };
        let q = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(1)),
            path: Default::default(),
        };
        let stmts: IndexVec<StatementIdx, _> = indexvec![
            Statement::new_kind(StatementKind::assign(
                a.variable_ref.clone(),
                [Exp::AccessPath(q), Exp::AccessPath(b)]
            )),
            Statement::new_kind(StatementKind::assign_or_update(
                p.clone(),
                Exp::AccessPath(a.clone())
            ))
        ];
        let body_block = &mut f[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::AccessPath(p)],
        }));
    }
    f.verify().expect("doesn't verify");
    f
}

//def G(b) {
//  c = F(a, b);
//  return c;
//}
fn function_g() -> FunctionData {
    let mut g = FunctionData {
        name: "G".to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    g.params.push(ParameterType::ByVal);
    let a = AccessPath {
        variable_ref: VariableRef::new_local("a".to_string()),
        path: Default::default(),
    };
    let b = AccessPath {
        variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
        path: Default::default(),
    };
    let c = AccessPath {
        variable_ref: VariableRef::new_local("c".to_string()),
        path: Default::default(),
    };
    let call_edges = CallEdges::Explicit(smallvec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> =
        indexvec![Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: vec![c.variable_ref.clone()].into(),
            args: vec![Exp::AccessPath(a), Exp::AccessPath(b)].into()
        })];
    let blocks = g.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return {
            args: vec![Exp::AccessPath(c)].into(),
        },
    ))));
    let body_block = &mut g[body];
    body_block.extend(stmts);
    g.verify().expect("doesn't verify");
    g
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
    f.params.push(ParameterType::ByVal);
    f.params.push(ParameterType::ByVal);
    let blocks = f.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let a = AccessPath {
            variable_ref: VariableRef::new_local("a".to_string()),
            path: Default::default(),
        };
        let p = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
            path: Default::default(),
        };
        let q = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(1)),
            path: Default::default(),
        };
        let stmts: IndexVec<StatementIdx, _> = indexvec![
            Statement::new_kind(StatementKind::assign_or_update(
                a.clone(),
                Exp::AccessPath(q)
            )),
            Statement::new_kind(StatementKind::assign_or_update(
                p.clone(),
                Exp::AccessPath(a.clone())
            ))
        ];
        let body_block = &mut f[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::AccessPath(p)],
        }));
    }
    let ss = SourceSinkQuery {
        source: TaintEndpoint {
            infunc: fx::Function(f.name.clone().into()),
            vertex: FlowVertex(FlowVariable::Formal(1i8.into()), fx::Path::empty()),
            label: fx::Label("Net".into()),
            direction: fx::TaintDirection::Forward,
        },
        sink: TaintEndpoint {
            infunc: fx::Function(f.name.clone().into()),
            vertex: FlowVertex(FlowVariable::Formal(0i8.into()), fx::Path::empty()),
            label: fx::Label("Net".into()),
            direction: fx::TaintDirection::Backward,
        },
    };
    f.verify().expect("doesn't verify");
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
    let x = VariableRef::new_local("x".to_string());

    // True branch: x = a (using builder API)
    let mut true_builder = BasicBlockBuilder::new(&mut f[true_branch]);
    true_builder.create_assign_or_update(
        true_builder.new_access_path(x.clone(), Vec::<&str>::new()),
        Exp::AccessPath(true_builder.new_access_path(a, Vec::<&str>::new())),
    );

    // False branch: x = b (using builder API)
    let mut false_builder = BasicBlockBuilder::new(&mut f[false_branch]);
    false_builder.create_assign_or_update(
        false_builder.new_access_path(x.clone(), Vec::<&str>::new()),
        Exp::AccessPath(false_builder.new_access_path(b, Vec::<&str>::new())),
    );

    // Merge block will get phi node during SSA conversion (using builder API)
    let mut merge_builder = BasicBlockBuilder::new(&mut f[merge]);
    merge_builder.create_ret(vec![Exp::AccessPath(
        merge_builder.new_access_path(x, Vec::<&str>::new()),
    )]);

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
    let mut builder = BasicBlockBuilder::new(&mut f[body]);

    // Create variables using builder helpers
    let s_var = builder.new_param_var(ParameterIdx::new(0));
    let new_value = builder.new_local_var("new_value");
    let s_access = builder.new_access_path(s_var.clone(), vec!["field"]);

    // Create update statement using builder API
    builder.create_update(
        s_access,
        Exp::AccessPath(builder.new_access_path(new_value.clone(), Vec::<&str>::new())),
    );

    // Create return statement using builder API
    builder.create_ret(vec![Exp::AccessPath(
        builder.new_access_path(s_var, Vec::<&str>::new()),
    )]);

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
    let mut builder = BasicBlockBuilder::new(&mut f[body]);

    // Create local variable and assign it a value
    let local_var = builder.new_param_var(ParameterIdx::new(0));

    // Create globals access and update its field with local_var
    let globals_var = builder.new_global_var();
    let globals_field_access = builder.new_access_path(globals_var.clone(), vec!["field"]);

    // This is the key assignment: globals.field = local_var
    builder.create_update(
        globals_field_access,
        Exp::AccessPath(builder.new_access_path(local_var.clone(), Vec::<&str>::new())),
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
    let mut builder = BasicBlockBuilder::new(&mut f[body]);

    // x = p0
    let x = builder.new_param_var(ParameterIdx::new(0));

    // t1 = x.foo
    let t1 = builder.new_local_var("t1");
    let x_foo = builder.new_access_path(x.clone(), vec!["foo"]);
    builder.create_assign(t1.clone(), vec![Exp::AccessPath(x_foo)]);

    // t2 = t1.bar
    let t2 = builder.new_local_var("t2");
    let t1_bar = builder.new_access_path(t1.clone(), vec!["bar"]);
    builder.create_assign(t2.clone(), vec![Exp::AccessPath(t1_bar)]);

    // t3 = t2.baz
    let t3 = builder.new_local_var("t3");
    let t2_baz = builder.new_access_path(t2.clone(), vec!["baz"]);
    builder.create_assign(t3.clone(), vec![Exp::AccessPath(t2_baz)]);

    builder.create_ret(vec![Exp::AccessPath(
        builder.new_access_path(t3, Vec::<&str>::new()),
    )]);

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
