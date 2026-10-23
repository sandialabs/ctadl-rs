use super::*;

use smallvec::smallvec;

use crate::index::idx::Idx;
use crate::mir::call::CallEdges;
use crate::ssa::transform;

/// A function under construction: interns locals into its own table.
struct F(FunctionData);

impl F {
    fn new() -> Self {
        F(FunctionData::default())
    }
    fn local(&mut self, name: &str) -> VariableRef {
        VariableRef::new_local_idx(self.0.intern_local(name))
    }
    fn st(&mut self, k: i64) -> AccessPath {
        let st = self.local(STACK_TOP);
        AccessPath::new(st, [OffsetAccess::Offset(Offset(k))])
    }
    /// `store __stack_top.[k].deref := v`
    fn store(&mut self, k: i64, v: &str) -> Statement {
        let dest = self.st(k);
        let v = self.local(v);
        Statement::new_kind(StatementKind::store(
            dest,
            FieldRef::symbol(DEREF),
            Exp::Variable(v),
        ))
    }
    /// `d = load __stack_top.[k].deref`
    fn load(&mut self, d: &str, k: i64) -> Statement {
        let source = self.st(k);
        let d = self.local(d);
        Statement::new_kind(StatementKind::load(d, source, FieldRef::symbol(DEREF)))
    }
    /// `f(__stack_top.[k])`: the address of slot `k` escapes into a call.
    fn escape_call(&mut self, k: i64) -> Statement {
        let arg = Exp::AccessPath(self.st(k));
        Statement::new_kind(StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(thin_vec::thin_vec!["f".to_string()]),
            },
            rets: Default::default(),
            args: thin_vec::thin_vec![arg],
        })
    }
    fn one_block(mut self, stmts: Vec<Statement>) -> FunctionData {
        let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        })));
        for s in stmts {
            block.statements.push_back(s);
        }
        self.0.blocks.push(block);
        self.0
    }
}

fn stmts(f: &FunctionData) -> Vec<String> {
    f.blocks
        .iter_enumerated()
        .flat_map(|(_, b)| b.iter().map(|s| s.kind.to_string()).collect::<Vec<_>>())
        .collect()
}

fn has_local(f: &FunctionData, name: &str) -> bool {
    f.locals.iter_enumerated().any(|(_, d)| d.name == name)
}

/// Loads left reading the frame, as `(dest, slot)`.
fn frame_loads(f: &FunctionData) -> Vec<i64> {
    f.blocks
        .iter_enumerated()
        .flat_map(|(_, b)| b.iter())
        .filter_map(|s| match &s.kind {
            StatementKind::Load { source, .. } => Some(slot(source)),
            _ => None,
        })
        .collect()
}

#[test]
fn straight_line_store_load_becomes_copies() {
    let mut f = F::new();
    let s = f.store(-16, "a");
    let l = f.load("b", -16);
    let mut f = f.one_block(vec![s, l]);
    let stats = promote_stack_slots_function(&mut f, EscapePolicy::AllAbove);
    assert_eq!(stats.promoted_accesses, 2);
    assert_eq!(stats.promoted_slots, 1);
    assert!(frame_loads(&f).is_empty());
    assert!(has_local(&f, "__stack_m16"));
}

#[test]
fn distinct_offsets_are_distinct_slots() {
    let mut f = F::new();
    let s1 = f.store(-16, "a");
    let s2 = f.store(-15, "b");
    let l = f.load("c", -16);
    let mut f = f.one_block(vec![s1, s2, l]);
    let stats = promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    assert_eq!(stats.promoted_slots, 2);
    // `c` reads `-16`'s local, not `-15`'s.
    assert!(has_local(&f, "__stack_m16"));
    assert!(has_local(&f, "__stack_m15"));
}

