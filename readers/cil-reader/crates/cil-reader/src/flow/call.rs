//! Call sites (`docs/cil-flow-api.md` §5.8): `call`, `callvirt`, `newobj` and
//! `calli`, including the runtime methods of multi-dimensional arrays (§5.5),
//! which become element accesses instead of calls.

use super::opcode::{Builder, token};
use super::operands::{ArrayMethod, FlowCx, ResolvedMethod};
use super::{
    CallKind, CallSite, Effect, Field, MethodOp, NewKind, Path, Place, Prefixes, Role, Root, Slot,
};
use crate::cil::{Code, Ins};
use crate::model::{MethodSig, TypeSig};

/// Indices of the `ByRef` parameters of `sig`.
pub(crate) fn byref_params(sig: &MethodSig) -> Vec<u16> {
    sig.params
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.unwrap_modifiers(), TypeSig::ByRef(_)))
        .map(|(i, _)| i as u16)
        .collect()
}

fn returns_value(sig: &MethodSig) -> bool {
    sig.return_type.unwrap_modifiers() != &TypeSig::Void
}

pub(super) fn call(b: &mut Builder, ins: &Ins<'_>, prefixes: Prefixes, cx: &FlowCx<'_>) {
    let tok = token(ins);
    if ins.opcode.code == Code::Calli {
        match cx.operands.call_site(tok) {
            Ok(sig) => calli(b, sig, prefixes),
            Err(_) => b.opaque(tok),
        }
        return;
    }
    let Ok(resolved) = cx.operands.method(tok) else {
        // The pop count is in the signature that did not resolve: nothing
        // is known about the operands.
        return b.opaque(tok);
    };
    if let Some(array) = resolved.array
        && array_access(b, ins.opcode.code, array, &resolved)
    {
        return;
    }
    if ins.opcode.code == Code::Newobj {
        newobj(b, resolved, prefixes);
    } else {
        call_or_callvirt(b, ins.opcode.code, resolved.op, prefixes);
    }
}

/// `Arg(0)` .. `Arg(n - 1)`.
fn arg_roles(n: usize) -> impl Iterator<Item = Role> {
    (0..n as u16).map(Role::Arg)
}

fn site(
    kind: CallKind<Slot>,
    method: Option<MethodOp>,
    sig: MethodSig,
    receiver: Option<Slot>,
    first_arg: u8,
    result: Option<Slot>,
    prefixes: Prefixes,
) -> CallSite<Slot> {
    let args = (0..sig.params.len() as u8)
        .map(|k| Slot::In(first_arg + k))
        .collect();
    CallSite {
        kind,
        method,
        byref_args: byref_params(&sig),
        sig,
        receiver,
        args,
        result,
        thrown: Slot::Thrown,
        prefixes,
    }
}

fn call_or_callvirt(b: &mut Builder, code: Code, op: MethodOp, prefixes: Prefixes) {
    let has_this = op.sig.has_this;
    let mut roles = Vec::with_capacity(op.sig.params.len() + 1);
    if has_this {
        roles.push(Role::Receiver);
    }
    roles.extend(arg_roles(op.sig.params.len()));
    b.pops(&roles);
    let result = returns_value(&op.sig).then(|| {
        b.pushes(1);
        Slot::Out(0)
    });
    let kind = match (code, has_this) {
        (Code::Callvirt, _) => CallKind::Virtual {
            constrained: prefixes.constrained,
        },
        (_, true) => CallKind::Instance,
        (_, false) => CallKind::Static,
    };
    let sig = op.sig.clone();
    b.effect(Effect::Call(site(
        kind,
        Some(op),
        sig,
        has_this.then_some(Slot::In(0)),
        has_this as u8,
        result,
        prefixes,
    )));
}

fn newobj(b: &mut Builder, resolved: ResolvedMethod, prefixes: Prefixes) {
    let op = resolved.op;
    b.pops(&arg_roles(op.sig.params.len()).collect::<Vec<_>>());
    b.pushes(1);
    let kind = if resolved.declaring_is_value_type == Some(true) {
        NewKind::ValueType
    } else {
        NewKind::Object
    };
    b.effect(Effect::New {
        dst: Slot::Out(0),
        kind,
        ty: resolved.declaring,
        sizes: vec![],
    });
    let sig = op.sig.clone();
    b.effect(Effect::Call(site(
        CallKind::Ctor,
        Some(op),
        sig,
        Some(Slot::Out(0)),
        0,
        None,
        prefixes,
    )));
}

fn calli(b: &mut Builder, sig: MethodSig, prefixes: Prefixes) {
    let has_this = sig.has_this;
    let n = sig.params.len();
    let mut roles = Vec::with_capacity(n + 2);
    if has_this {
        roles.push(Role::Receiver);
    }
    roles.extend(arg_roles(n));
    roles.push(Role::Callee);
    b.pops(&roles);
    let callee = Slot::In((roles.len() - 1) as u8);
    let result = returns_value(&sig).then(|| {
        b.pushes(1);
        Slot::Out(0)
    });
    b.effect(Effect::Call(site(
        CallKind::Indirect { callee },
        None,
        sig,
        has_this.then_some(Slot::In(0)),
        has_this as u8,
        result,
        prefixes,
    )));
}

/// `Get`/`Set`/`Address`/`.ctor` of a multi-dimensional array, as element
/// effects. False when the call's shape is not the runtime method's (an
/// instance `call`/`callvirt`, or a `newobj` for `.ctor`), so the caller
/// reports it as an ordinary call.
fn array_access(
    b: &mut Builder,
    code: Code,
    array: ArrayMethod,
    resolved: &ResolvedMethod,
) -> bool {
    let sig = &resolved.op.sig;
    let n = sig.params.len();
    let indices = |count: usize| (1..=count as u8).map(Slot::In).collect::<Vec<_>>();
    let index_roles = |count: usize| {
        std::iter::once(Role::Receiver)
            .chain(std::iter::repeat_n(Role::Index, count))
            .collect::<Vec<_>>()
    };
    let is_call = matches!(code, Code::Call | Code::Callvirt) && sig.has_this;
    match array {
        ArrayMethod::Get if is_call && returns_value(sig) => {
            b.pops(&index_roles(n));
            b.pushes(1);
            b.effect(Effect::Load {
                dst: Slot::Out(0),
                src: Path {
                    root: Root::Slot(Slot::In(0)),
                    field: Field::Element {
                        indices: indices(n),
                    },
                },
            });
        }
        ArrayMethod::Set if is_call && n >= 1 && !returns_value(sig) => {
            let mut roles = index_roles(n - 1);
            roles.push(Role::Value);
            b.pops(&roles);
            b.effect(Effect::Store {
                dst: Path {
                    root: Root::Slot(Slot::In(0)),
                    field: Field::Element {
                        indices: indices(n - 1),
                    },
                },
                src: Slot::In(n as u8),
            });
        }
        ArrayMethod::Address if is_call && returns_value(sig) => {
            b.pops(&index_roles(n));
            b.pushes(1);
            b.effect(Effect::AddressOf {
                dst: Slot::Out(0),
                place: Place::Element(Slot::In(0), indices(n)),
            });
        }
        ArrayMethod::Ctor if code == Code::Newobj => {
            b.pops(&vec![Role::Size; n]);
            b.pushes(1);
            b.effect(Effect::New {
                dst: Slot::Out(0),
                kind: NewKind::Array,
                ty: resolved.declaring,
                sizes: (0..n as u8).map(Slot::In).collect(),
            });
        }
        _ => return false,
    }
    true
}
