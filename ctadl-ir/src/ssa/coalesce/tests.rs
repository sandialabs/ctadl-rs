use super::*;

use smallvec::smallvec;

fn local(name: &str) -> VariableRef {
    VariableRef::new_local(name.to_string())
}

fn read(name: &str) -> Exp {
    Exp::Variable(local(name))
}

fn assign(dest: &str, sources: Vec<Exp>) -> Statement {
    Statement::new_kind(StatementKind::assign(local(dest), sources))
}

/// Builds a single-block function with the given statements and a
/// no-arg return terminator.
fn one_block_function(statements: Vec<Statement>) -> FunctionData {
    let mut f = FunctionData::default();
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![],
    })));
    for s in statements {
        block.statements.push_back(s);
    }
    f.blocks.push(block);
    f
}

fn block_kinds(f: &FunctionData) -> Vec<&StatementKind> {
    f.blocks[BasicBlockIdx::ZERO]
        .statements
        .iter()
        .map(|s| &s.kind)
        .collect()
}

#[test]
fn fuses_binop_temp_into_whole_var_copy() {
    // t = a, b; x = t  =>  x = a, b
    let mut f = one_block_function(vec![
        assign("t", vec![read("a"), read("b")]),
        assign("x", vec![read("t")]),
    ]);
    coalesce_function(&mut f);
    let kinds = block_kinds(&f);
    assert_eq!(kinds.len(), 1);
    let StatementKind::Assign { dest, sources } = kinds[0] else {
        panic!("expected assign");
    };
    assert_eq!(dest, &local("x"));
    assert_eq!(sources.as_slice(), &[read("a"), read("b")]);
}

#[test]
fn collapses_copy_chains() {
    // t0 = a, b; t1 = t0; x = t1  =>  x = a, b
    let mut f = one_block_function(vec![
        assign("t0", vec![read("a"), read("b")]),
        assign("t1", vec![read("t0")]),
        assign("x", vec![read("t1")]),
    ]);
    coalesce_function(&mut f);
    let kinds = block_kinds(&f);
    assert_eq!(kinds.len(), 1);
    let StatementKind::Assign { dest, sources } = kinds[0] else {
        panic!("expected assign");
    };
    assert_eq!(dest, &local("x"));
    assert_eq!(sources.as_slice(), &[read("a"), read("b")]);
}

#[test]
fn no_fusion_when_temp_used_twice() {
    // t = a; x = t; y = t  =>  unchanged
    let mut f = one_block_function(vec![
        assign("t", vec![read("a")]),
        assign("x", vec![read("t")]),
        assign("y", vec![read("t")]),
    ]);
    coalesce_function(&mut f);
    assert_eq!(block_kinds(&f).len(), 3);
}

#[test]
fn no_fusion_across_write_to_source() {
    // t = a; a = b; x = t  =>  unchanged (a changed between def and use)
    let mut f = one_block_function(vec![
        assign("t", vec![read("a")]),
        assign("a", vec![read("b")]),
        assign("x", vec![read("t")]),
    ]);
    coalesce_function(&mut f);
    assert_eq!(block_kinds(&f).len(), 3);
}

#[test]
fn substitutes_pure_copy_into_call_args() {
    // t = y; f(t)  =>  f(y)
    let mut f = one_block_function(vec![
        assign("t", vec![read("y")]),
        Statement::new_kind(StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(thin_vec::thin_vec!["f".to_string()]),
            },
            rets: thin_vec::ThinVec::new(),
            args: thin_vec::thin_vec![read("t")],
        }),
    ]);
    coalesce_function(&mut f);
    let kinds = block_kinds(&f);
    assert_eq!(kinds.len(), 1);
    let StatementKind::CallAssign { args, .. } = kinds[0] else {
        panic!("expected call");
    };
    assert_eq!(args.as_slice(), &[read("y")]);
}

#[test]
fn no_fusion_when_call_may_write_source() {
    // t = x; g(x); y = t  =>  unchanged (g may write through x)
    let mut f = one_block_function(vec![
        assign("t", vec![read("x")]),
        Statement::new_kind(StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(thin_vec::thin_vec!["g".to_string()]),
            },
            rets: thin_vec::ThinVec::new(),
            args: thin_vec::thin_vec![read("x")],
        }),
        assign("y", vec![read("t")]),
    ]);
    coalesce_function(&mut f);
    assert_eq!(block_kinds(&f).len(), 3);
}

#[test]
fn substitutes_into_return() {
    // t = y; return t  =>  return y
    let mut f = one_block_function(vec![assign("t", vec![read("y")])]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read("t")],
    }));
    coalesce_function(&mut f);
    assert_eq!(block_kinds(&f).len(), 0);
    let term = f.blocks[BasicBlockIdx::ZERO].terminator();
    let TerminatorKind::Return { args } = &term.kind else {
        panic!("expected return");
    };
    assert_eq!(args.as_slice(), &[read("y")]);
}

#[test]
fn versioned_variables_are_untouched() {
    // Already-SSA form must be a no-op.
    let t = local("t").with_version(1);
    let x = local("x").with_version(1);
    let mut f = one_block_function(vec![
        Statement::new_kind(StatementKind::Assign {
            dest: t.clone(),
            sources: smallvec![Exp::Variable(local("a").with_version(0))],
        }),
        Statement::new_kind(StatementKind::Assign {
            dest: x,
            sources: smallvec![Exp::Variable(t)],
        }),
    ]);
    coalesce_function(&mut f);
    assert_eq!(block_kinds(&f).len(), 2);
}