#[test]
fn policies_bound_what_an_escape_reaches() {
    let build = || {
        let mut f = F::new();
        let e = f.escape_call(-32);
        let mut v = vec![e];
        for k in [-48, -32, -24, -16, -8] {
            v.push(f.store(k, "a"));
            v.push(f.load("b", k));
        }
        f.one_block(v)
    };
    let kept = |p| {
        let mut f = build();
        promote_stack_slots_function(&mut f, p);
        frame_loads(&f)
    };
    assert_eq!(kept(EscapePolicy::Exact), vec![-32]);
    assert_eq!(kept(EscapePolicy::Window(16)), vec![-32, -24]);
    assert_eq!(kept(EscapePolicy::AllAbove), vec![-32, -24, -16, -8]);
}

#[test]
fn kept_slot_keeps_its_stores() {
    let mut f = F::new();
    let e = f.escape_call(-8);
    let s = f.store(-8, "a");
    let mut f = f.one_block(vec![e, s]);
    let stats = promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    assert_eq!(stats.kept_accesses, 1);
    assert_eq!(stats.promoted_accesses, 0);
    assert!(matches!(
        f.blocks[BasicBlockIdx::START_BLOCK].statements[StatementIdx::new(1)].kind,
        StatementKind::Store { .. }
    ));
}

#[test]
fn non_deref_load_is_an_escape() {
    let mut f = F::new();
    let src = f.st(-8);
    let d = f.local("d");
    let odd = Statement::new_kind(StatementKind::load(d, src, FieldRef::symbol("f")));
    let s = f.store(-8, "a");
    let mut f = f.one_block(vec![odd, s]);
    let stats = promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    assert_eq!(stats.promoted_slots, 0);
    assert_eq!(stats.kept_accesses, 1);
}

#[test]
fn assigned_frame_base_skips_function() {
    let mut f = F::new();
    let st = f.local(STACK_TOP);
    let x = f.local("x");
    let redef = Statement::new_kind(StatementKind::assign(st, [Exp::Variable(x)]));
    let s = f.store(-8, "a");
    let mut f = f.one_block(vec![redef, s]);
    let before = stmts(&f);
    let stats = promote_stack_slots_function(&mut f, EscapePolicy::AllAbove);
    assert_eq!(stats.skipped_functions, 1);
    assert_eq!(stmts(&f), before);
}

#[test]
fn function_without_frame_is_unchanged() {
    let mut f = F::new();
    let a = f.local("a");
    let b = f.local("b");
    let s = Statement::new_kind(StatementKind::assign(a, [Exp::Variable(b)]));
    let mut f = f.one_block(vec![s]);
    let before = stmts(&f);
    assert_eq!(
        promote_stack_slots_function(&mut f, EscapePolicy::Exact),
        Mem2RegStats::default()
    );
    assert_eq!(stmts(&f), before);
}

#[test]
fn second_run_changes_nothing() {
    let mut f = F::new();
    let e = f.escape_call(-32);
    let s1 = f.store(-16, "a");
    let s2 = f.store(-32, "a");
    let l = f.load("b", -16);
    let mut f = f.one_block(vec![e, s1, s2, l]);
    promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    let once = stmts(&f);
    let again = promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    assert_eq!(again.promoted_accesses, 0);
    assert_eq!(stmts(&f), once);
}

