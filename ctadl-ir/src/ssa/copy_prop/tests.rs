use super::*;

use std::collections::HashSet;

use smallvec::smallvec;

use crate::index::idx::Idx;
use crate::mir::visit::Visitor;

/// A versioned local, e.g. `v("x", 1)` is `x_1`.
fn v(name: &str, version: u32) -> VariableRef {
    VariableRef::new_local(name.to_string()).with_version(version)
}

fn read(vr: VariableRef) -> Exp {
    Exp::Variable(vr)
}

fn assign(dest: VariableRef, sources: Vec<Exp>) -> Statement {
    Statement::new_kind(StatementKind::Assign {
        dest,
        sources: sources.into_iter().collect(),
    })
}

/// A phi `dest = phi(operands)`; the predecessor block of each operand is
/// irrelevant to this pass, so tests use a dummy index.
fn phi(dest: VariableRef, operands: Vec<VariableRef>) -> Statement {
    Statement::new_kind(StatementKind::Phi {
        dest,
        operands: operands
            .into_iter()
            .map(|o| (BasicBlockIdx::new(0), o))
            .collect(),
    })
}

/// Builds a single-block function whose statements are `statements` and which
/// returns `ret`. The `return` is a genuine use, so the value it names is a
/// consumer the pass rewrites — not itself a copy it would delete.
fn one_block_returning(statements: Vec<Statement>, ret: VariableRef) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_return_type(ReturnType { arity: 1 });
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read(ret)],
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

/// The single return argument of the one-block function.
fn return_arg(f: &FunctionData) -> Exp {
    let TerminatorKind::Return { args } = &f.blocks[BasicBlockIdx::ZERO].terminator().kind else {
        panic!("expected return");
    };
    assert_eq!(args.len(), 1);
    args[0].clone()
}

#[test]
fn propagates_versioned_copy() {
    // x_2 = x_1; return x_2  =>  return x_1
    let mut f = one_block_returning(vec![assign(v("x", 2), vec![read(v("x", 1))])], v("x", 2));
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn collapses_copy_chain() {
    // x_2 = x_1; x_3 = x_2; return x_3  =>  return x_1
    let mut f = one_block_returning(
        vec![
            assign(v("x", 2), vec![read(v("x", 1))]),
            assign(v("x", 3), vec![read(v("x", 2))]),
        ],
        v("x", 3),
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 2);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn eliminates_single_input_phi() {
    // x_2 = phi(x_1); return x_2  =>  return x_1
    let mut f = one_block_returning(vec![phi(v("x", 2), vec![v("x", 1)])], v("x", 2));
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn eliminates_phi_with_identical_operands() {
    // x_3 = phi(x_1, x_1); return x_3  =>  return x_1
    let mut f = one_block_returning(vec![phi(v("x", 3), vec![v("x", 1), v("x", 1)])], v("x", 3));
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn eliminates_loop_phi_with_self_reference() {
    // x_2 = phi(x_1, x_2); return x_2  =>  return x_1
    // The only non-self operand is x_1, so the phi is trivial.
    let mut f = one_block_returning(vec![phi(v("x", 2), vec![v("x", 1), v("x", 2)])], v("x", 2));
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn keeps_nontrivial_phi() {
    // x_3 = phi(x_1, x_2); return x_3  =>  unchanged (two distinct operands)
    let mut f = one_block_returning(vec![phi(v("x", 3), vec![v("x", 1), v("x", 2)])], v("x", 3));
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(block_kinds(&f).len(), 1);
    assert_eq!(return_arg(&f), read(v("x", 3)));
}

#[test]
fn normalizes_prompt_example() {
    // x_2 = x_1; x_3 = phi(x_2, x_2); return x_3  =>  return x_1
    let mut f = one_block_returning(
        vec![
            assign(v("x", 2), vec![read(v("x", 1))]),
            phi(v("x", 3), vec![v("x", 2), v("x", 2)]),
        ],
        v("x", 3),
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 2);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("x", 1)));
}

#[test]
fn phi_of_copies_of_one_value_is_trivial() {
    // a_2 = a_1; a_3 = a_1; z_1 = phi(a_2, a_3); return z_1  =>  return a_1
    // Both operands resolve to a_1, so the phi becomes trivial after the copies
    // are resolved — the fixpoint over both relations is what catches this.
    let mut f = one_block_returning(
        vec![
            assign(v("a", 2), vec![read(v("a", 1))]),
            assign(v("a", 3), vec![read(v("a", 1))]),
            phi(v("z", 1), vec![v("a", 2), v("a", 3)]),
        ],
        v("z", 1),
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 3);
    assert_eq!(block_kinds(&f).len(), 0);
    assert_eq!(return_arg(&f), read(v("a", 1)));
}

#[test]
fn rewrites_operands_of_surviving_phi() {
    // a_2 = a_1; z_1 = phi(a_2, b_1); return z_1
    // The phi has two distinct operands so it survives, but its copied operand
    // a_2 is folded to the representative a_1, and the copy is deleted.
    let mut f = one_block_returning(
        vec![
            assign(v("a", 2), vec![read(v("a", 1))]),
            phi(v("z", 1), vec![v("a", 2), v("b", 1)]),
        ],
        v("z", 1),
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    let kinds = block_kinds(&f);
    assert_eq!(kinds.len(), 1);
    let StatementKind::Phi { dest, operands } = kinds[0] else {
        panic!("expected phi");
    };
    assert_eq!(dest, &v("z", 1));
    let ops: Vec<&VariableRef> = operands.iter().map(|(_, o)| o).collect();
    assert_eq!(ops, vec![&v("a", 1), &v("b", 1)]);
}

#[test]
fn rewrites_call_argument() {
    // x_2 = x_1; f(x_2); return x_1  =>  f(x_1)  (call arg folded, copy deleted)
    let call = Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(thin_vec::thin_vec!["f".to_string()]),
        },
        rets: thin_vec::ThinVec::new(),
        args: thin_vec::thin_vec![read(v("x", 2))],
    });
    let mut f = one_block_returning(
        vec![assign(v("x", 2), vec![read(v("x", 1))]), call],
        v("x", 1),
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    let kinds = block_kinds(&f);
    assert_eq!(kinds.len(), 1);
    let StatementKind::CallAssign { args, .. } = kinds[0] else {
        panic!("expected call");
    };
    assert_eq!(args.as_slice(), &[read(v("x", 1))]);
}

/// Asserts every SSA definition in `f` is unique (the core SSA invariant the
/// pass must preserve) and that the function still verifies.
fn assert_valid_ssa(f: &FunctionData) {
    f.verify().expect("valid IR");
    let mut defs: HashSet<VariableRef> = HashSet::new();
    struct Check<'a>(&'a mut HashSet<VariableRef>);
    impl Visitor for Check<'_> {
        fn visit_statement_kind(&mut self, stmt: &StatementKind, location: Location) {
            self.super_statement_kind(stmt, location);
            for dst in stmt.iter_dst_var() {
                assert!(self.0.insert(dst.clone()), "duplicate def: {dst}");
            }
        }
    }
    Check(&mut defs).visit_function_data(FunctionIdx::new(0), f);
}

