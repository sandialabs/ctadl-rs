//! Managed pointers. A pointer lowers to the place it points at whenever that can be traced,
//! and to the dereference field `DEREF_FIELD` (`D` below) when it cannot.
//!
//! | Effect on pointer `v`      | Resolved to `P`                    | Unresolved                       |
//! | -------------------------- | ---------------------------------- | -------------------------------- |
//! | `Load dst := v.*`          | read `P` into `dst`                | `dst := load v.D`                |
//! | `Store v.* := x`           | write `x` into `P`                 | `store v.D := x`                 |
//! | `Load dst := v.f`          | field `f` of `P`                   | `t := load v.D; dst := load t.f` |
//! | `Store v.f := x`           | field `f` of `P`                   | `t := load v.D; store t.f := x`  |
//! | `Clear v`                  | `P := ()` for a local or argument; else nothing | nothing             |
//!
//! Reading and writing a resolved place `P`:
//!
//! | Place          | read `P` into `x`    | write `x` into `P`    | field `f` of `P`                 |
//! | -------------- | -------------------- | --------------------- | -------------------------------- |
//! | `Local(n)`     | `x := loc{n}`        | `loc{n} := x`         | `loc{n}.f`                       |
//! | `Arg(n)`       | `x := p{n}`          | `p{n} := x`           | `p{n}.f`                         |
//! | `Field(b, g)`  | `x := load b.g`      | `store b.g := x`      | `t := load b.g`, then `t.f`      |
//! | `Static(g)`    | `x := load global.g` | `store global.g := x` | `t := load global.g`, then `t.f` |
//! | `Element(b,_)` | `x := load b.[]`     | `store b.[] := x`     | `t := load b.[]`, then `t.f`     |
//!
//! # Tracing addresses
//!
//! The flow layer's `MethodFlow::resolve_address` names a place by the variables that held its
//! base and indices, and forgets the place when one of them is written. Two writes that are
//! routine in compiled C# make it forget:
//!
//! - `ldelema` and `ldflda` push their address over their base's own stack slot, so
//!   `ldfld Arr; ldarg i; ldelema` names a base no variable holds any more;
//! - `a[i*3] += x` is `ldelema; dup; ldind; ...; stind`, and the `dup` overwrites the
//!   computed index's slot.
//!
//! Either way the address would escape resolution, and the fallback (a cell holding a copy of
//! the place) loses the write. Most addresses that escape the layer's analysis in compiled C#
//! are these, not real joins of two addresses.
//!
//! So this module runs the same must-analysis ([`Addresses`]) with two changes the frontend is
//! free to make and the layer is not. Element indices are not part of a place, since the
//! frontend's policy is that an index does not taint the element. And a base held in a
//! stack slot is saved into a frontend local, `ab{block}_{index}`, by the instruction that
//! takes the address, so the place names a variable nothing overwrites. A place is then an
//! [`Addr`]: a root (an argument, a local, the global heap or a saved base) and the fields
//! from it, which can go through another address (`ldloca r; ldflda Lo; ldfld X` is
//! `loc_r.Lo.X`). Everything the layer resolves, this resolves too, and the two are checked
//! against each other as methods are lowered.
//!
//! An address used as a *value* (an argument, a returned `ref`, a stored pointer) is
//! materialized when it resolves: `c := read P; store cell.D := c`, and `cell` is used in its
//! place. A call copies the cell back out afterwards, `u := load cell.D`, then writes `u` into
//! `P`. That works because SSA writes every parameter back at its entry version: the callee's
//! `store p.D := v` on its parameter's entry version comes back to the caller's `cell`. An unresolved address argument is passed
//! as is: it is already a pointer variable, and writes through its `D` come back to it.
//!
//! An address taken where it may reach a use that does not resolve (two addresses merged at a
//! join, as in C#'s `ref (c ? ref a : ref b)`) is materialized into its own variable when it
//! is taken. Reads through the merged pointer see both places; writes through it reach only
//! the cell, a known false negative counted as `unresolved_address_writes`.

use std::collections::VecDeque;

use cil_reader::MetadataToken;
use cil_reader::flow::{
    BoundIns, Control, Effect, InsId, MethodFlow, Place, StackKind, Transfer, Var,
};
use ctadl_ir::mir::{DEREF_FIELD, FieldRef};
use ctadl_ir::*;
use hashbrown::{HashMap, HashSet};

