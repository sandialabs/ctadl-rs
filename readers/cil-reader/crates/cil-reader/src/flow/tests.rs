//! Unit tests of the flow types: `map`, the read/write visitors and the edge
//! view. The per-opcode tests are in `opcode_tests`.

use super::*;
use crate::model::{MethodSig, TypeSig};

fn field(rid: u32) -> FieldOp {
    FieldOp {
        token: MetadataToken(0x0400_0000 | rid),
        is_static: false,
        ty: TypeSig::I4,
    }
}

fn method() -> MethodOp {
    MethodOp {
        token: MetadataToken(0x0600_0001),
        sig: void_sig(1),
        generic_args: vec![],
    }
}

fn void_sig(params: usize) -> MethodSig {
    MethodSig {
        has_this: true,
        explicit_this: false,
        calling_convention: 0,
        generic_arity: 0,
        return_type: TypeSig::Void,
        params: vec![TypeSig::I4; params],
    }
}

/// One effect of every kind, each slot distinct, so a mapping that drops,
/// duplicates or reorders slots shows.
fn every_effect() -> Vec<Effect<Slot>> {
    use Slot::*;
    let ty = TypeOp {
        token: MetadataToken(0x0200_0002),
    };
    vec![
        Effect::Assign {
            dst: Out(0),
            srcs: vec![In(0), In(1)],
            transfer: Transfer::Derived(Op::Add(OpFlags::default())),
        },
        Effect::Const {
            dst: Out(0),
            value: Const::I4(3),
        },
        Effect::Load {
            dst: Out(0),
            src: Path {
                root: Root::Slot(In(0)),
                field: Field::Element {
                    indices: vec![In(1), In(2)],
                },
            },
        },
        Effect::Store {
            dst: Path {
                root: Root::Slot(In(0)),
                field: Field::Block { size: In(2) },
            },
            src: In(1),
        },
        Effect::Store {
            dst: Path {
                root: Root::Static,
                field: Field::Named(field(1)),
            },
            src: In(0),
        },
        Effect::AddressOf {
            dst: Out(0),
            place: Place::Element(In(0), vec![In(1)]),
        },
        Effect::New {
            dst: Out(0),
            kind: NewKind::Array,
            ty: Some(ty),
            sizes: vec![In(0)],
        },
        Effect::FunctionPtr {
            dst: Out(0),
            method: method(),
            receiver: Some(In(0)),
        },
        Effect::Clear { addr: In(0) },
        Effect::Call(CallSite {
            kind: CallKind::Indirect { callee: In(2) },
            method: None,
            sig: void_sig(1),
            receiver: Some(In(0)),
            args: vec![In(1)],
            result: None,
            thrown: Thrown,
            byref_args: vec![],
            prefixes: Prefixes::default(),
        }),
        Effect::Return { src: Some(In(0)) },
        Effect::Throw { src: Caught },
        Effect::Jump { method: method() },
        Effect::Test {
            srcs: vec![In(0), In(1)],
        },
        Effect::Opaque {
            srcs: vec![In(0)],
            dsts: vec![Out(0), Temp(0)],
            reason: OpaqueReason::UndefinedOpcode,
        },
    ]
}

fn reads<S: Clone>(e: &Effect<S>) -> Vec<S> {
    let mut v = Vec::new();
    e.for_each_read(|s| v.push(s.clone()));
    v
}

fn writes<S: Clone>(e: &Effect<S>) -> Vec<S> {
    let mut v = Vec::new();
    e.for_each_write(|s| v.push(s.clone()));
    v
}

#[test]
fn identity_map_keeps_every_effect() {
    for e in every_effect() {
        assert_eq!(e.clone().map(|s| s), e);
    }
}

#[test]
fn map_visits_reads_then_writes_in_operand_order() {
    // Numbering the slots in the order map visits them gives the reads
    // first, in for_each_read order, then the writes.
    for e in every_effect() {
        let mut seen = Vec::new();
        let mapped = e.clone().map(|s| {
            seen.push(s);
            seen.len() - 1
        });
        let mut expected = reads(&e);
        expected.extend(writes(&e));
        assert_eq!(seen, expected, "{e}");
        // And the mapped effect has the numbers where the slots were.
        let back = mapped.map(|n| seen[n]);
        assert_eq!(back, e);
    }
}

