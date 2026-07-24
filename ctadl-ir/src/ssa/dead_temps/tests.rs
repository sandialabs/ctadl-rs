use super::*;

use smallvec::smallvec;

use crate::mir::call::{CallEdges, CallStyle};

thread_local! {
    static LOCALS: std::cell::RefCell<crate::mir::Locals> =
        std::cell::RefCell::new(crate::mir::Locals::default());
}

/// Interns `name` into a per-thread table so repeated names share one `LocalIdx`.
fn local(name: &str) -> VariableRef {
    VariableRef::new_local_idx(LOCALS.with(|l| l.borrow_mut().get_or_intern(name)))
}

fn param(idx: usize) -> VariableRef {
    VariableRef::new_parameter(ParameterIdx::new(idx))
}

fn read(name: &str) -> Exp {
    Exp::Variable(local(name))
}

fn assign(dest: VariableRef, sources: Vec<Exp>) -> Statement {
    Statement::new_kind(StatementKind::assign(dest, sources))
}

/// `dest = source.field` (a [`StatementKind::Load`]).
fn load(dest: &str, source: &str, field: &str) -> Statement {
    Statement::new_kind(StatementKind::load(
        local(dest),
        local(source),
        FieldPath::symbol(field),
    ))
}

/// `c* = f(args)` — a direct call to `f`.
fn call(rets: Vec<VariableRef>, args: Vec<Exp>) -> Statement {
    Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(thin_vec::thin_vec!["f".to_string()]),
        },
        rets: rets.into_iter().collect(),
        args: args.into_iter().collect(),
    })
}

/// Builds a single-block function with the given statements and a no-arg
/// return terminator.
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

fn block_kinds(f: &FunctionData, bb: usize) -> Vec<&StatementKind> {
    f.blocks[BasicBlockIdx::new(bb)]
        .statements
        .iter()
        .map(|s| &s.kind)
        .collect()
}

/// The dests defined by a block's statements, in order (skipping statements
/// that define no variable, such as stores).
fn dests(f: &FunctionData, bb: usize) -> Vec<VariableRef> {
    f.blocks[BasicBlockIdx::new(bb)]
        .statements
        .iter()
        .flat_map(|s| s.kind.iter_dst_var().cloned().collect::<Vec<_>>())
        .collect()
}

#[test]
fn deletes_unread_assign_temp() {
    // t = a;   (t never read)   =>  deleted
    let mut f = one_block_function(vec![assign(local("t"), vec![read("a")])]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn deletes_unread_multi_source_assign() {
    // t = a, b;   (t never read)   =>  deleted
    let mut f = one_block_function(vec![assign(local("t"), vec![read("a"), read("b")])]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn deletes_unread_load_temp() {
    // t = y.f;   (a Load, t never read)   =>  deleted
    let mut f = one_block_function(vec![load("t", "y", "f")]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn keeps_temp_read_by_terminator() {
    // t = a; return t   =>  kept (the return reads t)
    let mut f = one_block_function(vec![assign(local("t"), vec![read("a")])]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read("t")],
    }));
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 1);
}

/// A bare variable read expressed as a pathless [`Exp::AccessPath`] rather than
/// an [`Exp::Variable`]. Frontends can emit either for the same read.
fn read_access_path(name: &str) -> Exp {
    Exp::AccessPath(AccessPath::without_fields(local(name)))
}

#[test]
fn terminator_iter_src_var_yields_access_path_base() {
    // `return <access-path t>` reads t, even though the operand is an
    // Exp::AccessPath rather than an Exp::Variable.
    let term = TerminatorKind::Return {
        args: smallvec![read_access_path("t")],
    };
    let reads: Vec<VariableRef> = term.iter_src_var().cloned().collect();
    assert_eq!(reads, vec![local("t")]);
}

#[test]
fn keeps_temp_returned_via_access_path() {
    // t = y.f; return <access-path t>
    // The return operand is a pathless Exp::AccessPath, so t is used and its
    // defining Load must survive.
    let mut f = one_block_function(vec![load("t", "y", "f")]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read_access_path("t")],
    }));
    eliminate_dead_temps_function(&mut f);
    assert_eq!(dests(&f, 0), vec![local("t")]);
}

#[test]
fn keeps_used_temp() {
    // t = a; x = t; return x   =>  both kept
    let mut f = one_block_function(vec![
        assign(local("t"), vec![read("a")]),
        assign(local("x"), vec![read("t")]),
    ]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read("x")],
    }));
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 2);
}