use crate::method::{Emitter, Lower};
use crate::names::ELEMENT;

/// `D`: the dereference field shared with C, pcode, the index and the query.
pub fn deref() -> FieldRef {
    FieldRef::symbol(DEREF_FIELD)
}

/// What a traced address's place starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Root {
    /// An argument or a local.
    Var(Var),
    /// The static-field base.
    Global,
    /// The base the `AddressOf` at this instruction read from a stack slot, saved into
    /// `ab{block}_{index}` as it took the address.
    Saved(InsId),
}

/// One step from a place's root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Field(MetadataToken),
    Element,
    /// Through an address the root (or the place so far) holds.
    Deref,
}

/// A traced place: the root and the steps from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Addr {
    pub root: Root,
    pub steps: Vec<Step>,
}

impl Addr {
    fn with(&self, s: Step) -> Addr {
        let mut steps = self.steps.clone();
        steps.push(s);
        Addr {
            root: self.root,
            steps,
        }
    }
}

/// The name of the local an `AddressOf` saves its base into.
pub fn saved_base_name(at: InsId) -> String {
    format!("ab{}_{}", at.block, at.index)
}

/// The frontend's address analysis (see the module docs): for each instruction input, the
/// place it points at when every path agrees; and the `AddressOf`s whose address may reach an
/// input that does not resolve.
#[derive(Debug, Default)]
pub struct Addresses {
    inputs: HashMap<InsId, Vec<Option<Addr>>>,
    /// The `AddressOf` instructions whose base is saved (`Root::Saved`).
    pub saves_base: HashSet<InsId>,
    /// The `AddressOf` instructions whose address may reach an input that does not resolve.
    pub escaping: HashSet<InsId>,
    /// The place each `AddressOf` instruction takes the address of.
    pub formed: HashMap<InsId, Addr>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    /// Must: the variable holds the address of this place.
    addr: HashMap<Var, Addr>,
    /// Must: the stack variable holds the same value as this argument or local.
    copy: HashMap<Var, Var>,
    /// May: the `AddressOf`s whose address the variable may hold.
    may: HashMap<Var, HashSet<InsId>>,
}

impl State {
    fn kill(&mut self, v: Var) {
        self.addr.remove(&v);
        self.copy.remove(&v);
        self.may.remove(&v);
        self.addr.retain(|_, a| a.root != Root::Var(v));
        self.copy.retain(|_, c| *c != v);
    }

    fn kill_saved(&mut self, at: InsId) {
        self.addr.retain(|_, a| a.root != Root::Saved(at));
    }

    fn clear_stack(&mut self) {
        let is_stack = |v: &Var| matches!(v, Var::Stack(_) | Var::Temp(_));
        self.addr.retain(|v, _| !is_stack(v));
        self.copy.retain(|v, _| !is_stack(v));
        self.may.retain(|v, _| !is_stack(v));
    }

    fn meet(&self, other: &State) -> State {
        let mut may = self.may.clone();
        for (v, s) in &other.may {
            may.entry(*v).or_default().extend(s.iter().copied());
        }
        State {
            addr: self
                .addr
                .iter()
                .filter(|(v, a)| other.addr.get(*v) == Some(*a))
                .map(|(v, a)| (*v, a.clone()))
                .collect(),
            copy: self
                .copy
                .iter()
                .filter(|(v, c)| other.copy.get(*v) == Some(*c))
                .map(|(v, c)| (*v, *c))
                .collect(),
            may,
        }
    }
}

/// What the analysis needs to know about the method's variables.
pub struct VarInfo<'a> {
    /// Whether `Arg(n)` holds an address.
    pub ptr_args: &'a [bool],
    /// Whether `Local(n)` holds an address.
    pub ptr_locals: &'a [bool],
}

impl VarInfo<'_> {
    pub fn holds_address(&self, v: Var) -> bool {
        match v {
            Var::Arg(n) => self.ptr_args.get(n as usize).copied().unwrap_or(false),
            Var::Local(n) => self.ptr_locals.get(n as usize).copied().unwrap_or(false),
            Var::Stack(_) | Var::Temp(_) | Var::Caught(_) | Var::Thrown => false,
        }
    }
}