#[test]
fn map_changes_the_slot_type() {
    let e = Effect::Assign {
        dst: Slot::Out(0),
        srcs: vec![Slot::In(0)],
        transfer: Transfer::Copy,
    };
    let bound: Effect<String> = e.map(|s| format!("v{s}"));
    assert_eq!(
        bound,
        Effect::Assign {
            dst: "vo0".to_string(),
            srcs: vec!["vi0".to_string()],
            transfer: Transfer::Copy,
        }
    );
}

#[test]
fn reads_and_writes() {
    use Slot::*;
    let effects = every_effect();
    let rw: Vec<(Vec<Slot>, Vec<Slot>)> = effects.iter().map(|e| (reads(e), writes(e))).collect();
    assert_eq!(
        rw,
        vec![
            (vec![In(0), In(1)], vec![Out(0)]),
            (vec![], vec![Out(0)]),
            (vec![In(0), In(1), In(2)], vec![Out(0)]),
            (vec![In(0), In(2), In(1)], vec![]),
            (vec![In(0)], vec![]),
            (vec![In(0), In(1)], vec![Out(0)]),
            (vec![In(0)], vec![Out(0)]),
            (vec![In(0)], vec![Out(0)]),
            (vec![In(0)], vec![]),
            (vec![In(0), In(1), In(2)], vec![Thrown]),
            (vec![In(0)], vec![]),
            (vec![Caught], vec![]),
            (vec![], vec![]),
            (vec![In(0), In(1)], vec![]),
            (vec![In(0)], vec![Out(0), Temp(0)]),
        ]
    );
}

#[test]
fn edges_per_effect() {
    let effects = every_effect();
    let edges: Vec<Vec<String>> = effects
        .iter()
        .map(|e| e.edges().iter().map(|e| e.to_string()).collect())
        .collect();
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        edges,
        vec![
            s(&["i0 -> o0 Derived(add)", "i1 -> o0 Derived(add)"]),
            s(&[]),
            // Element indices give no edges of their own.
            s(&["i0[i1, i2] -> o0 Copy"]),
            s(&["i1 -> i0.*[..i2] Copy"]),
            s(&["i0 -> static.<04000001> Copy"]),
            s(&["&i0[i1] -> o0 Copy"]),
            s(&["i0 -> o0.length Copy"]),
            s(&[]),
            s(&[]),
            s(&[]),
            s(&[]),
            s(&[]),
            s(&[]),
            s(&[]),
            s(&["i0 -> o0 Derived(opaque)", "i0 -> t0 Derived(opaque)"]),
        ]
    );
}

#[test]
fn several_sizes_multiply_into_the_length() {
    let e = Effect::New {
        dst: Slot::Out(0),
        kind: NewKind::Array,
        ty: None,
        sizes: vec![Slot::In(0), Slot::In(1)],
    };
    let edges: Vec<String> = e.edges().iter().map(|e| e.to_string()).collect();
    assert_eq!(
        edges,
        [
            "i0 -> o0.length Derived(mul)",
            "i1 -> o0.length Derived(mul)"
        ]
    );
}

#[test]
fn control_targets_and_fall_through() {
    assert!(Control::Next.falls_through());
    assert!(Control::Cond { taken: vec![4] }.falls_through());
    assert!(Control::Switch(vec![1, 2]).falls_through());
    for c in [
        Control::Goto(1),
        Control::Leave(1),
        Control::Return,
        Control::Throw,
        Control::EndFinally,
        Control::EndFilter,
        Control::Jump,
    ] {
        assert!(!c.falls_through(), "{c}");
    }
    assert_eq!(Control::Switch(vec![1, 2]).targets(), &[1, 2]);
    assert_eq!(Control::Leave(7).targets(), &[7]);
    assert!(Control::Return.targets().is_empty());
}
