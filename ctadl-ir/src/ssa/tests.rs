use super::*;

use smallvec::smallvec;

use super::visit::Visitor;
use crate::indexvec;
use crate::mir::call::*;

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
    let a_idx = f.intern_local("a");
    let blocks = f.blocks.blocks_mut();
    let _start = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: vec![BasicBlockIdx::new(1)].into(),
        },
    ))));
    let body = blocks.push(BasicBlockData::new(None));
    {
        let a = AccessPath {
            variable_ref: VariableRef::new_local_idx(a_idx),
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
                [Exp::from(q)]
            )),
            Statement::new_kind(StatementKind::assign(
                p.variable_ref.clone(),
                [Exp::from(a.clone())]
            )),
        ];
        // stmts.extend(]));
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
    g.params.push(ParameterType::ByVal);
    let a_idx = g.intern_local("a");
    let c_idx = g.intern_local("c");
    let a = AccessPath {
        variable_ref: VariableRef::new_local_idx(a_idx),
        path: Default::default(),
    };
    let b = AccessPath {
        variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
        path: Default::default(),
    };
    let c = AccessPath {
        variable_ref: VariableRef::new_local_idx(c_idx),
        path: Default::default(),
    };
    let call_edges = CallEdges::Explicit(thin_vec::thin_vec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign(
            a.variable_ref.clone(),
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

// c1 is never set anywhere, this was a bug in ssa
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
    g.params.push(ParameterType::ByVal);
    let a_idx = g.intern_local("a");
    let c_idx = g.intern_local("c");
    let c1_idx = g.intern_local("c1");
    let a = AccessPath {
        variable_ref: VariableRef::new_local_idx(a_idx),
        path: Default::default(),
    };
    let b = AccessPath {
        variable_ref: VariableRef::new_parameter(ParameterIdx::new(0)),
        path: Default::default(),
    };
    let _c = AccessPath {
        variable_ref: VariableRef::new_local_idx(c_idx),
        path: Default::default(),
    };
    let c1 = AccessPath {
        variable_ref: VariableRef::new_local_idx(c1_idx),
        path: Default::default(),
    };
    let call_edges = CallEdges::Explicit(thin_vec::thin_vec!["F".to_string()]);
    let style = CallStyle::DirectCall { call_edges };
    let stmts: IndexVec<StatementIdx, _> = indexvec![
        Statement::new_kind(StatementKind::assign(
            a.variable_ref.clone(),
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
//   %global.bar = update(p;
// }
fn program_h() -> Program {
    let mut program = Program::default();
    program.functions.push(FunctionData::default());
    let h = &mut program.functions[0.into()];
    h.set_name("F".to_string());
    h.params.push(ParameterType::ByVal);
    let blocks = h.blocks.blocks_mut();
    let _start = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
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
            indexvec![Statement::new_kind(StatementKind::store(
                AccessPath::without_fields(global_ref.clone()),
                FieldPath::symbol("bar"),
                Exp::from(p.clone()),
            )),];
        let body_block = &mut h[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        }));
    }
    program
}

// def F(p)
// {
//   %global = update(%global, .bar := p);
// }
//
// The functional-update counterpart of `program_h`. A `Store` writes a field without defining a
// variable; an `Update` copies the `source` aggregate and defines a *new* version of the
// destination. SSA must therefore version the destination (a fresh version) distinctly from the
// source (the incoming version) it reads.
fn program_h_update() -> Program {
    let mut program = Program::default();
    program.functions.push(FunctionData::default());
    let h = &mut program.functions[0.into()];
    h.set_name("F".to_string());
    h.params.push(ParameterType::ByVal);
    let blocks = h.blocks.blocks_mut();
    let _start = blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
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
            indexvec![Statement::new_kind(StatementKind::update(
                AccessPath::without_fields(global_ref.clone()),
                global_ref.clone(),
                FieldPath::symbol("bar"),
                Exp::from(p.clone()),
            )),];
        let body_block = &mut h[body];
        body_block.extend(stmts);
        body_block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        }));
    }
    program
}

// Call this after running ssa. Checks some SSA properties and verifies the AST.
fn check_ssa_func(f: &FunctionData) {
    let mut verify = MirVerify::default();
    verify.visit_function_data(FunctionIdx::new(0), f);
    assert_eq!(Ok(()), verify.take_error());

    SsaCheck::default().visit_function_data(FunctionIdx::new(0), f);
}