impl Addresses {
    pub fn compute(flow: &MethodFlow, vars: &VarInfo<'_>) -> Addresses {
        let mut out = Addresses::default();
        let n = flow.blocks.len();
        let mut entry: Vec<Option<State>> = vec![None; n];
        let mut queue = VecDeque::new();
        for (b, block) in flow.blocks.iter().enumerate() {
            if b == 0 || block.handler_entry.is_some() || block.preds.is_empty() {
                entry[b] = Some(State::default());
                queue.push_back(b);
            }
        }
        while let Some(b) = queue.pop_front() {
            let mut state = entry[b].clone().unwrap_or_default();
            for (index, ins) in flow.blocks[b].instrs.iter().enumerate() {
                Self::step(&mut state, InsId { block: b, index }, ins, vars, None);
            }
            if ends_stack(flow, b) {
                state.clear_stack();
            }
            for &s in &flow.blocks[b].succs {
                let new = match &entry[s] {
                    None => state.clone(),
                    Some(old) => old.meet(&state),
                };
                if entry[s].as_ref() != Some(&new) {
                    entry[s] = Some(new);
                    if !queue.contains(&s) {
                        queue.push_back(s);
                    }
                }
            }
        }
        for (b, block) in flow.blocks.iter().enumerate() {
            let mut state = entry[b].clone().unwrap_or_default();
            for (index, ins) in block.instrs.iter().enumerate() {
                let at = InsId { block: b, index };
                let mut resolved = Vec::with_capacity(ins.inputs.len());
                for v in &ins.inputs {
                    let a = state.addr.get(v).cloned();
                    if a.is_none()
                        && let Some(sites) = state.may.get(v)
                    {
                        out.escaping.extend(sites.iter().copied());
                    }
                    resolved.push(a);
                }
                out.inputs.insert(at, resolved);
                Self::step(&mut state, at, ins, vars, Some(&mut out));
            }
        }
        out
    }

    /// The place input `i` of instruction `at` points at.
    pub fn input(&self, at: InsId, i: usize) -> Option<&Addr> {
        self.inputs.get(&at)?.get(i)?.as_ref()
    }

    /// The place an `AddressOf` of `ins` takes, from the state before it.
    fn formed(
        state: &State,
        at: InsId,
        ins: &BoundIns,
        place: &Place<Var>,
        vars: &VarInfo<'_>,
    ) -> Addr {
        let through = |b: Var, s: Step| -> Addr {
            if let Some(a) = state.addr.get(&b) {
                return a.with(s);
            }
            let base = match b {
                Var::Arg(_) | Var::Local(_) => Some(b),
                _ => state.copy.get(&b).copied(),
            };
            let pointer = match base {
                Some(c) => vars.holds_address(c),
                None => ins
                    .inputs
                    .iter()
                    .position(|x| *x == b)
                    .and_then(|i| ins.input_kinds.get(i))
                    .is_some_and(|k| matches!(k, StackKind::ManagedPtr | StackKind::NativeInt)),
            };
            let root = match base {
                Some(c) => Root::Var(c),
                None => Root::Saved(at),
            };
            let steps = if pointer {
                vec![Step::Deref, s]
            } else {
                vec![s]
            };
            Addr { root, steps }
        };
        match place {
            Place::Local(n) => Addr {
                root: Root::Var(Var::Local(*n)),
                steps: Vec::new(),
            },
            Place::Arg(n) => Addr {
                root: Root::Var(Var::Arg(*n)),
                steps: Vec::new(),
            },
            Place::Static(g) => Addr {
                root: Root::Global,
                steps: vec![Step::Field(g.token)],
            },
            Place::Field(b, g) => through(*b, Step::Field(g.token)),
            Place::Element(b, _) => through(*b, Step::Element),
        }
    }

