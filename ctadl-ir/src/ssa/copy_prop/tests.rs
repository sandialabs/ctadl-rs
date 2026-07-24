use super::*;

use smallvec::smallvec;

use crate::index::idx::Idx;

thread_local! {
    static LOCALS: std::cell::RefCell<crate::mir::Locals> =
        std::cell::RefCell::new(crate::mir::Locals::default());
}

fn intern(name: &str) -> VariableRef {
    VariableRef::new_local_idx(LOCALS.with(|l| l.borrow_mut().get_or_intern(name)))
}

/// A versioned local `name_version`.
fn v(name: &str, version: u32) -> VariableRef {
    intern(name).with_version(version)
}

/// An *unversioned* local — the shape of the pre-SSA anchor source (`p`, in
/// `p_0 = p`). The pass must never treat these as copies.
fn bare(name: &str) -> VariableRef {
    intern(name)
}

fn read(var: VariableRef) -> Exp {
    Exp::Variable(var)
}

/// A pure copy `dest = src`.
fn copy(dest: VariableRef, src: VariableRef) -> Statement {
    Statement::new_kind(StatementKind::assign(dest, [read(src)]))
}

/// A `dest = phi(operands...)`. The pass reads phis structurally and ignores
/// the predecessor labels, so we hand each operand a distinct dummy block.
fn phi(dest: VariableRef, operands: &[VariableRef]) -> Statement {
    Statement::new_kind(StatementKind::Phi {
        dest,
        operands: operands
            .iter()
            .enumerate()
            .map(|(i, op)| (BasicBlockIdx::new(i), op.clone()))
            .collect(),
    })
}

/// Single block holding `statements`, terminated by `return ret`.
fn one_block(statements: Vec<Statement>, ret: Vec<Exp>) -> FunctionData {
    let mut f = FunctionData::default();
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: ret.into_iter().collect(),
    })));
    for s in statements {
        block.statements.push_back(s);
    }
    f.blocks.push(block);
    f
}

fn kinds(f: &FunctionData, bb: usize) -> Vec<&StatementKind> {
    f.blocks[BasicBlockIdx::new(bb)]
        .statements
        .iter()
        .map(|s| &s.kind)
        .collect()
}

/// The dests defined by block `bb`, in order.
fn dests(f: &FunctionData, bb: usize) -> Vec<VariableRef> {
    f.blocks[BasicBlockIdx::new(bb)]
        .statements
        .iter()
        .flat_map(|s| s.kind.iter_dst_var().cloned().collect::<Vec<_>>())
        .collect()
}

/// The source variables read by the `idx`-th statement of block `bb`.
fn stmt_srcs(f: &FunctionData, bb: usize, idx: usize) -> Vec<VariableRef> {
    f.blocks[BasicBlockIdx::new(bb)]
        .statements
        .iter()
        .nth(idx)
        .expect("statement index out of range")
        .kind
        .iter_src_var()
        .cloned()
        .collect()
}

/// The variables read by block `bb`'s terminator.
fn term_srcs(f: &FunctionData, bb: usize) -> Vec<VariableRef> {
    f.blocks[BasicBlockIdx::new(bb)]
        .terminator()
        .iter_src_var()
        .cloned()
        .collect()
}

