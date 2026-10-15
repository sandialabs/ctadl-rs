//! The Layer 1 invariants of `docs/cil-flow-api.md` §7.1–7.5, as a checker
//! that the unit tests and the corpus property test (`cil:flow` in xtask)
//! share.
//!
//! The pop and push counts come from Cecil's opcode table
//! ([`StackBehaviour`]), with `Varpop`/`Varpush` resolved from the operand's
//! signature, independently of the per-opcode table in `opcode.rs`. That is
//! the external oracle §7.1 asks for.

use super::operands::FlowCx;
use super::{CallKind, Effect, Field, InsFlow, Op, Place, Role, Root, Slot, Transfer};
use crate::cil::{Code, Ins, RawOperand, StackBehaviour};
use crate::model::TypeSig;

/// How many values `ins` pops and pushes, by Cecil's opcode table. `None`
/// when that depends on a signature that does not resolve. `leave`'s
/// `PopAll` counts as 0: Layer 1 pops nothing, and Layer 2 empties the stack.
pub fn stack_counts(ins: &Ins<'_>, cx: &FlowCx<'_>) -> Option<(usize, usize)> {
    use StackBehaviour::*;
    let token = match ins.operand {
        RawOperand::Token(t) | RawOperand::CallSite(t) => Some(t),
        _ => None,
    };
    let sig = || -> Option<crate::model::MethodSig> {
        let t = token?;
        if ins.opcode.code == Code::Calli {
            cx.operands.call_site(t).ok()
        } else {
            cx.operands.method(t).ok().map(|m| m.op.sig)
        }
    };
    let returns = |ret: &TypeSig| ret.unwrap_modifiers() != &TypeSig::Void;
    let pop = match ins.opcode.stack_pop {
        Pop0 | PopAll => 0,
        Pop1 | Popi | Popref => 1,
        Pop1_pop1 | Popi_pop1 | Popi_popi | Popi_popi8 | Popi_popr4 | Popi_popr8 | Popref_pop1
        | Popref_popi => 2,
        Popi_popi_popi | Popref_popi_popi | Popref_popi_popi8 | Popref_popi_popr4
        | Popref_popi_popr8 | Popref_popi_popref => 3,
        Varpop => match ins.opcode.code {
            Code::Ret => returns(&cx.method_sig.return_type) as usize,
            Code::Newobj => sig()?.params.len(),
            Code::Calli => {
                let s = sig()?;
                s.has_this as usize + s.params.len() + 1
            }
            _ => {
                let s = sig()?;
                s.has_this as usize + s.params.len()
            }
        },
        // Push behaviours never appear as a pop behaviour.
        Push0 | Push1 | Push1_push1 | Pushi | Pushi8 | Pushr4 | Pushr8 | Pushref | Varpush => {
            return None;
        }
    };
    let push = match ins.opcode.stack_push {
        Push0 => 0,
        Push1 | Pushi | Pushi8 | Pushr4 | Pushr8 | Pushref => 1,
        Push1_push1 => 2,
        Varpush => returns(&sig()?.return_type) as usize,
        _ => return None,
    };
    Some((pop, push))
}

/// How an effect uses one slot it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    AssignSrc(TransferKind),
    StoreSrc,
    PathRoot,
    PlaceBase,
    Index,
    Size,
    CallReceiver,
    CallArg(u16),
    Callee,
    FnPtrReceiver,
    ClearAddr,
    TestSrc,
    ReturnSrc,
    ThrowSrc,
    OpaqueSrc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferKind {
    Copy,
    Cast,
    Box,
    Unbox,
    MkRefAny,
    Derived,
}

fn transfer_kind(t: &Transfer) -> TransferKind {
    match t {
        Transfer::Copy => TransferKind::Copy,
        Transfer::Cast(_) => TransferKind::Cast,
        Transfer::Box(_) => TransferKind::Box,
        Transfer::Unbox(_) => TransferKind::Unbox,
        Transfer::Derived(Op::MkRefAny(_)) => TransferKind::MkRefAny,
        Transfer::Derived(_) => TransferKind::Derived,
    }
}

fn uses(e: &Effect<Slot>, out: &mut Vec<(Slot, Use)>) {
    let path = |root: &Root<Slot>, field: &Field<Slot>, out: &mut Vec<(Slot, Use)>| {
        if let Root::Slot(s) = root {
            out.push((*s, Use::PathRoot));
        }
        match field {
            Field::Element { indices } => out.extend(indices.iter().map(|s| (*s, Use::Index))),
            Field::Block { size } => out.push((*size, Use::Size)),
            Field::Named(_) | Field::Length | Field::Deref => {}
        }
    };
    match e {
        Effect::Assign { srcs, transfer, .. } => {
            let k = transfer_kind(transfer);
            out.extend(srcs.iter().map(|s| (*s, Use::AssignSrc(k))));
        }
        Effect::Const { .. } | Effect::Jump { .. } => {}
        Effect::Load { src, .. } => path(&src.root, &src.field, out),
        Effect::Store { dst, src } => {
            path(&dst.root, &dst.field, out);
            out.push((*src, Use::StoreSrc));
        }
        Effect::AddressOf { place, .. } => match place {
            Place::Field(base, _) => out.push((*base, Use::PlaceBase)),
            Place::Element(base, indices) => {
                out.push((*base, Use::PlaceBase));
                out.extend(indices.iter().map(|s| (*s, Use::Index)));
            }
            Place::Local(_) | Place::Arg(_) | Place::Static(_) => {}
        },
        Effect::New { sizes, .. } => out.extend(sizes.iter().map(|s| (*s, Use::Size))),
        Effect::FunctionPtr { receiver, .. } => {
            out.extend(receiver.iter().map(|s| (*s, Use::FnPtrReceiver)))
        }
        Effect::Clear { addr } => out.push((*addr, Use::ClearAddr)),
        Effect::Call(site) => {
            out.extend(site.receiver.iter().map(|s| (*s, Use::CallReceiver)));
            out.extend(
                site.args
                    .iter()
                    .enumerate()
                    .map(|(k, s)| (*s, Use::CallArg(k as u16))),
            );
            if let CallKind::Indirect { callee } = &site.kind {
                out.push((*callee, Use::Callee));
            }
        }
        Effect::Return { src } => out.extend(src.iter().map(|s| (*s, Use::ReturnSrc))),
        Effect::Throw { src } => out.push((*src, Use::ThrowSrc)),
        Effect::Test { srcs } => out.extend(srcs.iter().map(|s| (*s, Use::TestSrc))),
        Effect::Opaque { srcs, .. } => out.extend(srcs.iter().map(|s| (*s, Use::OpaqueSrc))),
    }
}

