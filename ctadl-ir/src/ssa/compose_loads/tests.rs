use super::*;

use std::collections::BTreeSet;

use thin_vec::thin_vec;

use crate::index::idx::Idx;
use crate::mir::path_syntax::{parse_path, path_to_string};

thread_local! {
    static LOCALS: std::cell::RefCell<crate::mir::Locals> =
        std::cell::RefCell::new(crate::mir::Locals::default());
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

/// The module docs' example: a store through the end of a chain lands on the root, and each load
/// of the chain reads from the root.
#[test]
fn store_through_chain_lands_on_root() {
    let mut f = function(vec![block(
        vec![
            copy("t1", "x", ".a"),
            copy("t2", "t1", ".b"),
            assign(ap("t2", ".c"), vec![Exp::Path(ap("v", ""))]),
        ],
        vec![],
    )]);
    assert_eq!(compose_loads_function(&mut f), 2);
    assert_eq!(
        statements(&f, 0),
        vec![
            copy("t1", "x", ".a").kind,
            copy("t2", "x", ".a.b").kind,
            assign(ap("x", ".a.b.c"), vec![Exp::Path(ap("v", ""))]).kind,
        ]
    );
}

/// Reads through memory are composed wherever they appear: assignment sources, `value(..)`,
/// call arguments, an indirect call's callee and return values.
#[test]
fn reads_through_memory_are_composed() {
    let call = Statement::new_kind(StatementKind::CallAssign {
        style: CallStyle::FuncPtrCall {
            callee: ap("t", ".fp"),
            signature: None,
        },
        rets: thin_vec![],
        args: thin_vec![Exp::Path(ap("t", ".g"))],
    });
    let mut f = function(vec![block(
        vec![
            copy("t", "x", ".deref"),
            assign(ap("y", ""), vec![Exp::Value(ap("t", ".h"))]),
            call,
        ],
        vec![Exp::Path(ap("t", ".r"))],
    )]);
    assert_eq!(compose_loads_function(&mut f), 4);
    let s = statements(&f, 0);
    assert_eq!(
        s[1],
        assign(ap("y", ""), vec![Exp::Value(ap("x", ".deref.h"))]).kind
    );
    let StatementKind::CallAssign { style, args, .. } = &s[2] else {
        panic!("not a call: {:?}", s[2]);
    };
    assert_eq!(args.to_vec(), vec![Exp::Path(ap("x", ".deref.g"))]);
    let CallStyle::FuncPtrCall { callee, .. } = style else {
        panic!("not an indirect call: {style:?}");
    };
    assert_eq!(*callee, ap("x", ".deref.fp"));
    assert_eq!(return_args(&f, 0), vec![Exp::Path(ap("x", ".deref.r"))]);
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

/// A plain copy carries a capture forward, and an address copy composes with offsets adding,
/// though the address read itself stays where it is. A copy of an uncaptured variable captures
/// nothing.
#[test]
fn copies_carry_captures() {
    let mut f = function(vec![block(
        vec![
            copy("t1", "x", ".deref.[4]"),
            copy("t2", "t1", ""),
            copy("t3", "t2", ".[4]"),
            copy("u", "x", ""),
            assign(ap("y", ""), vec![Exp::Path(ap("t3", ".deref"))]),
            assign(ap("z", ""), vec![Exp::Path(ap("u", ".f"))]),
        ],
        vec![],
    )]);
    compose_loads_function(&mut f);
    let s = statements(&f, 0);
    assert_eq!(s[2], copy("t3", "t2", ".[4]").kind);
    assert_eq!(s[4], copy("y", "x", ".deref.[8].deref").kind);
    assert_eq!(s[5], copy("z", "u", ".f").kind);
}

/// Only a copy with a single access-path source captures; a `value(..)` source or a second
/// source does not. Nothing crosses a block boundary.
#[test]
fn what_does_not_capture() {
    let mut f = function(vec![
        block(
            vec![
                assign(ap("t1", ""), vec![Exp::Value(ap("x", ".a"))]),
                assign(
                    ap("t2", ""),
                    vec![Exp::Path(ap("x", ".a")), Exp::Path(ap("w", ""))],
                ),
                copy("t3", "x", ".a"),
                copy("y1", "t1", ".f"),
                copy("y2", "t2", ".f"),
            ],
            vec![],
        ),
        block(vec![copy("y3", "t3", ".f")], vec![]),
    ]);
    assert_eq!(compose_loads_function(&mut f), 0);
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