/// ```text
/// bb0: goto bb1, bb2
/// bb1: store st.[-8].deref := a; goto bb3
/// bb2: store st.[-8].deref := b; goto bb3
/// bb3: c = load st.[-8].deref; return c
/// ```
///
/// After promotion and SSA, `c` reads a phi of the two stores, and `a` never reaches bb2's path.
#[test]
fn diamond_gets_a_phi_after_ssa() {
    let mut f = F::new();
    let s1 = f.store(-8, "a");
    let s2 = f.store(-8, "b");
    let l = f.load("c", -8);
    let c = f.local("c");
    let mut f = f.0;
    f.set_name("diamond".to_string());
    f.set_return_type(ReturnType { arity: 1 });
    let goto = |ts: &[usize]| {
        Some(Terminator::new_kind(TerminatorKind::Goto {
            targets: ts.iter().map(|&t| BasicBlockIdx::new(t)).collect(),
        }))
    };
    f.blocks.push(BasicBlockData::new(goto(&[1, 2])));
    let mut b1 = BasicBlockData::new(goto(&[3]));
    b1.statements.push_back(s1);
    f.blocks.push(b1);
    let mut b2 = BasicBlockData::new(goto(&[3]));
    b2.statements.push_back(s2);
    f.blocks.push(b2);
    let mut b3 = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![Exp::Variable(c)],
    })));
    b3.statements.push_back(l);
    f.blocks.push(b3);

    let stats = promote_stack_slots_function(&mut f, EscapePolicy::Exact);
    assert_eq!(stats.promoted_accesses, 3);
    transform(&mut f, true);
    let phis: Vec<_> = f.blocks[BasicBlockIdx::new(3)]
        .iter()
        .filter(|s| matches!(s.kind, StatementKind::Phi { .. }))
        .collect();
    assert_eq!(phis.len(), 1, "{f}");
    assert!(frame_loads(&f).is_empty());
}

#[test]
fn parse_policies() {
    assert_eq!(EscapePolicy::parse("exact"), Some(EscapePolicy::Exact));
    assert_eq!(EscapePolicy::parse("above"), Some(EscapePolicy::AllAbove));
    assert_eq!(
        EscapePolicy::parse("window:16"),
        Some(EscapePolicy::Window(16))
    );
    assert_eq!(EscapePolicy::parse("window:0"), None);
    assert_eq!(EscapePolicy::parse("bogus"), None);
}

/// Full promotion, at any size.
fn full() -> Mem2Reg {
    Mem2Reg {
        escape: EscapePolicy::Window(8),
        full_above: Some(0),
    }
}

/// `x = __stack_top.[k]`: the address of slot `k` is copied.
fn copy_addr(f: &mut F, x: &str, k: i64) -> Statement {
    let x = f.local(x);
    let a = Exp::AccessPath(f.st(k));
    Statement::new_kind(StatementKind::assign(x, [a]))
}

/// `transfer(&x.b); sink(x.b)`, which is `example.c`'s shape: the callee writes the slot and
/// the function reads it back. Full promotion reloads the slot after the call, and loses
/// nothing it needs to say.
#[test]
fn full_reloads_after_a_call_that_reaches_the_slot() {
    let mut f = F::new();
    let e = f.escape_call(-32);
    let l = f.load("b", -28);
    let mut f = f.one_block(vec![e, l]);
    let (stats, note) = promote_function(&mut f, full(), None);
    assert_eq!(stats.full_functions, 1);
    assert_eq!(stats.escaped_slots, 1);
    assert_eq!(stats.reloads, 1);
    assert_eq!(note, None);
    assert_eq!(frame_loads(&f), vec![-28], "{f}");
    // `%L2` is `__stack_m28`.
    assert_eq!(
        stmts(&f),
        vec![
            "direct-call f(%L0.[-32])",
            "%L2 = load %L0.[-28].deref",
            "assign %L1 = %L2"
        ]
    );
}

#[test]
fn full_store_writes_only_the_local() {
    let mut f = F::new();
    let s = f.store(-32, "a");
    let e = f.escape_call(-32);
    let l = f.load("b", -32);
    let mut f = f.one_block(vec![s, e, l]);
    let (stats, note) = promote_function(&mut f, full(), None);
    assert_eq!(stats.promoted_accesses, 2);
    assert_eq!(stats.kept_accesses, 0);
    assert!(
        !stmts(&f).iter().any(|s| s.starts_with("store")),
        "{:?}",
        stmts(&f)
    );
    // The callee no longer sees `a`.
    let note = note.unwrap();
    assert_eq!(
        note.kinds.keys().copied().collect::<Vec<_>>(),
        vec![EscapeKind::CallInput]
    );
    assert_eq!(
        note.message(),
        "mem2reg full: : frame address escapes (call input); flows through those slots may be \
         missed"
    );
}