#[test]
fn forwards_a_single_copy_and_deletes_it() {
    // x_2 = x_1; return x_2   =>   return x_1, copy gone
    let mut f = one_block(vec![copy(v("x", 2), v("x", 1))], vec![read(v("x", 2))]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn forwards_a_copy_chain_to_its_root() {
    // x_2 = x_1; x_3 = x_2; return x_3   =>   return x_1, both copies gone
    let mut f = one_block(
        vec![copy(v("x", 2), v("x", 1)), copy(v("x", 3), v("x", 2))],
        vec![read(v("x", 3))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 2);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn single_operand_phi_is_trivial() {
    // x_2 = phi(x_1); return x_2   =>   return x_1
    let mut f = one_block(vec![phi(v("x", 2), &[v("x", 1)])], vec![read(v("x", 2))]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn phi_of_identical_operands_is_trivial() {
    // x_3 = phi(x_1, x_1); return x_3   =>   return x_1
    let mut f = one_block(
        vec![phi(v("x", 3), &[v("x", 1), v("x", 1)])],
        vec![read(v("x", 3))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn loop_header_self_referential_phi_is_trivial() {
    // x_3 = phi(x_1, x_3); return x_3   =>   the self operand drops out, x_3 aliases x_1
    let mut f = one_block(
        vec![phi(v("x", 3), &[v("x", 1), v("x", 3)])],
        vec![read(v("x", 3))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn classic_copy_then_trivial_phi_shape() {
    // The module's motivating example:
    //   x_2 = x_1
    //   x_3 = phi(x_2, x_2)
    //   y_1 = x_3
    //   return y_1
    // All three should collapse to a single read of x_1.
    let mut f = one_block(
        vec![
            copy(v("x", 2), v("x", 1)),
            phi(v("x", 3), &[v("x", 2), v("x", 2)]),
            copy(v("y", 1), v("x", 3)),
        ],
        vec![read(v("y", 1))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 3);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("x", 1)]);
}

#[test]
fn resolving_a_copy_exposes_a_trivial_phi() {
    // a_2 = a_1; x_2 = phi(a_2, a_1)
    // Only once a_2 resolves to a_1 do both phi operands agree, making it trivial.
    let mut f = one_block(
        vec![
            copy(v("a", 2), v("a", 1)),
            phi(v("x", 2), &[v("a", 2), v("a", 1)]),
        ],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 2);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 0), vec![v("a", 1)]);
}

#[test]
fn nontrivial_phi_is_kept_but_operands_rewritten() {
    // a_2 = a_1; x_2 = phi(a_2, b_1); return x_2
    // The phi merges two distinct values, so it survives — but its a_2 operand
    // is rewritten to the representative a_1.
    let mut f = one_block(
        vec![
            copy(v("a", 2), v("a", 1)),
            phi(v("x", 2), &[v("a", 2), v("b", 1)]),
        ],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1); // only the copy
    // The surviving statement is the phi; its operands read a_1 and b_1.
    assert_eq!(dests(&f, 0), vec![v("x", 2)]);
    assert_eq!(stmt_srcs(&f, 0, 0), vec![v("a", 1), v("b", 1)]);
    assert_eq!(term_srcs(&f, 0), vec![v("x", 2)]);
}

#[test]
fn unversioned_anchor_copy_is_not_propagated() {
    // p_0 = p   (versioned dest, UNVERSIONED source): the parameter anchor that
    // transform() seeds. It must survive untouched.
    let mut f = one_block(vec![copy(v("p", 0), bare("p"))], vec![read(v("p", 0))]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![v("p", 0)]);
    assert_eq!(term_srcs(&f, 0), vec![v("p", 0)]);
}

#[test]
fn unversioned_dest_copy_is_not_propagated() {
    // t = a_1   (unversioned dest): pre-SSA shape, left alone.
    let mut f = one_block(vec![copy(bare("t"), v("a", 1))], vec![read(bare("t"))]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![bare("t")]);
}

#[test]
fn self_copy_is_not_aliased() {
    // x_1 = x_1: dest == src, so it is never recorded as an alias (which would
    // otherwise make x_1 its own parent). Kept as-is.
    let mut f = one_block(vec![copy(v("x", 1), v("x", 1))], vec![read(v("x", 1))]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![v("x", 1)]);
}

#[test]
fn multi_source_assign_is_not_a_copy() {
    // x_2 = a_1, b_1   (two sources): not a pure copy, so not aliased.
    let mut f = one_block(
        vec![Statement::new_kind(StatementKind::assign(
            v("x", 2),
            [read(v("a", 1)), read(v("b", 1))],
        ))],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![v("x", 2)]);
    assert_eq!(term_srcs(&f, 0), vec![v("x", 2)]);
}

#[test]
fn load_is_not_a_copy() {
    // x_2 = y_1.f   (a Load, not an Assign): never aliased even though it names
    // one variable.
    let mut f = one_block(
        vec![Statement::new_kind(StatementKind::load(
            v("x", 2),
            v("y", 1),
            FieldPath::symbol("f"),
        ))],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![v("x", 2)]);
}

#[test]
fn phi_of_only_self_references_is_left_undefined() {
    // x_2 = phi(x_2, x_2): every operand resolves to the phi itself, so there is
    // no value to alias to. A dead/undefined phi — left in place.
    let mut f = one_block(vec![phi(v("x", 2), &[v("x", 2), v("x", 2)])], vec![]);
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(dests(&f, 0), vec![v("x", 2)]);
}

#[test]
fn copy_cycle_terminates_and_deletes_both() {
    // x_2 = x_1; x_1 = x_2: a two-node cycle with no external anchor. resolve()'s
    // step cap keeps it total; both defs are aliased and removed. The point of
    // this test is that the pass terminates and doesn't panic.
    let mut f = one_block(
        vec![copy(v("x", 2), v("x", 1)), copy(v("x", 1), v("x", 2))],
        vec![],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 2);
    assert!(kinds(&f, 0).is_empty());
}

#[test]
fn rewrites_uses_that_are_not_copies() {
    // x_2 = x_1; y_2 = x_2, x_2   (a multi-source use of the copied value)
    // The copy is deleted and the multi-source assign now reads x_1 twice.
    let mut f = one_block(
        vec![
            copy(v("x", 2), v("x", 1)),
            Statement::new_kind(StatementKind::assign(
                v("y", 2),
                [read(v("x", 2)), read(v("x", 2))],
            )),
        ],
        vec![read(v("y", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert_eq!(dests(&f, 0), vec![v("y", 2)]);
    assert_eq!(stmt_srcs(&f, 0, 0), vec![v("x", 1), v("x", 1)]);
}

/// Two blocks: block 0 (`stmts0`, goto 1) then block 1 (`stmts1`, `return ret`).
fn two_blocks(stmts0: Vec<Statement>, stmts1: Vec<Statement>, ret: Vec<Exp>) -> FunctionData {
    let mut f = FunctionData::default();
    let blocks = f.blocks.blocks_mut();
    let mut b0 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Goto {
        targets: smallvec![BasicBlockIdx::new(1)],
    })));
    for s in stmts0 {
        b0.statements.push_back(s);
    }
    blocks.push(b0);
    let mut b1 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: ret.into_iter().collect(),
    })));
    for s in stmts1 {
        b1.statements.push_back(s);
    }
    blocks.push(b1);
    f
}

#[test]
fn propagates_a_copy_across_blocks() {
    // block0: x_2 = x_1; goto 1
    // block1: return x_2   =>   copy deleted in block0, block1 returns x_1
    let mut f = two_blocks(
        vec![copy(v("x", 2), v("x", 1))],
        vec![],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 1);
    assert!(kinds(&f, 0).is_empty());
    assert_eq!(term_srcs(&f, 1), vec![v("x", 1)]);
    // The CFG is untouched: block0 still gotos block1.
    let TerminatorKind::Goto { targets } = &f.blocks[BasicBlockIdx::ZERO].terminator().kind else {
        panic!("expected goto terminator");
    };
    assert_eq!(targets.as_slice(), &[BasicBlockIdx::new(1)]);
}

#[test]
fn function_with_no_aliases_is_unchanged() {
    // x_2 = a_1, b_1; return x_2   (nothing aliasable) — pass reports 0 and mutates nothing.
    let mut f = one_block(
        vec![Statement::new_kind(StatementKind::assign(
            v("x", 2),
            [read(v("a", 1)), read(v("b", 1))],
        ))],
        vec![read(v("x", 2))],
    );
    let removed = propagate_copies_function(&mut f);
    assert_eq!(removed, 0);
    assert_eq!(kinds(&f, 0).len(), 1);
    assert_eq!(term_srcs(&f, 0), vec![v("x", 2)]);
}

#[test]
fn empty_function_is_a_noop() {
    let mut f = FunctionData::default();
    assert!(f.blocks.is_empty());
    assert_eq!(propagate_copies_function(&mut f), 0);
    assert!(f.blocks.is_empty());
}

#[test]
fn program_entry_point_runs_on_every_function() {
    let mut program = Program::default();
    program.functions.push(one_block(
        vec![copy(v("x", 2), v("x", 1))],
        vec![read(v("x", 2))],
    ));
    program.functions.push(one_block(
        vec![phi(v("y", 2), &[v("y", 1)])],
        vec![read(v("y", 2))],
    ));
    propagate_copies(&mut program);
    assert!(kinds(&program.functions[FunctionIdx::new(0)], 0).is_empty());
    assert_eq!(
        term_srcs(&program.functions[FunctionIdx::new(0)], 0),
        vec![v("x", 1)]
    );
    assert!(kinds(&program.functions[FunctionIdx::new(1)], 0).is_empty());
    assert_eq!(
        term_srcs(&program.functions[FunctionIdx::new(1)], 0),
        vec![v("y", 1)]
    );
}