#[test]
fn test_ssa_function_f() {
    let f = function_f();
    let mut f_ssa = f.clone();
    log::trace!("{f}");
    transform(&mut f_ssa, false);
    log::trace!("{f_ssa}");
    check_ssa_func(&f_ssa);
}

#[test]
fn test_ssa_function_g() {
    let g = function_g();
    let mut g_ssa = g.clone();
    log::trace!("{g}");
    transform(&mut g_ssa, false);
    check_ssa_func(&g_ssa);
    assert_eq!(g_ssa[0.into()].len(), 5);
    assert!(g_ssa[0.into()].terminator_opt().is_some());
}

#[test]
fn test_ssa_function_g1() {
    let g = function_g1();
    let mut g_ssa = g.clone();
    log::trace!("{g}");
    transform(&mut g_ssa, false);
    check_ssa_func(&g_ssa);
    assert_eq!(g_ssa[0.into()].len(), 5);
    assert!(g_ssa[0.into()].terminator_opt().is_some());
}

#[test]
fn test_ssa_function_h() {
    let mut p = program_h();
    log::trace!("{p}");
    transform_program(&mut p, false);
    check_ssa_func(&p.functions[0.into()]);
}

#[test]
fn test_ssa_function_h_update() {
    let mut p = program_h_update();
    log::trace!("{p}");
    transform_program(&mut p, false);
    log::trace!("{p}");
    let f = &p.functions[0.into()];
    check_ssa_func(f);

    // A functional update defines a fresh version of the aggregate: SSA must rename the `Update`'s
    // destination to a version distinct from the `source` it reads (the whole point of naming them
    // separately). Both are the global heap here, so `%global_1 = update(%global_0, .bar := p_0)`.
    let (dest, source) = f
        .blocks
        .iter()
        .flat_map(|b| b.statements.iter())
        .find_map(|s| match &s.kind {
            StatementKind::Update { dest, source, .. } => Some((dest.clone(), source.clone())),
            _ => None,
        })
        .expect("expected an Update statement");
    assert_eq!(
        dest.variable_ref.variable, source.variable,
        "same aggregate variable"
    );
    assert!(
        dest.variable_ref.version.is_some(),
        "destination is versioned"
    );
    assert!(source.version.is_some(), "source is versioned");
    assert_ne!(
        dest.variable_ref.version, source.version,
        "destination gets a fresh version distinct from the source read"
    );
}

#[test]
fn test_prune_unreachable() {
    let mut f = FunctionData {
        name: "PruneMe".to_string(),
        return_type: ReturnType { arity: 0 },
        ..Default::default()
    };
    let blocks = f.blocks.blocks_mut();

    // Block 0: goto 1
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: smallvec![BasicBlockIdx::new(1)],
        },
    ))));

    // Block 1: return
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Return { args: smallvec![] },
    ))));

    // Block 2: unreachable, goto 1
    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: smallvec![BasicBlockIdx::new(1)],
        },
    ))));

    assert_eq!(f.blocks.num_nodes(), 3);

    // Transform with prune=true
    transform(&mut f, true);

    // Should have 2 blocks + 1 exit block (from complete()) = 3 blocks
    // If it didn't prune, it would have 4 blocks (and panic)
    assert_eq!(f.blocks.num_nodes(), 3);
}

/// Checks for multiply-defined variables
#[derive(Default, Debug)]
struct SsaCheck {
    defs: HashSet<VariableRef>,
}

impl Visitor for SsaCheck {
    fn visit_statement_kind(&mut self, stmt: &StatementKind, location: Location) {
        self.super_statement_kind(stmt, location);
        for dst in stmt.iter_dst_var() {
            assert!(self.defs.insert(dst.clone()), "multiple definitions: {dst}");
        }
    }
}

