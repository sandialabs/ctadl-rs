use internment::ArcIntern;
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
        // stmts.extend(]));
        let body_block = &mut f[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::AccessPath(p)],
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
    let mut g = FunctionData::default();
    g.name = "G".to_string();
    g.set_return_type(ReturnType { arity: 1 });
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
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign_or_update(
            a.clone(),
            Exp::Bytes(1u8.to_be_bytes().to_vec())
        )),
        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: vec![VariableRef::new_local("c".to_string())].into(),
            args: vec![Exp::AccessPath(a), Exp::AccessPath(b)].into()
        }),
    ];
    let blocks = g.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return {
            args: vec![Exp::AccessPath(c)].into(),
        },
    ))));
    let body_block = &mut g[body];
    body_block.extend(stmts);
    g
}

// c1 is never set anywhere, this was a bug in ssa
//def G1(b) {
//  a = 1;
//  c = F(a, b);
//  return c1;
//}
fn function_g1() -> FunctionData {
    let mut g = FunctionData::default();
    g.name = "G1".to_string();
    g.set_return_type(ReturnType { arity: 1 });
    g.params.push(ParameterType::ByVal);
    let a = AccessPath {
        variable_ref: VariableRef::new_local("a".to_string()),
        path: Default::default(),
    };
    let b = AccessPath {
        variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
        path: Default::default(),
    };
    let c1 = AccessPath {
        variable_ref: VariableRef::new_local("c1".to_string()),
        path: Default::default(),
    };
    let call_edges = CallEdges::Explicit(smallvec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign_or_update(
            a.clone(),
            Exp::Bytes(1u8.to_be_bytes().to_vec())
        )),
        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: vec![VariableRef::new_local("c".to_string())].into(),
            args: vec![Exp::AccessPath(a), Exp::AccessPath(b)].into()
        }),
    ];
    let blocks = g.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return {
            args: vec![Exp::AccessPath(c1)].into(),
        },
    ))));
    let body_block = &mut g[body];
    body_block.extend(stmts);
    g
}

// def F(p)
// {
//   %global.bar = update(p;
// }
fn program_h() -> Program {
    let mut program = Program::default();
    program.functions.push(FunctionData::default());
    let h = &mut program.functions[0.into()];
    h.set_name("F".to_string());
    h.params.push(ParameterType::ByVal);
    let blocks = h.blocks.blocks_mut();
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let p = AccessPath {
            variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
            path: Default::default(),
        };
        let global_ref = VariableRef::new_var_ref(ArcIntern::new(Variable::GlobalHeap));
        let stmts: IndexVec<StatementIdx, _> =
            indexvec![Statement::new_kind(StatementKind::Update {
                dest: (global_ref.clone(), ["bar"].into_iter().collect()),
                source: global_ref.clone(),
                value: Exp::AccessPath(p.clone()),
            })];
        let body_block = &mut h[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        }));
    }
    program
}
