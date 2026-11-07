//! Unit tests of the reference evaluator over hand-built IR. The rule-by-rule corpus is in
//! `frontends/ctadl-flowy/tests/semantics`, where programs are easier to read.

use thin_vec::thin_vec;

use super::*;
use crate::cir::{
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

    /// Declares the global `name` and returns a reference to it.
    fn global(&mut self, name: &str) -> VariableRef {
        VariableRef::global(self.program.declare_global(name, None))
    }

    fn eval(&self) -> Outcome {
        let ssa = ssa_form(&self.program);
        ssa.verify().expect("verifies");
        evaluate(&ssa, &ports(), &Config::default())
    }

    /// Evaluates this program together with `others`, as the programs of one project.
    fn eval_with(&self, others: &[&P]) -> Outcome {
        let ssa: Vec<Program> = std::iter::once(self)
            .chain(others.iter().copied())
            .map(|p| ssa_form(&p.program))
            .collect();
        for p in &ssa {
            p.verify().expect("verifies");
        }
        let refs: Vec<&Program> = ssa.iter().collect();
        evaluate_programs(&refs, &ports(), &Config::default())
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

/// A global written in a callee reaches the caller (G2, G5: nothing is passed for it), and a
/// call with no body is reported.
#[test]
fn globals_are_shared_and_bodyless_calls_reported() {
    let mut p = P::new();
    let g = p.global("g");
    let g2 = g.clone();
    p.function("set", 1, 0, |_| {
        (
            vec![StatementKind::store(
                g2,
                PathSegment::Deref,
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
                StatementKind::assign(r.clone(), [Exp::path(ap(&g, &[PathSegment::Deref]))]),
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

/// A store to a global in one function is read in another with no call between them (G2): the
/// two are roots of the unfolding with no common caller.
#[test]
fn a_global_needs_no_common_caller() {
    let mut p = P::new();
    let g = p.global("g");
    let g2 = g.clone();
    p.function("put", 0, 0, |fd| {
        let a = local(fd, "a");
        (
            vec![
                src(&a),
                StatementKind::store(g2, PathSegment::Deref, Exp::variable(a)),
            ],
            vec![],
        )
    });
    p.function("get", 0, 0, |fd| {
        let r = local(fd, "r");
        (
            vec![
                StatementKind::assign(r.clone(), [Exp::path(ap(&g, &[PathSegment::Deref]))]),
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    assert_eq!(reached(&o), 1, "{o:?}");
}

/// A function of one program that writes `$g`, and one of another program that reads `$g`:
/// two programs that declare one name declare one variable (G3), and two names are two.
#[test]
fn two_programs_share_a_global_by_name() {
    let put = |name: &str| {
        let mut p = P::new();
        let g = p.global(name);
        p.function("put", 0, 0, |fd| {
            let a = local(fd, "a");
            (
                vec![
                    src(&a),
                    StatementKind::store(g, PathSegment::Deref, Exp::variable(a)),
                ],
                vec![],
            )
        });
        p
    };
    let mut get = P::new();
    // A second global first, so that `g`'s index differs between the two programs.
    get.global("other");
    let g = get.global("g");
    get.function("get", 0, 0, |fd| {
        let r = local(fd, "r");
        (
            vec![
                StatementKind::assign(r.clone(), [Exp::path(ap(&g, &[PathSegment::Deref]))]),
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    let o = get.eval_with(&[&put("g")]);
    assert_eq!(reached(&o), 1, "{o:?}");
    let o = get.eval_with(&[&put("h")]);
    assert_eq!(reached(&o), 0, "{o:?}");
}

/// Nothing is passed for a global at a call (G5): a callee that names `$g` reads the cell its
/// caller stored to, though no argument carries it.
#[test]
fn nothing_is_passed_for_a_global_at_a_call() {
    let mut p = P::new();
    let g = p.global("g");
    let g2 = g.clone();
    p.function("get", 0, 1, |fd| {
        let r = local(fd, "r");
        (
            vec![StatementKind::assign(
                r.clone(),
                [Exp::path(ap(&g2, &[PathSegment::Deref]))],
            )],
            vec![Exp::variable(r)],
        )
    });
    p.function("F", 0, 0, |fd| {
        let a = local(fd, "a");
        let r = local(fd, "r");
        (
            vec![
                src(&a),
                StatementKind::store(g, PathSegment::Deref, Exp::variable(a)),
                call("get", vec![r.clone()], vec![]),
                sink(Exp::variable(r)),
            ],
            vec![],
        )
    });
    let o = p.eval();
    assert_eq!(reached(&o), 1, "{o:?}");
}
