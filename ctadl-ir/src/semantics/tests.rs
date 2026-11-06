//! Unit tests of the reference evaluator over hand-built IR. The rule-by-rule corpus is in
//! `frontends/ctadl-flowy/tests/semantics`, where programs are easier to read.

use thin_vec::thin_vec;

use super::*;
use crate::mir::{
    AccessPath, BasicBlockData, ParameterIdx, ParameterType, ReturnType, Statement, Terminator,
};

/// A program builder: one block per function, statements given as kinds.
struct P {
    program: Program,
}

impl P {
    fn new() -> Self {
        P {
            program: Program::default(),
        }
    }

    fn function(
        &mut self,
        name: &str,
        params: usize,
        arity: u8,
        body: impl FnOnce(&mut FunctionData) -> (Vec<StatementKind>, Vec<Exp>),
    ) {
        let idx = self.program.new_function();
        let f = &mut self.program[idx];
        f.set_name(name.to_string());
        for _ in 0..params {
            f.params.push(ParameterType::SharedFields);
        }
        f.return_type = ReturnType { arity };
        let (stmts, ret) = body(f);
        let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
            args: ret.into_iter().collect(),
        })));
        for s in stmts {
            block.statements.push_back(Statement::new_kind(s));
        }
        f.blocks.blocks_mut().push(block);
    }

    fn eval(&self) -> Outcome {
        let ssa = ssa_form(&self.program);
        ssa.verify().expect("verifies");
        evaluate(&ssa, &ports(), &Config::default())
    }
}

fn ports() -> Ports {
    Ports {
        sources: ["source".to_string()].into_iter().collect(),
        sinks: ["sink".to_string()].into_iter().collect(),
        ..Ports::default()
    }
}

fn local(f: &mut FunctionData, name: &str) -> VariableRef {
    VariableRef::new_local_idx(f.intern_local(name))
}

fn call(name: &str, rets: Vec<VariableRef>, args: Vec<Exp>) -> StatementKind {
    StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(thin_vec![name.to_string()]),
        },
        rets: rets.into_iter().collect(),
        args: args.into_iter().collect(),
    }
}

fn src(r: &VariableRef) -> StatementKind {
    call("source", vec![r.clone()], vec![Exp::new_str("S")])
}

fn sink(e: Exp) -> StatementKind {
    call("sink", vec![], vec![e, Exp::new_str("S")])
}

fn ap(v: &VariableRef, segs: &[PathSegment]) -> AccessPath {
    AccessPath::new(v.clone(), segs.iter().cloned())
}

fn f(name: &str) -> PathSegment {
    PathSegment::field(name)
}

fn reached(o: &Outcome) -> usize {
    o.observed.iter().filter(|o| o.label == "S").count()
}