//#[test]
//fn test_simple_nopred() {
//    // a = 42
//    // b = a
//    // c = a + b
//    // a = c
//    // c = a
//    let blocks = BasicBlocks::new(
//        indexvec![BasicBlockData {
//            statements: indexvec![
//                StatementKind::Assign {
//                    dst: "a".into(),
//                    src: vec![Exp::Const(42)]
//                },
//                StatementKind::Assign {
//                    dst: "b".into(),
//                    src: vec![Exp::AccessPath("a".into())]
//                },
//                StatementKind::Assign {
//                    dst: "c".into(),
//                    src: vec![Exp::AccessPath("a".into()), Exp::AccessPath("b".into())]
//                },
//                StatementKind::Assign {
//                    dst: "a".into(),
//                    src: vec![Exp::AccessPath("c".into()), Exp::Const(23)]
//                },
//                StatementKind::Assign {
//                    dst: "c".into(),
//                    src: vec![Exp::AccessPath("a".into())]
//                },
//            ],
//        }],
//        Default::default(),
//    );
//    let function = FunctionData::new("test", Params::new([]), blocks);
//    let mut ctx = SsaContext::new(&function);
//    assert_eq!(ctx.counter.count, 1);
//    ctx.build();
//    // v1: 42
//    // v2: v1 + v1
//    // v3: 23
//    // v4: v2 + v3
//    assert!(ctx.uses.get(&Value::Id(2)).unwrap().contains(&Value::Id(1)));
//    assert!(ctx.uses.get(&Value::Id(4)).unwrap().contains(&Value::Id(2)));
//    assert!(ctx.uses.get(&Value::Id(4)).unwrap().contains(&Value::Id(3)));
//}

//#[test]
//fn test_simple_pred() {
//    // block 0:
//    // d = 192
//    //
//    // block 1:
//    // a = 42
//    // b = a
//    // c = a + b
//    // a = c
//    // note that d is not defined
//    // c = a + d

//    let basic_blocks = indexvec![
//        // block 0:
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "d".into(),
//                src: vec![Exp::Const(192)]
//            }],
//        },
//        // block 1:
//        BasicBlockData {
//            statements: indexvec![
//                StatementKind::Assign {
//                    dst: "a".into(),
//                    src: vec![Exp::Const(42)]
//                },
//                StatementKind::Assign {
//                    dst: "b".into(),
//                    src: vec![Exp::AccessPath("a".into())]
//                },
//                StatementKind::Assign {
//                    dst: "c".into(),
//                    src: vec![Exp::AccessPath("a".into()), Exp::AccessPath("b".into())]
//                },
//                StatementKind::Assign {
//                    dst: "a".into(),
//                    src: vec![Exp::AccessPath("c".into()), Exp::Const(23)]
//                },
//                StatementKind::Assign {
//                    dst: "c".into(),
//                    src: vec![Exp::AccessPath("a".into()), Exp::AccessPath("d".into())]
//                },
//            ],
//        }
//    ];
//    let edges = vec![(BasicBlockIdx::new(0), BasicBlockIdx::new(1))];
//    let blocks = BasicBlocks::new(basic_blocks, edges);
//    let function = FunctionData::new("test", Params::new([]), blocks);
//    let mut ctx = SsaContext::new(&function);
//    assert_eq!(ctx.counter.count, 1);
//    ctx.build();
//    // v1: 192
//    //
//    // v2: 42
//    // v3: v2 + v2
//    // v4: 23
//    // v5: v3 + v4
//    // v6: v5 + v1
//    assert!(ctx.uses.get(&Value::Id(3)).unwrap().contains(&Value::Id(2)));
//    assert!(ctx.uses.get(&Value::Id(5)).unwrap().contains(&Value::Id(3)));
//    assert!(ctx.uses.get(&Value::Id(5)).unwrap().contains(&Value::Id(4)));
//    assert!(ctx.uses.get(&Value::Id(6)).unwrap().contains(&Value::Id(5)));
//    assert!(ctx.uses.get(&Value::Id(6)).unwrap().contains(&Value::Id(1)));
//}

