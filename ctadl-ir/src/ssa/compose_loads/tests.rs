use super::*;

use std::collections::BTreeSet;

use crate::cir::path_syntax::{parse_path, path_to_string};
use crate::index::idx::Idx;

thread_local! {
    static LOCALS: std::cell::RefCell<crate::cir::Locals> =
        std::cell::RefCell::new(crate::cir::Locals::default());
}

fn var(name: &str) -> VariableRef {
    VariableRef::new_local_idx(LOCALS.with(|l| l.borrow_mut().get_or_intern(name)))
}

/// `name` followed by the path `path`, written in the path grammar (`.a.[8].deref`).
fn ap(name: &str, path: &str) -> AccessPath {
    AccessPath::new(var(name), parse_path(path).unwrap())
}

fn assign(dest: AccessPath, sources: Vec<Exp>) -> Statement {
    Statement::new_kind(StatementKind::Assign {
        dest,
        sources: sources.into_iter().collect(),
    })
}

/// `dest = name·path`.
fn copy(dest: &str, name: &str, path: &str) -> Statement {
    assign(ap(dest, ""), vec![Exp::Path(ap(name, path))])
}

fn block(statements: Vec<Statement>, ret: Vec<Exp>) -> BasicBlockData {
    let mut b = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
        args: ret.into_iter().collect(),
    })));
    for s in statements {
        b.statements.push_back(s);
    }
    b
}

fn function(blocks: Vec<BasicBlockData>) -> FunctionData {
    let mut f = FunctionData::default();
    for b in blocks {
        f.blocks.push(b);
    }
    f
}

fn statements(f: &FunctionData, b: usize) -> Vec<StatementKind> {
    f.blocks[BasicBlockIdx::new(b)]
        .statements
        .iter()
        .map(|s| s.kind.clone())
        .collect()
}

fn return_args(f: &FunctionData, b: usize) -> Vec<Exp> {
    match &f.blocks[BasicBlockIdx::new(b)].terminator().kind {
        TerminatorKind::Return { args } => args.to_vec(),
        k => panic!("not a return: {k:?}"),
    }
}

/// A read of the whole temporary or of an address relative to it is not a read through memory,
/// so it stays on the temporary. A store at an address is still composed.
#[test]
fn whole_and_address_reads_stay() {
    let mut f = function(vec![block(
        vec![
            copy("t", "x", ".a"),
            assign(
                ap("y", ""),
                vec![Exp::Path(ap("t", "")), Exp::Path(ap("t", ".[8]"))],
            ),
            assign(ap("t", ".[8]"), vec![Exp::Path(ap("v", ""))]),
        ],
        vec![Exp::Path(ap("t", ""))],
    )]);
    assert_eq!(compose_loads_function(&mut f), 1);
    let s = statements(&f, 0);
    assert_eq!(
        s[1],
        assign(
            ap("y", ""),
            vec![Exp::Path(ap("t", "")), Exp::Path(ap("t", ".[8]"))],
        )
        .kind
    );
    assert_eq!(
        s[2],
        assign(ap("x", ".a.[8]"), vec![Exp::Path(ap("v", ""))]).kind
    );
    assert_eq!(return_args(&f, 0), vec![Exp::Path(ap("t", ""))]);
}

/// Within a block, the map from temporaries is built before anything is rewritten, so a
/// temporary assigned twice stands for its last assignment, even at a read before it.
#[test]
fn last_assignment_wins() {
    let mut f = function(vec![block(
        vec![
            copy("t", "x", ".a"),
            copy("y", "t", ".f"),
            copy("t", "w", ".b"),
        ],
        vec![],
    )]);
    compose_loads_function(&mut f);
    assert_eq!(statements(&f, 0)[1], copy("y", "w", ".b.f").kind);
}

fn mentioned(aps: &[AccessPath]) -> BTreeSet<String> {
    aps.iter()
        .flat_map(mentioned_paths)
        .map(|p| path_to_string(&p))
        .collect()
}

/// The paths of the composed program are the ones the admissible set is seeded with
/// ([`mentioned_paths`]). Composed, a chain of loads mentions the same paths as the access path
/// it spells, as long as no offset sits at a junction of the chain.
#[test]
fn composed_chain_mentions_what_the_path_mentions() {
    let mut f = function(vec![block(
        vec![
            copy("t1", "x", ".[8].deref"),
            copy("t2", "t1", ".a"),
            assign(ap("t2", ".[4].b"), vec![Exp::Path(ap("v", ""))]),
        ],
        vec![],
    )]);
    compose_loads_function(&mut f);
    let mut paths = Vec::new();
    for s in statements(&f, 0) {
        let StatementKind::Assign { dest, sources } = s else {
            unreachable!()
        };
        if !dest.is_pathless() {
            paths.push(dest);
        }
        paths.extend(sources.iter().filter_map(Exp::access_path).cloned());
    }
    assert_eq!(
        mentioned(&paths),
        mentioned(&[ap("x", ".[8].deref.a.[4].b"), ap("v", "")])
    );
    assert_eq!(
        mentioned(&[ap("x", ".[8].deref.a.[4].b")]),
        BTreeSet::from(
            [
                "",
                ".[8]",
                ".[8].deref",
                ".[8].deref.a",
                ".[4]",
                ".[8].deref.a.[4].b"
            ]
            .map(String::from)
        )
    );
}

/// The paths depend on the composition at a junction of offsets: `t = x.[1]; t.[2].deref`
/// mentions the offset `[2]` uncomposed, and `[3]` composed.
#[test]
fn offsets_at_a_junction_depend_on_composition() {
    let chain = [ap("x", ".[1]"), ap("t", ".[2].deref")];
    let mut f = function(vec![block(
        vec![copy("t", "x", ".[1]"), copy("y", "t", ".[2].deref")],
        vec![],
    )]);
    compose_loads_function(&mut f);
    assert_eq!(statements(&f, 0)[1], copy("y", "x", ".[3].deref").kind);
    let composed = [ap("x", ".[1]"), ap("x", ".[3].deref")];
    assert!(mentioned(&chain).contains(".[2]"));
    assert!(!mentioned(&composed).contains(".[2]"));
    assert!(mentioned(&composed).contains(".[3]"));
}