/// `l = y; l.f = x` reaches `y.f` (S8): a write through a copy of a pointer reaches the
/// original.
#[test]
fn store_through_a_copy_reaches_the_original() {
    let mut p = P::new();
    p.function("F", 1, 0, |fd| {
        let x = local(fd, "x");
        let y = VariableRef::new_parameter(ParameterIdx::new(0));
        let l = local(fd, "l");
        let r = local(fd, "r");
        (
            vec![
                src(&x),
                StatementKind::assign(l.clone(), [Exp::variable(y.clone())]),
                StatementKind::store(l.clone(), f("f"), Exp::variable(x)),
                StatementKind::assign(r.clone(), [Exp::path(ap(&y, &[f("f")]))]),
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    assert_eq!(reached(&p.eval()), 1);
}

/// A label copied forward is `Fixed` and does not move back: `y = x1; y = x2` in SSA is two
/// variables, and a phi does not merge its sources.
#[test]
fn a_copied_label_does_not_move_back() {
    let mut p = P::new();
    p.function("F", 0, 0, |fd| {
        let a = local(fd, "a");
        let b = local(fd, "b");
        let y = local(fd, "y");
        (
            vec![
                src(&a),
                // `y = a, b`: two copies into one variable.
                StatementKind::assign(y.clone(), [Exp::variable(a), Exp::variable(b.clone())]),
                sink(Exp::variable(b)),
            ],
            vec![],
        )
    });
    assert_eq!(reached(&p.eval()), 0);
}

/// The suffix `#value` moves forward only: `t = v` does not give `v.#value` what `t.#value`
/// gets afterwards, while a value held in memory, `p.f.#value`, moves back like its cell.
#[test]
fn value_suffix_is_forward_only_at_the_root() {
    let mut p = P::new();
    p.function("F", 1, 0, |fd| {
        let x = local(fd, "x");
        let v = VariableRef::new_parameter(ParameterIdx::new(0));
        let t = local(fd, "t");
        let w = local(fd, "w");
        (
            vec![
                src(&x),
                StatementKind::assign(t.clone(), [Exp::variable(v.clone())]),
                StatementKind::store(t.clone(), PathSegment::Value, Exp::variable(x.clone())),
                StatementKind::store(t.clone(), f("f"), Exp::value(x)),
                // `v.#value` is not reached; `v.f.#value` is.
                StatementKind::assign(w.clone(), [Exp::path(ap(&v, &[f("f")]))]),
                sink(Exp::variable(w)),
                sink(Exp::variable(v)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    // Only the first sink, the read of `v.f`, sees `S`.
    assert_eq!(o.observed.len(), 1, "{o:?}");
}

/// `r1 = id(a); r2 = id(b)` gives `r2` only `b` (S10): each call is its own instance.
#[test]
fn calls_are_instances() {
    let mut p = P::new();
    p.function("id", 1, 1, |_| {
        (
            vec![],
            vec![Exp::variable(VariableRef::new_parameter(
                ParameterIdx::new(0),
            ))],
        )
    });
    p.function("F", 0, 0, |fd| {
        let a = local(fd, "a");
        let b = local(fd, "b");
        let r1 = local(fd, "r1");
        let r2 = local(fd, "r2");
        (
            vec![
                src(&a),
                call("id", vec![r1.clone()], vec![Exp::variable(a)]),
                call("id", vec![r2.clone()], vec![Exp::variable(b)]),
                sink(Exp::variable(r2)),
                sink(Exp::variable(r1)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    assert_eq!(reached(&o), 1, "{o:?}");
}

/// A global written in a callee reaches the caller (S13), and a call with no body is reported.
#[test]
fn globals_are_shared_and_bodyless_calls_reported() {
    let mut p = P::new();
    p.function("set", 1, 0, |_| {
        (
            vec![StatementKind::store(
                VariableRef::new_global(),
                f("g"),
                Exp::variable(VariableRef::new_parameter(ParameterIdx::new(0))),
            )],
            vec![],
        )
    });
    p.function("F", 0, 0, |fd| {
        let a = local(fd, "a");
        let r = local(fd, "r");
        (
            vec![
                src(&a),
                call("set", vec![], vec![Exp::variable(a)]),
                call("nobody", vec![], vec![]),
                StatementKind::assign(
                    r.clone(),
                    [Exp::path(ap(&VariableRef::new_global(), &[f("g")]))],
                ),
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    assert_eq!(reached(&o), 1, "{o:?}");
    assert!(
        o.unresolved
            .iter()
            .any(|u| u.contains("no body for nobody")),
        "{o:?}"
    );
}

/// An indirect call resolves through a target label stored in a field (S11).
#[test]
fn function_pointer_through_a_field() {
    let mut p = P::new();
    p.function("id", 1, 1, |_| {
        (
            vec![],
            vec![Exp::variable(VariableRef::new_parameter(
                ParameterIdx::new(0),
            ))],
        )
    });
    p.function("F", 0, 0, |fd| {
        let a = local(fd, "a");
        let o = local(fd, "o");
        let r = local(fd, "r");
        (
            vec![
                src(&a),
                StatementKind::store(
                    o.clone(),
                    f("op"),
                    Exp::ObjectRef(CallObject::FunctionPtr("id".into())),
                ),
                StatementKind::CallAssign {
                    style: CallStyle::FuncPtrCall {
                        callee: ap(&o, &[f("op")]),
                        signature: None,
                    },
                    rets: thin_vec![r.clone()],
                    args: thin_vec![Exp::variable(a)],
                },
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    assert_eq!(reached(&o), 1, "{o:?}");
}

/// An address carries its base's labels (S3): `p = q.[8]` with `q` tainted taints `p`.
#[test]
fn an_address_reads_its_base() {
    let mut p = P::new();
    p.function("F", 0, 0, |fd| {
        let q = local(fd, "q");
        let pv = local(fd, "p");
        (
            vec![
                src(&q),
                StatementKind::assign(pv.clone(), [Exp::path(ap(&q, &[PathSegment::offset(8)]))]),
                sink(Exp::variable(pv)),
            ],
            vec![],
        )
    });
    assert_eq!(reached(&p.eval()), 1);
}
