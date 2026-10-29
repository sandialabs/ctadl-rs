//! The invariants of the flow layer, as a checker that the unit tests and the
//! corpus property test (`cil:flow` in xtask) share. They are numbered 7.1–7.9,
//! and each violation names the number it breaks.
//!
//! Layer 1, for each instruction's [`InsFlow`] ([`layer1_violations`]):
//!
//! - **7.1** The instruction has as many inputs as it pops and as many outputs
//!   as it pushes. The counts come from Cecil's opcode table
//!   ([`StackBehaviour`]), with `Varpop`/`Varpush` resolved from the operand's
//!   signature ([`stack_counts`]), independently of the per-opcode table in
//!   `opcode.rs`, so they are an external oracle. An `Opaque` fallback for a
//!   signature that does not resolve has no counts to check.
//! - **7.2** Every `In(i)` is read by some effect, unless its role is
//!   `Discarded`; a `Discarded` input is read by none.
//! - **7.3** Every `Out(i)` is written by exactly one effect.
//! - **7.4** Every `Temp` is written before it is read, within the instruction.
//! - **7.5** An input's [`Role`] allows each of its uses. `Value` is only an
//!   assign or store source. `Object` is a path root, an address base, the
//!   source of a cast or unbox (castclass, isinst, unbox), or ldvirtftn's
//!   receiver. `Address` is a path root, a cleared address, or mkrefany's
//!   source. `SrcAddress` is only a path root. `Receiver` is a call's receiver,
//!   or a path root or address base (the runtime methods of multi-dimensional
//!   arrays). `Arg(k)` is only argument `k`, `Index` only an element index, and
//!   `Size` only a `New` size or a block size. `Lhs`/`Rhs` are the source of a
//!   derived value or of a test, and `Condition` only of a test. `Callee`,
//!   `Exception` and `ReturnValue` are only calli's callee, the thrown value
//!   and the returned value. Any input may be an `Opaque` source.
//! - **7.6** The per-opcode table's `match` on [`Code`] has no `_` arm, so a
//!   new opcode is a compile error. The compiler checks this one.
//!
//! Layer 2, for each [`MethodFlow`] ([`layer2_violations`]):
//!
//! - **7.7** No instruction pops more than its entry height holds or leaves
//!   more than `max_stack`, and every edge into a block other than a handler
//!   entry leaves the height the block starts at, unless a [`FlowWarning`]
//!   records it.
//! - **7.8** Binding by depth changes nothing an instruction does. Its effects
//!   bound to depth-named variables, and its Layer 1 effects with every slot
//!   named apart, run through a small symbolic interpreter, must give the same
//!   events (memory writes, calls, tests and exits, with the values they read),
//!   the same outputs, and the same arguments, locals and thrown value. This is
//!   what checks the collision rule of `method::bind_effects`.
//! - **7.9** `succs` and `preds` agree, every catch and filter entry has
//!   height 1, every finally and fault entry has height 0, and every `leave`
//!   target has height 0.

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

/// Whether an input of `role` may be used as `u` (invariant 7.5).
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

    // 7.1: pop and push counts.
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

// ---- Layer 2 (7.7–7.9) -------------------------------------------------------

use super::method::{BoundIns, FlowWarning, HandlerKind, MethodFlow, Var};
use super::{Control as Ctl, Transfer as Tr};
use std::collections::HashMap;
use std::hash::Hash;

/// A symbolic value of the 7.8 interpreter: an initial value, or what an
/// effect computed from the values it read.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Init(String),
    /// Effect index, which of its results, and the values it read.
    Term(usize, usize, Vec<Val>),
}

/// What one run of an instruction's effects does: the values it leaves in
/// variables, and the observable events (memory writes, calls, tests,
/// exits) with the values they read.
struct Run<S> {
    env: HashMap<S, Val>,
    events: Vec<(usize, Vec<Val>)>,
}

/// Runs `effects` symbolically. A plain copy is the identity; anything else
/// makes a term of what it read.
fn interpret<S: Clone + Eq + Hash>(effects: &[Effect<S>], init: impl Fn(&S) -> Val) -> Run<S> {
    let mut run = Run {
        env: HashMap::new(),
        events: Vec::new(),
    };
    for (k, e) in effects.iter().enumerate() {
        let mut vals = Vec::new();
        e.for_each_read(|s| vals.push(run.env.get(s).cloned().unwrap_or_else(|| init(s))));
        match e {
            Effect::Assign {
                dst,
                transfer: Tr::Copy,
                srcs,
            } if srcs.len() == 1 => {
                run.env.insert(dst.clone(), vals[0].clone());
            }
            Effect::Store { .. }
            | Effect::Clear { .. }
            | Effect::Return { .. }
            | Effect::Throw { .. }
            | Effect::Test { .. }
            | Effect::Jump { .. }
            | Effect::Call(_)
            | Effect::Opaque { .. } => run.events.push((k, vals.clone())),
            _ => {}
        }
        if matches!(e, Effect::Assign { transfer: Tr::Copy, srcs, .. } if srcs.len() == 1) {
            continue;
        }
        let mut n = 0;
        e.for_each_write(|d| {
            run.env.insert(d.clone(), Val::Term(k, n, vals.clone()));
            n += 1;
        });
    }
    run
}