/// End-to-end: build a diamond whose join needs a phi, convert it to real SSA
/// via [`crate::ssa::transform`], then run the pass. The result must stay valid
/// SSA, and a second run must be a no-op (the pass reaches a fixpoint).
#[test]
fn integrates_with_real_ssa_and_is_idempotent() {
    // def F(p, q):
    //   b0: x = p; goto b1, b2
    //   b1: x = q; goto b3
    //   b2:        goto b3     (x from b0 reaches here)
    //   b3: y = x; return y
    let mut f = FunctionData::default();
    f.set_name("F".to_string());
    f.set_return_type(ReturnType { arity: 1 });
    f.params.push(ParameterType::ByVal);
    f.params.push(ParameterType::ByVal);
    let p = || Exp::Variable(VariableRef::new_parameter(ParameterIdx::new(0)));
    let q = || Exp::Variable(VariableRef::new_parameter(ParameterIdx::new(1)));
    let x = || VariableRef::new_local("x".to_string());
    let y = || VariableRef::new_local("y".to_string());
    let blocks = f.blocks.blocks_mut();

    let mut b0 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Goto {
        targets: smallvec![BasicBlockIdx::new(1), BasicBlockIdx::new(2)],
    })));
    b0.statements.push_back(assign(x(), vec![p()]));
    blocks.push(b0);

    let mut b1 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Goto {
        targets: smallvec![BasicBlockIdx::new(3)],
    })));
    b1.statements.push_back(assign(x(), vec![q()]));
    blocks.push(b1);

    blocks.push(BasicBlockData::new(Some(Terminator::new_kind(
        TerminatorKind::Goto {
            targets: smallvec![BasicBlockIdx::new(3)],
        },
    ))));

    let mut b3 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![Exp::Variable(y())],
    })));
    b3.statements
        .push_back(assign(y(), vec![Exp::Variable(x())]));
    blocks.push(b3);

    crate::ssa::transform(&mut f, false);
    assert_valid_ssa(&f);

    let removed = propagate_copies_function(&mut f);
    assert!(
        removed > 0,
        "expected the y = x copy (at least) to be folded"
    );
    assert_valid_ssa(&f);

    // Fixpoint: nothing left to fold.
    assert_eq!(propagate_copies_function(&mut f), 0);
    assert_valid_ssa(&f);
}

#[test]
fn leaves_unversioned_anchor_copy() {
    // The version-0 anchor `p_0 = p` (unversioned source) is not a copy here,
    // and a wholly-unversioned copy is left untouched too: the pass is a no-op
    // on anything not fully versioned.
    let p0 = VariableRef::new_parameter(ParameterIdx::new(0)).with_version(0);
    let p = VariableRef::new_parameter(ParameterIdx::new(0));
    let mut f = FunctionData::default();
    f.params.push(ParameterType::ByVal);
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![],
    })));
    block.statements.push_back(assign(p0, vec![read(p)]));
    block.statements.push_back(assign(
        VariableRef::new_local("t".to_string()),
        vec![read(VariableRef::new_local("a".to_string()))],
    ));
    f.blocks.push(block);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(block_kinds(&f).len(), 2);
}