#[test]
fn collapses_copy_chain_to_dead_root() {
    // t0 = a; t1 = t0; t2 = t1;   (t2 never read)   =>  all deleted
    let mut f = one_block_function(vec![
        assign(local("t0"), vec![read("a")]),
        assign(local("t1"), vec![read("t0")]),
        assign(local("t2"), vec![read("t1")]),
    ]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn collapses_load_then_copy_chain() {
    // t = y.f; x = t;   (x never read)   =>  both deleted
    let mut f = one_block_function(vec![
        load("t", "y", "f"),
        assign(local("x"), vec![read("t")]),
    ]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn keeps_producer_with_a_remaining_live_use() {
    // t = a; x = t; y = t; return y
    // x is dead, but t still feeds y, so t must survive.
    let mut f = one_block_function(vec![
        assign(local("t"), vec![read("a")]),
        assign(local("x"), vec![read("t")]),
        assign(local("y"), vec![read("t")]),
    ]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read("y")],
    }));
    eliminate_dead_temps_function(&mut f);
    // Only `x = t` is removed; `t = a` and `y = t` remain in order.
    assert_eq!(dests(&f, 0), vec![local("t"), local("y")]);
}

#[test]
fn keeps_call_with_unread_ret() {
    // c = f(a);   (c never read)   =>  kept, because the call may have effects
    let mut f = one_block_function(vec![call(vec![local("c")], vec![read("a")])]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 1);
}

#[test]
fn call_arg_keeps_feeding_temp_alive() {
    // t = a; f(t);   (call result unused, but the call reads t)   =>  both kept
    let mut f = one_block_function(vec![
        assign(local("t"), vec![read("a")]),
        call(vec![], vec![read("t")]),
    ]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 2);
}

#[test]
fn keeps_store() {
    // store %global.f := v;   (a Store defines no variable)   =>  never a candidate
    let mut f = one_block_function(vec![Statement::new_kind(StatementKind::store(
        AccessPath::without_fields(VariableRef::new_global()),
        FieldPath::symbol("f"),
        read("v"),
    ))]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 1);
}

#[test]
fn keeps_unread_non_local_def() {
    // p0 = a;   (dest is a parameter, not a local)   =>  kept
    let mut f = one_block_function(vec![assign(param(0), vec![read("a")])]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 1);
}

#[test]
fn versioned_locals_are_untouched() {
    // Already-SSA form: t_1 = a_0, t_1 unread. A no-op — the pass runs before SSA.
    let mut f = one_block_function(vec![Statement::new_kind(StatementKind::Assign {
        dest: local("t").with_version(1),
        sources: smallvec![Exp::Variable(local("a").with_version(0))],
    })]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 1);
}

#[test]
fn deletes_multiple_dead_defs_of_same_temp() {
    // t = a; t = b;   (t never read)   =>  both defs deleted
    let mut f = one_block_function(vec![
        assign(local("t"), vec![read("a")]),
        assign(local("t"), vec![read("b")]),
    ]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn deletes_dead_temp_preserving_order_of_survivors() {
    // keep = a; dead = b; last = c; return keep, last
    let mut f = one_block_function(vec![
        assign(local("keep"), vec![read("a")]),
        assign(local("dead"), vec![read("b")]),
        assign(local("last"), vec![read("c")]),
    ]);
    f.blocks[BasicBlockIdx::ZERO].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![read("keep"), read("last")],
    }));
    eliminate_dead_temps_function(&mut f);
    assert_eq!(dests(&f, 0), vec![local("keep"), local("last")]);
}

/// Builds a two-block function: block 0 (`stmts0`, goto 1) and block 1
/// (`stmts1`, no-arg return). Used to check cross-block use counting.
fn two_block_function(stmts0: Vec<Statement>, stmts1: Vec<Statement>) -> FunctionData {
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
        args: smallvec![],
    })));
    for s in stmts1 {
        b1.statements.push_back(s);
    }
    blocks.push(b1);
    f
}

#[test]
fn keeps_temp_used_in_another_block() {
    // block0: t = a; goto 1
    // block1: x = t;   (x unread, but that keeps `t = a` from being dead)
    let mut f = two_block_function(
        vec![assign(local("t"), vec![read("a")])],
        vec![assign(local("x"), vec![read("t")])],
    );
    eliminate_dead_temps_function(&mut f);
    // block1's `x = t` is dead and removed, which in turn kills `t = a` in block0.
    assert_eq!(block_kinds(&f, 1).len(), 0);
    assert_eq!(block_kinds(&f, 0).len(), 0);
}

#[test]
fn deletes_dead_temp_across_blocks_preserving_cfg() {
    // block0: dead = a; goto 1
    // block1: (empty) return
    let mut f = two_block_function(vec![assign(local("dead"), vec![read("a")])], vec![]);
    eliminate_dead_temps_function(&mut f);
    assert_eq!(block_kinds(&f, 0).len(), 0);
    // Deletion doesn't touch the CFG: the goto -> block1 edge is intact.
    let TerminatorKind::Goto { targets } = &f.blocks[BasicBlockIdx::ZERO].terminator().kind else {
        panic!("expected goto terminator");
    };
    assert_eq!(targets.as_slice(), &[BasicBlockIdx::new(1)]);
}

#[test]
fn empty_function_is_a_noop() {
    let mut f = FunctionData::default();
    assert!(f.blocks.is_empty());
    eliminate_dead_temps_function(&mut f);
    assert!(f.blocks.is_empty());
}

#[test]
fn program_entry_point_eliminates_in_every_function() {
    let mut program = Program::default();
    program.functions.push(one_block_function(vec![assign(
        local("t"),
        vec![read("a")],
    )]));
    program.functions.push(one_block_function(vec![assign(
        local("u"),
        vec![read("b")],
    )]));
    eliminate_dead_temps(&mut program);
    assert_eq!(
        block_kinds(&program.functions[FunctionIdx::new(0)], 0).len(),
        0
    );
    assert_eq!(
        block_kinds(&program.functions[FunctionIdx::new(1)], 0).len(),
        0
    );
}