/// Whether an input of `role` may be used as `u` (invariant 7.5). An
/// `Object` is a path root or an address base, or the source of a cast or
/// unbox (castclass, isinst, unbox) or the receiver of ldvirtftn. An
/// `Address` is a path root or a cleared place, or the source of mkrefany.
/// A `Value` is only ever an assign or store source. Any input may be an
/// `Opaque` source.
fn allowed(role: Role, u: Use) -> bool {
    use TransferKind as T;
    use Use::*;
    if u == OpaqueSrc {
        return true;
    }
    match role {
        Role::Value => matches!(u, AssignSrc(_) | StoreSrc),
        Role::Object => matches!(
            u,
            PathRoot | PlaceBase | AssignSrc(T::Cast | T::Unbox) | FnPtrReceiver
        ),
        Role::Receiver => matches!(u, CallReceiver | PathRoot | PlaceBase),
        Role::Arg(k) => u == CallArg(k),
        Role::Index => u == Index,
        Role::Address => matches!(u, PathRoot | ClearAddr | AssignSrc(T::MkRefAny)),
        Role::SrcAddress => u == PathRoot,
        Role::Size => u == Size,
        Role::Lhs | Role::Rhs => matches!(u, AssignSrc(T::Derived) | TestSrc),
        Role::Condition => u == TestSrc,
        Role::Callee => u == Callee,
        Role::Exception => u == ThrowSrc,
        Role::ReturnValue => u == ReturnSrc,
        Role::Discarded => false,
    }
}

/// The violations of invariants 7.1–7.5 by `flow`, the Layer 1 record of
/// `ins` (the instruction itself, after any prefixes). Empty when it holds
/// them all.
pub fn layer1_violations(ins: &Ins<'_>, flow: &InsFlow, cx: &FlowCx<'_>) -> Vec<String> {
    let mut v = Vec::new();
    let n_in = flow.inputs.len();
    let n_out = flow.outputs as usize;

    // 7.1: pop and push counts. An Opaque fallback for a signature that did
    // not resolve has no counts to check.
    match stack_counts(ins, cx) {
        Some((pop, push)) => {
            if (n_in, n_out) != (pop, push) {
                v.push(format!(
                    "7.1: pops {n_in} and pushes {n_out}, the opcode table says {pop} and {push}"
                ));
            }
        }
        None if !flow.is_opaque() => {
            v.push("7.1: counts unknown, but the effects are not Opaque".to_string());
        }
        None => {}
    }

    let mut all_uses = Vec::new();
    let mut temps_defined = Vec::new();
    let mut out_defs = vec![0usize; n_out];
    for (k, e) in flow.effects.iter().enumerate() {
        let mut u = Vec::new();
        uses(e, &mut u);
        for &(s, _) in &u {
            match s {
                Slot::In(i) if i as usize >= n_in => {
                    v.push(format!("effect {k} reads {s}, past the {n_in} inputs"));
                }
                Slot::Out(i) if i as usize >= n_out => {
                    v.push(format!("effect {k} reads {s}, past the {n_out} outputs"));
                }
                // 7.4: a temporary is defined before it is used.
                Slot::Temp(t) if !temps_defined.contains(&t) => {
                    v.push(format!("7.4: effect {k} reads {s} before defining it"));
                }
                _ => {}
            }
        }
        all_uses.extend(u);
        e.for_each_write(|s| match *s {
            Slot::Out(i) if (i as usize) < n_out => out_defs[i as usize] += 1,
            Slot::Out(i) => v.push(format!("effect {k} writes o{i}, past the {n_out} outputs")),
            Slot::In(i) => v.push(format!("effect {k} writes input i{i}")),
            Slot::Temp(t) => temps_defined.push(t),
            Slot::Arg(_) | Slot::Local(_) | Slot::Thrown | Slot::Caught => {}
        });
    }

    for (i, &role) in flow.inputs.iter().enumerate() {
        let slot = Slot::In(i as u8);
        let mine: Vec<Use> = all_uses
            .iter()
            .filter(|(s, _)| *s == slot)
            .map(|&(_, u)| u)
            .collect();
        // 7.2: every input is used unless it is discarded.
        if role == Role::Discarded {
            if !mine.is_empty() {
                v.push(format!("{slot} is Discarded but used as {mine:?}"));
            }
        } else if mine.is_empty() {
            v.push(format!("7.2: {slot} ({role:?}) is never used"));
        }
        // 7.5: the role matches every use.
        for u in mine {
            if !allowed(role, u) {
                v.push(format!(
                    "7.5: {slot} has role {role:?} but is used as {u:?}"
                ));
            }
        }
    }

    // 7.3: every output is defined by exactly one effect.
    for (i, &n) in out_defs.iter().enumerate() {
        if n != 1 {
            v.push(format!("7.3: o{i} is defined {n} times"));
        }
    }
    v
}