    fn step(
        state: &mut State,
        at: InsId,
        ins: &BoundIns,
        vars: &VarInfo<'_>,
        mut record: Option<&mut Addresses>,
    ) {
        for e in &ins.effects {
            match e {
                Effect::AddressOf { dst, place } => {
                    let a = Self::formed(state, at, ins, place, vars);
                    if let Some(out) = record.as_deref_mut() {
                        if a.root == Root::Saved(at) {
                            out.saves_base.insert(at);
                        }
                        out.formed.insert(at, a.clone());
                    }
                    state.kill_saved(at);
                    state.kill(*dst);
                    if a.root != Root::Var(*dst) {
                        state.addr.insert(*dst, a);
                    }
                    state.may.insert(*dst, HashSet::from([at]));
                }
                Effect::Assign {
                    dst,
                    srcs,
                    transfer: Transfer::Copy,
                } if srcs.len() == 1 => {
                    let src = srcs[0];
                    if *dst == src {
                        continue;
                    }
                    let a = state.addr.get(&src).cloned();
                    let may = state.may.get(&src).cloned();
                    let canonical = match src {
                        Var::Arg(_) | Var::Local(_) => Some(src),
                        _ => state.copy.get(&src).copied(),
                    };
                    state.kill(*dst);
                    if let Some(a) = a
                        && a.root != Root::Var(*dst)
                    {
                        state.addr.insert(*dst, a);
                    }
                    if let Some(m) = may {
                        state.may.insert(*dst, m);
                    }
                    if let (Var::Stack(_) | Var::Temp(_), Some(c)) = (dst, canonical)
                        && c != *dst
                    {
                        state.copy.insert(*dst, c);
                    }
                }
                _ => {
                    let mut written = Vec::new();
                    e.for_each_write(|v| written.push(*v));
                    for v in written {
                        state.kill(v);
                    }
                }
            }
        }
    }
}

/// Whether block `b` ends with the stack emptied (`leave`, `endfinally`, `endfilter`).
fn ends_stack(flow: &MethodFlow, b: usize) -> bool {
    matches!(
        flow.blocks[b].instrs.last().map(|i| &i.flow.control),
        Some(Control::Leave(_) | Control::EndFinally | Control::EndFilter)
    )
}

/// A place as a path the IR can read and write: `root.fields[0].fields[1]...`.
#[derive(Debug, Clone)]
pub struct PlacePath {
    pub root: VariableRef,
    pub fields: Vec<FieldRef>,
}

impl PlacePath {
    pub fn with(&self, f: FieldRef) -> PlacePath {
        let mut fields = self.fields.clone();
        fields.push(f);
        PlacePath {
            root: self.root.clone(),
            fields,
        }
    }
}

