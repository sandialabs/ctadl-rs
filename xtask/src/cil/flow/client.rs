//! A client of the flow layer written as `docs/cil-flow-api.md` §9 says one
//! can be: it matches only on `Effect`, `CallKind` and `Control`, never on
//! `Code` (this module imports nothing from `cil_reader::cil`). It answers
//! "which arguments reach the return value", a small may-taint analysis over
//! a `MethodFlow`, with these policies of its own:
//!
//! - no implicit flow (`Test` is ignored), indices do not taint elements;
//! - a store taints its root (the object, or the place an address resolves
//!   to); a load from a tainted root is tainted;
//! - a call's result is tainted by any tainted argument or receiver, and so
//!   is every `ref`/`out` argument whose address resolves to a local or
//!   argument (copy-out, as for `int.TryParse(s, out n)`).

use std::collections::{BTreeSet, HashSet, VecDeque};

use cil_reader::flow::{CallKind, Control, Effect, InsId, MethodFlow, Place, Root, Var};

type Taint = HashSet<Var>;

/// The arguments whose values may reach a `ret`.
pub fn args_reaching_return(mf: &MethodFlow, arg_count: u16) -> BTreeSet<u16> {
    (0..arg_count)
        .filter(|&a| reaches_return(mf, Var::Arg(a)))
        .collect()
}

fn place_var(p: &Place<Var>) -> Option<Var> {
    match p {
        Place::Local(n) => Some(Var::Local(*n)),
        Place::Arg(n) => Some(Var::Arg(*n)),
        Place::Field(base, _) | Place::Element(base, _) => Some(*base),
        Place::Static(_) => None,
    }
}

/// The local or argument that the address in `v`, an input of `at`, points
/// into, when it resolves.
fn resolved(mf: &MethodFlow, at: InsId, v: &Var) -> Option<Var> {
    let i = mf.ins(at).inputs.iter().position(|x| x == v)?;
    mf.resolve_address(at, i as u8).and_then(place_var)
}

fn reaches_return(mf: &MethodFlow, source: Var) -> bool {
    let n = mf.blocks.len();
    let mut entry: Vec<Option<Taint>> = vec![None; n];
    let mut queue = VecDeque::new();
    if n > 0 {
        entry[0] = Some(Taint::from([source]));
        queue.push_back(0);
    }
    let mut returned = false;
    while let Some(b) = queue.pop_front() {
        let mut t = entry[b].clone().unwrap_or_default();
        for (index, ins) in mf.blocks[b].instrs.iter().enumerate() {
            let at = InsId { block: b, index };
            for e in &ins.effects {
                returned |= step(mf, at, e, &mut t);
            }
        }
        if matches!(
            mf.blocks[b].instrs.last().map(|i| &i.flow.control),
            Some(Control::Leave(_) | Control::EndFinally | Control::EndFilter)
        ) {
            t.retain(|v| !matches!(v, Var::Stack(_)));
        }
        let block = &mf.blocks[b];
        for &s in block.succs.iter().chain(&block.exc_succs) {
            let merged: Taint = match &entry[s] {
                Some(old) => old.union(&t).copied().collect(),
                None => t.clone(),
            };
            if entry[s].as_ref() != Some(&merged) {
                entry[s] = Some(merged);
                queue.push_back(s);
            }
        }
    }
    returned
}

/// Applies one effect; true when it returns a tainted value.
fn step(mf: &MethodFlow, at: InsId, e: &Effect<Var>, t: &mut Taint) -> bool {
    let tainted = |t: &Taint, v: &Var| t.contains(v);
    let set = |t: &mut Taint, v: Var, on: bool| {
        if on {
            t.insert(v);
        } else {
            t.remove(&v);
        }
    };
    match e {
        Effect::Assign { dst, srcs, .. } => {
            let on = srcs.iter().any(|s| tainted(t, s));
            set(t, *dst, on);
        }
        Effect::Load { dst, src } => {
            let on = matches!(src.root, Root::Slot(r) if tainted(t, &r));
            set(t, *dst, on);
        }
        Effect::Store { dst, src } => {
            if let (true, Root::Slot(root)) = (tainted(t, src), dst.root) {
                t.insert(root);
                t.extend(resolved(mf, at, &root));
            }
        }
        Effect::AddressOf { dst, place } => {
            let on = place_var(place).is_some_and(|v| tainted(t, &v));
            set(t, *dst, on);
        }
        Effect::Call(site) => {
            let mut on = site.args.iter().any(|a| tainted(t, a))
                || site.receiver.is_some_and(|r| tainted(t, &r));
            if let CallKind::Indirect { callee } = site.kind {
                on |= tainted(t, &callee);
            }
            if on {
                // Copy-out through ref/out arguments that resolve.
                for &k in &site.byref_args {
                    if let Some(arg) = site.args.get(k as usize) {
                        t.extend(resolved(mf, at, arg));
                    }
                }
            }
            if let Some(r) = site.result {
                set(t, r, on);
            }
        }
        Effect::Return { src: Some(s) } => return tainted(t, s),
        Effect::Const { dst, .. } | Effect::New { dst, .. } | Effect::FunctionPtr { dst, .. } => {
            set(t, *dst, false)
        }
        Effect::Opaque { srcs, dsts, .. } => {
            let on = srcs.iter().any(|s| tainted(t, s));
            for d in dsts {
                set(t, *d, on);
            }
        }
        Effect::Return { src: None }
        | Effect::Clear { .. }
        | Effect::Throw { .. }
        | Effect::Jump { .. }
        | Effect::Test { .. } => {}
    }
    false
}