//#[test]
//fn test_simple_recursive() {
//    let basic_blocks = indexvec![
//        // block 0:
//        // s0: x = 42
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "x".into(),
//                src: vec![Exp::Const(42)]
//            }],
//        },
//        // block 1:
//        // loop header
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 2:
//        // if test
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 3:
//        // [if true]
//        // s0: x = something
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "x".into(),
//                src: vec![Exp::Const(90)]
//            }],
//        },
//        // block 4:
//        // if false
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 5:
//        // join after if
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 6:
//        // use of x after loop
//        // s0: y = x + 1
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "y".into(),
//                src: vec![Exp::AccessPath("x".into()), Exp::Const(1)]
//            }],
//        },
//    ];
//    let edges = vec![
//        (BasicBlockIdx::new(0), BasicBlockIdx::new(1)),
//        (BasicBlockIdx::new(1), BasicBlockIdx::new(2)),
//        (BasicBlockIdx::new(1), BasicBlockIdx::new(6)),
//        (BasicBlockIdx::new(2), BasicBlockIdx::new(3)),
//        (BasicBlockIdx::new(2), BasicBlockIdx::new(4)),
//        (BasicBlockIdx::new(3), BasicBlockIdx::new(5)),
//        (BasicBlockIdx::new(4), BasicBlockIdx::new(5)),
//        (BasicBlockIdx::new(5), BasicBlockIdx::new(1)),
//    ];
//    let blocks = BasicBlocks::new(basic_blocks, edges);
//    let function = FunctionData::new("test", Params::new([]), blocks);
//    let mut ctx = SsaContext::new(&function);
//    ctx.set_counter_value(0); // this example starts at 0 in paper, sigh
//    // Fill the blocks in order of the paper
//    for &i in &[0, 3, 6] {
//        ctx.seal_block(i.into());
//        ctx.fill_block(i.into());
//    }
//}

//#[test]
//fn test_access_path_recursive() {
//    let basic_blocks = indexvec![
//        // block 0:
//        // s0: x = 42
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: AccessPath::new("x".into(), ["foo".to_string(), "bar".to_string()]),
//                src: vec![Exp::Const(42)]
//            }],
//        },
//        // block 1:
//        // loop header
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 2:
//        // if test
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 3:
//        // [if true]
//        // s0: x = something
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "x".into(),
//                src: vec![Exp::Const(90)]
//            }],
//        },
//        // block 4:
//        // if false
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 5:
//        // join after if
//        BasicBlockData {
//            statements: indexvec![],
//        },
//        // block 6:
//        // use of x after loop
//        // s0: y = x + 1
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "y".into(),
//                src: vec![
//                    Exp::AccessPath(AccessPath::new(
//                        "x".into(),
//                        ["foo".to_string(), "bar".to_string()]
//                    )),
//                    Exp::Const(1)
//                ]
//            }],
//        },
//    ];
//    let edges = vec![
//        (BasicBlockIdx::new(0), BasicBlockIdx::new(1)),
//        (BasicBlockIdx::new(1), BasicBlockIdx::new(2)),
//        (BasicBlockIdx::new(1), BasicBlockIdx::new(6)),
//        (BasicBlockIdx::new(2), BasicBlockIdx::new(3)),
//        (BasicBlockIdx::new(2), BasicBlockIdx::new(4)),
//        (BasicBlockIdx::new(3), BasicBlockIdx::new(5)),
//        (BasicBlockIdx::new(4), BasicBlockIdx::new(5)),
//        (BasicBlockIdx::new(5), BasicBlockIdx::new(1)),
//    ];
//    let blocks = BasicBlocks::new(basic_blocks, edges);
//    let function = FunctionData::new("test", Params::new([]), blocks);
//    let mut ctx = SsaContext::new(&function);
//    ctx.set_counter_value(0); // this example starts at 0 in paper, sigh
//    // Fill the blocks in order of the paper
//    for &i in &[0, 3, 6] {
//        ctx.seal_block(i.into());
//        ctx.fill_block(i.into());
//        //eprintln!("{ctx}");
//    }
//}

//#[test]
//fn test_irreducible_ssa() {
//    let basic_blocks = indexvec![
//        // block 0:
//        // s0: x = 42
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "x".into(),
//                src: vec![Exp::Const(42)]
//            }],
//        },
//        Default::default(), // block 1
//        Default::default(), // block 2
//        // block 4
//        BasicBlockData {
//            statements: indexvec![StatementKind::Assign {
//                dst: "y".into(),
//                src: vec![Exp::AccessPath("x".into())]
//            }],
//        },
//    ];
//    let edges: Vec<(BasicBlockIdx, BasicBlockIdx)> = vec![
//        (0.into(), 1.into()),
//        (0.into(), 2.into()),
//        (1.into(), 2.into()),
//        (2.into(), 1.into()),
//        (2.into(), 3.into()),
//    ];
//    let blocks = BasicBlocks::new(basic_blocks, edges);
//    let function = FunctionData::new("test", Params::new([]), blocks);
//    let df = SsaDataFlow::new(&function);
//    // Minimal SSA has no phis
//    assert!(df.phis.len() > 0);
//    log::trace!("irreducible df: {:#?}", df);
//}