fn slot_name(s: &Slot) -> String {
    s.to_string()
}

fn var_name(v: &Var, base: u16) -> String {
    match *v {
        Var::Stack(d) if d >= base => format!("i{}", d - base),
        Var::Caught(_) => "caught".to_string(),
        other => other.to_string(),
    }
}

/// Invariant 7.8 for one instruction: binding by depth changes nothing an
/// instruction does compared with binding In and Out to distinct names.
fn check_binding(ins: &BoundIns) -> Option<String> {
    let flow = &ins.flow;
    let base = ins.height.saturating_sub(flow.inputs.len() as u16);
    let distinct = interpret(&flow.effects, |s| Val::Init(slot_name(s)));
    let bound = interpret(&ins.effects, |v| Val::Init(var_name(v, base)));
    // The appended collision copies are not events.
    if distinct.events != bound.events {
        return Some(format!(
            "7.8: events differ: {:?} vs {:?}",
            distinct.events, bound.events
        ));
    }
    for (j, var) in ins.outputs.iter().enumerate() {
        let d = distinct.env.get(&Slot::Out(j as u8));
        let b = bound.env.get(var);
        if d != b {
            return Some(format!("7.8: o{j} is {d:?} but {var} is {b:?}"));
        }
    }
    for (slot, val) in &distinct.env {
        let var = match *slot {
            Slot::Arg(n) => Var::Arg(n),
            Slot::Local(n) => Var::Local(n),
            Slot::Thrown => Var::Thrown,
            _ => continue,
        };
        if bound.env.get(&var) != Some(val) {
            return Some(format!("7.8: {slot} differs from {var}"));
        }
    }
    None
}

