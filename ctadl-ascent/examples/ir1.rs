use smallvec::smallvec;

use ctadl_ir::index::{idx::Idx, index_vec::IndexVec};
use ctadl_ir::*;

fn main() {
    let mut program = program_h();
    program.functions.push(function_f());
    program.functions.push(function_g());
    program.functions.push(function_g1());
    println!("{program}");
}

// def F(p, q)
// {
//   a = q;
//   p = a;
//   return p;
// }
fn function_f() -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name("F".to_string());
    f.set_return_type(ReturnType { arity: 1 });
    f.params.push(ParameterType::SharedFields);
    f.params.push(ParameterType::SharedFields);
    let a_idx = f.intern_local("a");
    let blocks = f.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let a = AccessPath::from(VariableRef::new_local_idx(a_idx));
        let p = AccessPath::from(VariableRef::new_parameter(ParameterIdx::new(0)));
        let q = AccessPath::from(VariableRef::new_parameter(ParameterIdx::new(1)));
        let stmts: IndexVec<StatementIdx, _> = indexvec![
            Statement::new_kind(StatementKind::assign(a.base.clone(), [Exp::from(q)])),
            Statement::new_kind(StatementKind::assign(
                p.base.clone(),
                [Exp::from(a.clone())]
            ))
        ];
        let body_block = &mut f[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::from(p)],
        }));
    }
    f
}

//def G(b) {
//  a = 1;
//  c = F(a, b);
//  return c;
//}
fn function_g() -> FunctionData {
    let mut g = FunctionData {
        name: "G".to_string(),
        ..Default::default()
    };
    g.set_return_type(ReturnType { arity: 1 });
    g.params.push(ParameterType::SharedFields);
    let a_idx = g.intern_local("a");
    let c_idx = g.intern_local("c");
    let a = AccessPath::from(VariableRef::new_local_idx(a_idx));
    let b = AccessPath::from(VariableRef::new_parameter(ParameterIdx::new(0)));
    let c = AccessPath::from(VariableRef::new_local_idx(c_idx));
    let call_edges = CallEdges::Explicit(ctadl_ir::thin_vec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign(
            a.base.clone(),
            [Exp::Bytes(1u8.to_be_bytes().to_vec())]
        )),
        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: vec![VariableRef::new_local_idx(c_idx)].into(),
            args: vec![Exp::from(a), Exp::from(b)].into()
        }),
    ];
    let blocks = g.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return {
            args: vec![Exp::from(c)].into(),
        },
    ))));
    let body_block = &mut g[body];
    body_block.extend(stmts);
    g
}

// c1 is never set anywhere: SSA must handle a use with no def.
//def G1(b) {
//  a = 1;
//  c = F(a, b);
//  return c1;
//}
fn function_g1() -> FunctionData {
    let mut g = FunctionData {
        name: "G1".to_string(),
        ..Default::default()
    };
    g.set_return_type(ReturnType { arity: 1 });
    g.params.push(ParameterType::SharedFields);
    let a_idx = g.intern_local("a");
    let c_idx = g.intern_local("c");
    let c1_idx = g.intern_local("c1");
    let a = AccessPath::from(VariableRef::new_local_idx(a_idx));
    let b = AccessPath::from(VariableRef::new_parameter(ParameterIdx::new(0)));
    let c1 = AccessPath::from(VariableRef::new_local_idx(c1_idx));
    let call_edges = CallEdges::Explicit(ctadl_ir::thin_vec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign(
            a.base.clone(),
            [Exp::Bytes(1u8.to_be_bytes().to_vec())]
        )),
        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: vec![VariableRef::new_local_idx(c_idx)].into(),
            args: vec![Exp::from(a), Exp::from(b)].into()
        }),
    ];
    let blocks = g.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return {
            args: vec![Exp::from(c1)].into(),
        },
    ))));
    let body_block = &mut g[body];
    body_block.extend(stmts);
    g
}

// def F(p)
// {
//   $g.bar = p;
// }
fn program_h() -> Program {
    let mut program = Program::default();
    let g = program.declare_global("g", None);
    program.functions.push(FunctionData::default());
    let h = &mut program.functions[0.into()];
    h.set_name("F".to_string());
    h.params.push(ParameterType::SharedFields);
    let blocks = h.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let p = AccessPath::from(VariableRef::new_parameter(ParameterIdx::new(0)));
        let global_ref = VariableRef::global(g);
        let stmts: IndexVec<StatementIdx, _> =
            indexvec![Statement::new_kind(StatementKind::store(
                AccessPath::from(global_ref.clone()),
                ctadl_ir::cir::PathSegment::field("bar"),
                Exp::from(p.clone()),
            ))];
        let body_block = &mut h[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        }));
    }
    program
}