#[test]
fn full_skips_calls_that_do_not_reach_the_slot() {
    let mut f = F::new();
    let e = f.escape_call(-64);
    let s = f.store(-32, "a");
    let e2 = f.escape_call(-32);
    let l = f.load("b", -32);
    let mut f = f.one_block(vec![e, s, e2, l]);
    let (stats, _) = promote_function(&mut f, full(), None);
    assert_eq!(stats.reloads, 1);
}

#[test]
fn full_notes_copied_and_stored_addresses() {
    let mut f = F::new();
    let c = copy_addr(&mut f, "p", -16);
    let addr = Exp::AccessPath(f.st(-8));
    let g = f.local("g");
    let st = Statement::new_kind(StatementKind::store(
        AccessPath::without_fields(g),
        FieldRef::symbol(DEREF),
        addr,
    ));
    let l1 = f.load("a", -16);
    let l2 = f.load("b", -8);
    let mut f = f.one_block(vec![c, st, l1, l2]);
    let (stats, note) = promote_function(&mut f, full(), None);
    assert_eq!(stats.reloads, 0);
    let note = note.unwrap();
    assert_eq!(
        note.kinds,
        BTreeMap::from([(EscapeKind::Copied, 1), (EscapeKind::Stored, 1)])
    );
    assert!(note.message().contains("(copied, stored)"));
}

#[test]
fn full_takes_functions_above_the_threshold() {
    let build = || {
        let mut f = F::new();
        let e = f.escape_call(-32);
        let s = f.store(-32, "a");
        let l = f.load("b", -32);
        f.one_block(vec![e, s, l])
    };
    let run = |above, mode| {
        let mut f = build();
        let config = Mem2Reg {
            escape: EscapePolicy::Window(8),
            full_above: above,
        };
        promote_function(&mut f, config, mode).0.full_functions
    };
    assert_eq!(run(Some(1), None), 1);
    assert_eq!(run(Some(2), None), 0);
    assert_eq!(run(None, None), 0);
    assert_eq!(run(None, Some(FullMode::Force)), 1);
    assert_eq!(run(Some(0), Some(FullMode::Forbid)), 0);
}

#[test]
fn full_second_run_changes_nothing() {
    let mut f = F::new();
    let s = f.store(-32, "a");
    let e = f.escape_call(-32);
    let l = f.load("b", -32);
    let mut f = f.one_block(vec![s, e, l]);
    promote_function(&mut f, full(), None);
    let once = stmts(&f);
    let (again, _) = promote_function(&mut f, full(), None);
    assert_eq!(again.promoted_accesses, 0);
    assert_eq!(again.reloads, 0);
    assert_eq!(stmts(&f), once);
}

#[test]
fn parse_configs() {
    assert_eq!(
        Mem2Reg::parse("full"),
        Some(Mem2Reg {
            escape: EscapePolicy::Window(8),
            full_above: Some(DEFAULT_FULL_THRESHOLD),
        })
    );
    assert_eq!(Mem2Reg::parse("full:0"), Some(full()));
    assert_eq!(
        Mem2Reg::parse("window:4"),
        Some(EscapePolicy::Window(4).into())
    );
    assert_eq!(Mem2Reg::parse("full:x"), None);
    assert_eq!(full().tag(), "w8,full>0");
}

/// `keep` in `nightly/tests/jni/JniWide.c`: the pcode frontend returns the value in slot 4 as
/// the slot's address, so the slot stays in memory even under full promotion.
#[test]
fn full_keeps_a_returned_slot() {
    let mut f = F::new();
    let s = f.store(4, "a");
    let ret = Exp::AccessPath(f.st(4));
    let mut f = f.0;
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![ret],
    })));
    block.statements.push_back(s);
    f.blocks.push(block);
    let (stats, note) = promote_function(&mut f, full(), None);
    assert_eq!(stats.promoted_slots, 0);
    assert_eq!(stats.kept_accesses, 1);
    assert_eq!(note, None);
}