/// The violations of invariants 7.7–7.9 by `mf`. Empty when it holds them
/// all.
pub fn layer2_violations(mf: &MethodFlow) -> Vec<String> {
    let mut v = Vec::new();
    let warned = |w: &dyn Fn(&FlowWarning) -> bool| mf.warnings.iter().any(w);

    for (b, block) in mf.blocks.iter().enumerate() {
        // 7.7: underflows and max_stack, or a warning.
        for ins in &block.instrs {
            let offset = ins.flow.offset;
            let pops = ins.flow.inputs.len() as u16;
            if ins.height < pops
                && !warned(
                    &|w| matches!(w, FlowWarning::Underflow { offset: o, .. } if *o == offset),
                )
            {
                v.push(format!("7.7: IL_{offset:04x} underflows without a warning"));
            }
            let after = ins.height.saturating_sub(pops) as u32 + ins.flow.outputs as u32;
            if after > mf.max_stack
                && !ins.is_handler_entry()
                && !warned(
                    &|w| matches!(w, FlowWarning::ExceedsMaxStack { offset: o, .. } if *o == offset),
                )
            {
                v.push(format!(
                    "7.7: IL_{offset:04x} exceeds max-stack {} without a warning",
                    mf.max_stack
                ));
            }
            // 7.8
            if !ins.is_handler_entry()
                && let Some(msg) = check_binding(ins)
            {
                v.push(format!("IL_{offset:04x} {}: {msg}", ins.flow.opcode));
            }
        }
        // 7.7: predecessors agree on the entry height.
        for &s in &block.succs {
            let succ = &mf.blocks[s];
            if succ.handler_entry.is_none()
                && block.exit_height != succ.entry_height
                && !warned(
                    &|w| matches!(w, FlowWarning::HeightMismatch { offset, .. } if *offset == succ.start),
                )
            {
                v.push(format!(
                    "7.7: IL_{:04x} leaves height {} but IL_{:04x} starts at {}, without a warning",
                    block.start, block.exit_height, succ.start, succ.entry_height
                ));
            }
            // 7.9: succs and preds agree.
            if !succ.preds.contains(&b) {
                v.push(format!(
                    "7.9: IL_{:04x} -> IL_{:04x} is missing from the preds",
                    block.start, succ.start
                ));
            }
        }
        for &p in &block.preds {
            if !mf.blocks[p].succs.contains(&b) {
                v.push(format!(
                    "7.9: IL_{:04x} has pred IL_{:04x}, which does not lead to it",
                    block.start, mf.blocks[p].start
                ));
            }
        }
        // 7.9: leave targets start empty.
        if let Some(Ctl::Leave(t)) = block.instrs.last().map(|i| &i.flow.control)
            && let Some(tb) = mf.block_at(*t)
            && mf.blocks[tb].entry_height != 0
        {
            v.push(format!(
                "7.9: leave target IL_{t:04x} has a non-empty stack"
            ));
        }
    }
    // 7.9: handler entries.
    for (index, h) in mf.handlers.iter().enumerate() {
        let want = match h.kind {
            HandlerKind::Catch | HandlerKind::Filter => 1,
            HandlerKind::Finally | HandlerKind::Fault => 0,
        };
        for b in [h.entry, h.handler_block].into_iter().flatten() {
            let height = mf.blocks[b].entry_height;
            if height != want {
                v.push(format!(
                    "7.9: handler {index} ({:?}) entry IL_{:04x} has height {height}, not {want}",
                    h.kind, mf.blocks[b].start
                ));
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::super::method::bind_effects;
    use super::super::{CallSite, NewKind, Prefixes, TypeOp};
    use super::*;
    use crate::cil::opcodes;
    use crate::model::{MethodSig, TypeSig};
    use crate::token::MetadataToken;

    fn newobj(args: u8) -> InsFlow {
        let sig = MethodSig {
            has_this: true,
            explicit_this: false,
            calling_convention: 0,
            generic_arity: 0,
            return_type: TypeSig::Void,
            params: vec![TypeSig::I4; args as usize],
        };
        InsFlow {
            offset: 0,
            size: 5,
            opcode: opcodes::NEWOBJ,
            prefixes: Prefixes::default(),
            inputs: (0..args as u16).map(Role::Arg).collect(),
            outputs: 1,
            effects: vec![
                Effect::New {
                    dst: Slot::Out(0),
                    kind: NewKind::Object,
                    ty: Some(TypeOp {
                        token: MetadataToken(0x0200_0002),
                    }),
                    sizes: vec![],
                },
                Effect::Call(CallSite {
                    kind: CallKind::Ctor,
                    method: None,
                    sig,
                    receiver: Some(Slot::Out(0)),
                    args: (0..args).map(Slot::In).collect(),
                    result: None,
                    thrown: Slot::Thrown,
                    byref_args: vec![],
                    prefixes: Prefixes::default(),
                }),
            ],
            control: Ctl::Next,
        }
    }

    fn bound(flow: InsFlow, height: u16) -> BoundIns {
        let base = height - flow.inputs.len() as u16;
        let (effects, inputs, outputs) = bind_effects(&flow, base, None);
        BoundIns {
            input_places: vec![None; inputs.len()],
            input_kinds: vec![],
            flow,
            effects,
            inputs,
            outputs,
            height,
        }
    }

    fn rendered(ins: &BoundIns) -> Vec<String> {
        ins.effects.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn newobj_with_arguments_goes_through_a_temp() {
        // newobj C::.ctor(a, b) at entry height 2: the new object shares s0
        // with the first argument.
        let ins = bound(newobj(2), 2);
        assert_eq!(
            rendered(&ins),
            [
                "New t0 Object(02000002)",
                "Call Ctor recv=t0 args=[s0, s1] thrown=thrown",
                "Assign s0 := [t0] Copy",
            ]
        );
        assert_eq!(ins.outputs, [Var::Stack(0)]);
        assert_eq!(check_binding(&ins), None);
    }

    #[test]
    fn newobj_without_arguments_needs_no_temp() {
        let ins = bound(newobj(0), 3);
        assert_eq!(
            rendered(&ins),
            [
                "New s3 Object(02000002)",
                "Call Ctor recv=s3 args=[] thrown=thrown"
            ]
        );
        assert_eq!(check_binding(&ins), None);
    }

    #[test]
    fn dup_binds_without_a_temp() {
        let flow = InsFlow {
            offset: 0,
            size: 1,
            opcode: opcodes::DUP,
            prefixes: Prefixes::default(),
            inputs: vec![Role::Value],
            outputs: 2,
            effects: vec![
                Effect::Assign {
                    dst: Slot::Out(0),
                    srcs: vec![Slot::In(0)],
                    transfer: Tr::Copy,
                },
                Effect::Assign {
                    dst: Slot::Out(1),
                    srcs: vec![Slot::In(0)],
                    transfer: Tr::Copy,
                },
            ],
            control: Ctl::Next,
        };
        let ins = bound(flow, 1);
        assert_eq!(
            rendered(&ins),
            ["Assign s0 := [s0] Copy", "Assign s1 := [s0] Copy"]
        );
        assert_eq!(check_binding(&ins), None);
    }

    #[test]
    fn the_interpreter_catches_a_binding_without_the_rule() {
        // newobj bound naively: New s0 overwrites the first argument.
        let mut ins = bound(newobj(2), 2);
        ins.effects = newobj(2)
            .effects
            .into_iter()
            .map(|e| {
                e.map(|s| match s {
                    Slot::In(i) => Var::Stack(i as u16),
                    Slot::Out(j) => Var::Stack(j as u16),
                    Slot::Thrown => Var::Thrown,
                    _ => unreachable!(),
                })
            })
            .collect();
        let msg = check_binding(&ins).expect("a violation");
        assert!(msg.starts_with("7.8"), "{msg}");
    }
}