impl Lower<'_, '_, '_, '_, '_> {
    /// Whether variable `v` is an argument or local that holds an address.
    pub fn var_holds_address(&self, v: Var) -> bool {
        VarInfo {
            ptr_args: &self.ptr_args,
            ptr_locals: &self.ptr_locals,
        }
        .holds_address(v)
    }

    /// The position of `v` among `ins`'s inputs.
    pub fn input_index(ins: &BoundIns, v: Var) -> Option<usize> {
        ins.inputs.iter().position(|x| *x == v)
    }

    /// The place input `v` of the current instruction points at, when it can be traced.
    pub fn resolved(&self, ins: &BoundIns, v: Var) -> Option<Addr> {
        let i = Self::input_index(ins, v)?;
        self.addrs.input(self.cur, i).cloned()
    }

    /// Whether input `v` of the current instruction is a pointer for field access: it holds
    /// a traced address, or its kind is a managed or unmanaged pointer.
    pub fn input_is_pointer(&self, ins: &BoundIns, v: Var) -> bool {
        let Some(i) = Self::input_index(ins, v) else {
            return false;
        };
        self.addrs.input(self.cur, i).is_some()
            || matches!(
                ins.input_kinds.get(i),
                Some(StackKind::ManagedPtr | StackKind::NativeInt)
            )
    }

    /// The path of a traced place.
    pub fn place_path(&self, em: &mut Emitter<'_>, a: &Addr) -> PlacePath {
        let root = match a.root {
            Root::Var(v) => em.var(v),
            Root::Global => VariableRef::new_global(),
            Root::Saved(at) => em.local(&saved_base_name(at)),
        };
        let fields = a
            .steps
            .iter()
            .map(|s| match s {
                Step::Field(t) => self.field_ref(*t),
                Step::Element => FieldRef::symbol(ELEMENT),
                Step::Deref => deref(),
            })
            .collect();
        PlacePath { root, fields }
    }

    /// The field symbol of a field token, `<cls::name>`.
    pub fn field_ref(&self, token: MetadataToken) -> FieldRef {
        FieldRef::symbol(
            self.names.field_symbol(token).unwrap_or_else(|| {
                format!("<{}::?{:08x}>", crate::names::UNKNOWN_CLASS, token.raw())
            }),
        )
    }

    /// Loads the path's prefix (all but its last field) into a chain of temporaries,
    /// returning the variable the last field hangs off.
    fn prefix(&self, em: &mut Emitter<'_>, p: &PlacePath) -> VariableRef {
        let mut base = p.root.clone();
        for f in &p.fields[..p.fields.len().saturating_sub(1)] {
            let t = em.fresh("pt");
            em.load(t.clone(), base, f.clone());
            base = t;
        }
        base
    }

    /// `x := P`.
    pub fn read(&self, em: &mut Emitter<'_>, p: &PlacePath, x: VariableRef) {
        match p.fields.last() {
            None => em.assign(x, [Exp::Variable(p.root.clone())]),
            Some(last) => {
                let last = last.clone();
                let base = self.prefix(em, p);
                em.load(x, base, last);
            }
        }
    }

    /// `P := x`.
    pub fn write(&self, em: &mut Emitter<'_>, p: &PlacePath, x: Exp) {
        match p.fields.last() {
            None => em.assign(p.root.clone(), [x]),
            Some(last) => {
                let last = last.clone();
                let base = self.prefix(em, p);
                em.store(base, last, x);
            }
        }
    }

    /// A cell holding a copy of `P`: `c := read P; store cell.D := c`.
    pub fn materialize(&self, em: &mut Emitter<'_>, p: &PlacePath, cell: VariableRef) {
        let c = em.fresh("c");
        self.read(em, p, c.clone());
        em.store(cell, deref(), Exp::Variable(c));
    }

    /// The value of `v`, an operand of `ins`, used as a value rather than dereferenced: the
    /// variable itself, or a materialized cell when it holds a traced address.
    pub fn value(&mut self, em: &mut Emitter<'_>, ins: &BoundIns, v: Var) -> Exp {
        if let Some(a) = self.resolved(ins, v) {
            let p = self.place_path(em, &a);
            let cell = em.fresh("cell");
            self.materialize(em, &p, cell.clone());
            return Exp::Variable(cell);
        }
        em.exp(v)
    }

    /// Passes an operand that may hold a traced address to a call: a cell holding a copy of
    /// the place, with the place to copy back to after the call. Untraced, the operand is
    /// passed as it is: if it is a pointer, the callee's writes through `D` come back to it.
    pub fn copy_in(
        &mut self,
        em: &mut Emitter<'_>,
        ins: &BoundIns,
        v: Var,
        copy_outs: &mut Vec<(VariableRef, PlacePath)>,
    ) -> VariableRef {
        if let Some(a) = self.resolved(ins, v) {
            let p = self.place_path(em, &a);
            return self.copy_in_place(em, p, copy_outs);
        }
        em.var(v)
    }

    /// [`Self::copy_in`] for a place the frontend knows: a cell holding its copy.
    pub fn copy_in_place(
        &mut self,
        em: &mut Emitter<'_>,
        p: PlacePath,
        copy_outs: &mut Vec<(VariableRef, PlacePath)>,
    ) -> VariableRef {
        let cell = em.fresh("cell");
        self.materialize(em, &p, cell.clone());
        copy_outs.push((cell.clone(), p));
        cell
    }

    /// After a call: `u := load cell.D; P := u` for each copied-in place.
    pub fn copy_out(&mut self, em: &mut Emitter<'_>, copy_outs: Vec<(VariableRef, PlacePath)>) {
        for (cell, p) in copy_outs {
            let u = em.fresh("u");
            em.load(u.clone(), cell, deref());
            self.write(em, &p, Exp::Variable(u));
        }
    }

    /// Checks this analysis against the flow layer's: every input the layer resolves, this
    /// resolves too. A disagreement is counted, not fatal.
    pub fn cross_check(&mut self, at: InsId, ins: &BoundIns) {
        for (i, place) in ins.input_places.iter().enumerate() {
            if place.is_some() && self.addrs.input(at, i).is_none() {
                self.out.stats.address_disagreements += 1;
                log::warn!(
                    "cil: the flow layer resolves input {i} of IL_{:04x} to {}, the frontend \
                     does not",
                    ins.flow.offset,
                    place.as_ref().unwrap()
                );
            }
        }
    }
}
