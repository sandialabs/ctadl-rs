use super::*;

use smallvec::smallvec;

fn function(build: impl FnOnce(&mut FunctionData) -> Vec<Statement>) -> FunctionData {
    let mut f = FunctionData::default();
    let stmts = build(&mut f);
    let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![],
    })));
    for s in stmts {
        block.statements.push_back(s);
    }
    f.blocks.push(block);
    f
}

fn local(f: &mut FunctionData, name: &str) -> VariableRef {
    VariableRef::new_local_idx(f.intern_local(name))
}

fn stmts(f: &FunctionData) -> Vec<String> {
    f.blocks
        .iter_enumerated()
        .flat_map(|(_, b)| b.iter().map(|s| s.kind.to_string()).collect::<Vec<_>>())
        .collect()
}

/// `base = update (source.[k].deref := v)`
fn update(f: &mut FunctionData, base: &str, source: &str, k: i64, v: &str) -> Statement {
    let dest = AccessPath::new(local(f, base), [OffsetAccess::Offset(Offset(k))]);
    let source = local(f, source);
    let v = local(f, v);
    Statement::new_kind(StatementKind::update(
        dest,
        source,
        FieldRef::deref(),
        Exp::Variable(v),
    ))
}

#[test]
fn a_frame_update_becomes_the_store_it_stands_for() {
    let mut f = function(|f| vec![update(f, STACK_TOP, STACK_TOP, -16, "v")]);
    let mut store = function(|f| {
        let dest = AccessPath::new(local(f, STACK_TOP), [OffsetAccess::Offset(Offset(-16))]);
        let v = local(f, "v");
        vec![Statement::new_kind(StatementKind::store(
            dest,
            FieldRef::deref(),
            Exp::Variable(v),
        ))]
    });
    assert_eq!(coarsen_frame_updates_function(&mut f), 1);
    assert_eq!(stmts(&f), stmts(&store));
    assert_eq!(coarsen_frame_updates_function(&mut store), 0);
}

#[test]
fn other_updates_are_left_alone() {
    let mut f = function(|f| {
        vec![
            update(f, "s", "s", 0, "v"),
            update(f, STACK_TOP, "s", 0, "v"),
            update(f, "s", STACK_TOP, 0, "v"),
        ]
    });
    let before = stmts(&f);
    assert_eq!(coarsen_frame_updates_function(&mut f), 0);
    assert_eq!(stmts(&f), before);
}
