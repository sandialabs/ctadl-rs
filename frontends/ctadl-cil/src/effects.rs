//! Effects to statements. One `match` over [`Effect`] with no `_` arm, so a review compares a
//! row of the table to an arm.
//!
//! `F(field)` is the field symbol: `<cls::f>` for a named field, `[]` for an element,
//! `length`, and `DEREF_FIELD` for `Deref` and `Block`. The dereference is never spelled as a
//! string literal: `DEREF_FIELD` is the field the index and the query treat as a pointer
//! target.
//!
//! | Effect                                        | Statements                                    |
//! | --------------------------------------------- | --------------------------------------------- |
//! | `Assign { Copy \| Cast \| Unbox \| Derived }` | `dst := srcs`                                 |
//! | `Assign { Box(T) }`                           | `dst := srcs, cil<T>` (T instantiated)        |
//! | `Const { I4 \| I8 }`                          | `dst := v`                                    |
//! | `Const { R4 \| R8 }`                          | `dst := bits`                                 |
//! | `Const { Null \| SizeOf \| ArgList }`         | `dst := 0`                                    |
//! | `Const { String(tok) }`                       | `dst := "text"`                               |
//! | `Const { Token(tok) }`                        | `dst := "name of tok"`                        |
//! | `Load { Slot(b).f }`                          | `dst := load b.F(f)`, or `ptr.rs` through a pointer |
//! | `Load { Static.f }`                           | `dst := load global.F(f)`                     |
//! | `Store { Slot(b).f := src }`                  | `store b.F(f) := src`, or `ptr.rs`            |
//! | `Store { Static.f := src }`                   | `store global.F(f) := src`                    |
//! | `AddressOf { dst, place }`                    | nothing; if it escapes resolution, a cell (`ptr.rs`) |
//! | `New { Object \| ValueType, T }`              | `dst := cil<T>`                               |
//! | `New { Array, T }`                            | `dst := cil<T[]>`                             |
//! | `New { StackAlloc }`                          | `dst := 0`                                    |
//! | `FunctionPtr { dst, m }`                      | `dst := ptr<fid(m)>`                          |
//! | `Clear { addr }`                              | resolved to a local or argument: `P := ()`; else nothing |
//! | `Call(site)`                                  | `calls.rs`                                    |
//! | `Throw { src }`                               | `throwval := src`, into `excout` if it may leave the method; the terminator routes it |
//! | `Return` / `Jump`                             | terminators (`method.rs`)                     |
//! | `Test`                                        | nothing: a branch taints nothing              |
//! | `Opaque { srcs, dsts }`                       | each `d := srcs`; counted                     |
//!
//! A struct is lowered like an object: `ldloc` of a struct local is an assign and `ldfld` on
//! it a load, so a copy carries its fields' taint, and two copies of one struct alias.
//!
//! An operand that holds a resolved address is materialized wherever it is used as a value
//! (`ptr.rs`). So is a copy into a `ref` local of a resolved address, into the local's `D`,
//! so that a place through the local (`Field(loc, f)` with `loc` a `ref` local, `loc.D.f`)
//! has something to read.

use cil_reader::flow::{
    BoundIns, Const, Effect, Field, InsId, NewKind, Path, Place, Root, Transfer, Var,
};
use cil_reader::model::TypeSig;
use ctadl_ir::call::{CallObject, CilClass};
use ctadl_ir::mir::FieldRef;
use ctadl_ir::*;

use crate::method::{Emitter, Lower, parameters};
use crate::names::{ELEMENT, LENGTH};
use crate::ptr::{self, PlacePath, deref, saved_base_name};

impl Lower<'_, '_, '_, '_, '_> {
    /// Lowers one bound instruction.
    pub fn instruction(&mut self, em: &mut Emitter<'_>, at: InsId, ins: &BoundIns) {
        self.new_value = None;
        self.cur = at;
        for e in &ins.effects {
            let mut written = Vec::new();
            e.for_each_write(|v| written.push(*v));
            let copied_fnptr = match e {
                Effect::Assign {
                    srcs,
                    transfer: Transfer::Copy,
                    ..
                } if srcs.len() == 1 => self.static_fnptrs.get(&srcs[0]).cloned(),
                _ => None,
            };
            for v in &written {
                self.static_fnptrs.remove(v);
            }
            if let (Some(f), Some(d)) = (copied_fnptr, written.first()) {
                self.static_fnptrs.insert(*d, f);
            }
            self.effect(em, at, ins, e);
        }
    }

    fn effect(&mut self, em: &mut Emitter<'_>, at: InsId, ins: &BoundIns, e: &Effect<Var>) {
        match e {
            Effect::Assign {
                dst,
                srcs,
                transfer,
            } => {
                let mut sources: Vec<Exp> = Vec::with_capacity(srcs.len() + 1);
                for s in srcs {
                    sources.push(match transfer {
                        // A copy of an address is still the address: the flow layer moves
                        // the fact, and a use of the copy resolves on its own.
                        Transfer::Copy => em.exp(*s),
                        Transfer::Cast(_)
                        | Transfer::Box(_)
                        | Transfer::Unbox(_)
                        | Transfer::Derived(_) => self.value(em, ins, *s),
                    });
                }
                if let Transfer::Box(t) = transfer
                    && let Some(cls) = self.names.token_cls(t.token)
                {
                    sources.push(Exp::new_object_ref(CallObject::CilObject(CilClass(
                        cls.as_str().into(),
                    ))));
                }
                let d = em.var(*dst);
                em.assign(d.clone(), sources);
                if matches!(transfer, Transfer::Copy)
                    && self.var_holds_address(*dst)
                    && let [s] = srcs.as_slice()
                    && let Some(place) = self.resolved(ins, *s)
                {
                    let p = self.place_path(em, &place);
                    self.materialize(em, &p, d);
                }
            }
            Effect::Const { dst, value } => {
                let v = match value {
                    Const::I4(i) => Exp::new_int(*i as i64),
                    Const::I8(i) => Exp::new_int(*i),
                    Const::R4(f) => Exp::new_int(f.to_bits() as i64),
                    Const::R8(f) => Exp::new_int(f.to_bits() as i64),
                    Const::Null | Const::SizeOf(_) | Const::ArgList => Exp::new_int(0),
                    Const::String(tok) => match self.body.user_string(*tok) {
                        Ok(s) => Exp::new_str(&s),
                        Err(_) => Exp::new_str(""),
                    },
                    Const::Token(tok) => Exp::new_str(&self.names.token_name(*tok)),
                };
                let d = em.var(*dst);
                em.assign(d, [v]);
            }
            Effect::Load { dst, src } => self.load(em, ins, *dst, src),
            Effect::Store { dst, src } => {
                let value = self.value(em, ins, *src);
                self.store(em, ins, dst, value);
            }
            Effect::AddressOf { dst, place } => {
                let formed = self.addrs.formed.get(&at).cloned();
                if self.addrs.saves_base.contains(&at)
                    && let Place::Field(b, _) | Place::Element(b, _) = place
                {
                    // The place's base is in a stack slot something will overwrite (often this
                    // very instruction): keep it where the place can name it (`ptr.rs`).
                    let saved = em.local(&saved_base_name(at));
                    let v = em.exp(*b);
                    em.assign(saved, [v]);
                }
                if self.addrs.escaping.contains(&at)
                    && let Some(a) = formed
                {
                    // A use of this address does not resolve (a join of two addresses):
                    // give the pointer a cell holding the place's value, so reads through it
                    // see it. Writes through it reach only the cell.
                    self.out.stats.unresolved_address_writes += 1;
                    let p = self.place_path(em, &a);
                    let cell = em.fresh("cell");
                    self.materialize(em, &p, cell.clone());
                    let d = em.var(*dst);
                    em.assign(d, [Exp::Variable(cell)]);
                }
            }
            Effect::New {
                dst,
                kind,
                ty,
                sizes: _,
            } => {
                let v = match kind {
                    NewKind::Object | NewKind::ValueType => {
                        if *kind == NewKind::ValueType {
                            self.new_value = Some(*dst);
                        }
                        match ty.and_then(|t| self.names.token_cls(t.token)) {
                            Some(cls) => Exp::new_object_ref(CallObject::CilObject(CilClass(
                                cls.as_str().into(),
                            ))),
                            None => Exp::new_int(0),
                        }
                    }
                    NewKind::Array => match ty.map(|t| self.array_cls(t.token)) {
                        Some(cls) => Exp::new_object_ref(CallObject::CilObject(CilClass(
                            cls.as_str().into(),
                        ))),
                        None => Exp::new_int(0),
                    },
                    NewKind::StackAlloc => Exp::new_int(0),
                };
                let d = em.var(*dst);
                em.assign(d, [v]);
            }
            Effect::FunctionPtr {
                dst,
                method,
                receiver: _,
            } => {
                let d = em.var(*dst);
                match self.names.method_key(method.token) {
                    Some(key) => {
                        let fid = self.out.callee(&key, parameters(&method.sig));
                        if !method.sig.has_this {
                            self.static_fnptrs.insert(*dst, fid.clone());
                        }
                        em.assign(
                            d,
                            [Exp::new_object_ref(CallObject::FunctionPtr(
                                fid.as_str().into(),
                            ))],
                        );
                    }
                    None => em.assign(d, [Exp::new_int(0)]),
                }
            }
            Effect::Clear { addr } => {
                // A strong update where the place is a whole variable; nothing otherwise.
                if let Some(a) = self.resolved(ins, *addr)
                    && a.steps.is_empty()
                    && matches!(a.root, ptr::Root::Var(_))
                {
                    let p = self.place_path(em, &a);
                    em.assign(p.root, []);
                }
            }
            Effect::Call(site) => self.call(em, at, ins, site),
            Effect::Throw { src } => {
                let t = em.throwval();
                let v = em.exp(*src);
                em.assign(t, [v]);
                self.raise_out(em);
            }
            Effect::Return { .. } | Effect::Jump { .. } => {}
            Effect::Test { .. } => {}
            Effect::Opaque { srcs, dsts, .. } => {
                self.out.stats.opaque_effects += 1;
                let sources: Vec<Exp> = srcs.iter().map(|s| self.value(em, ins, *s)).collect();
                for d in dsts {
                    let d = em.var(*d);
                    em.assign(d, sources.iter().cloned());
                }
            }
        }
    }

    /// `F(field)`, for a field that is not a dereference.
    fn field_symbol(&self, field: &Field<Var>) -> FieldRef {
        match field {
            Field::Named(op) => self.field_ref(op.token),
            Field::Element { .. } => FieldRef::symbol(ELEMENT),
            Field::Length => FieldRef::symbol(LENGTH),
            Field::Deref | Field::Block { .. } => deref(),
        }
    }

    /// `dst := root.field`, through a pointer root as `ptr.rs` lowers it.
    fn load(&mut self, em: &mut Emitter<'_>, ins: &BoundIns, dst: Var, src: &Path<Var>) {
        let d = em.var(dst);
        let b = match src.root {
            Root::Static => {
                let f = self.field_symbol(&src.field);
                em.load(d, VariableRef::new_global(), f);
                return;
            }
            Root::Slot(b) => b,
        };
        match &src.field {
            Field::Deref | Field::Block { .. } => match self.resolved(ins, b) {
                Some(place) => {
                    let p = self.place_path(em, &place);
                    self.read(em, &p, d);
                }
                None => {
                    let base = em.var(b);
                    em.load(d, base, deref());
                }
            },
            Field::Named(_) | Field::Element { .. } | Field::Length => {
                let f = self.field_symbol(&src.field);
                match self.pointer_base(em, ins, b) {
                    Some(p) => self.read(em, &p.with(f), d),
                    None => {
                        let base = em.var(b);
                        em.load(d, base, f);
                    }
                }
            }
        }
    }

    /// `store root.field := value`, through a pointer root as `ptr.rs` lowers it.
    fn store(&mut self, em: &mut Emitter<'_>, ins: &BoundIns, dst: &Path<Var>, value: Exp) {
        let b = match dst.root {
            Root::Static => {
                let f = self.field_symbol(&dst.field);
                em.store(VariableRef::new_global(), f, value);
                return;
            }
            Root::Slot(b) => b,
        };
        match &dst.field {
            Field::Deref | Field::Block { .. } => match self.resolved(ins, b) {
                Some(place) => {
                    let p = self.place_path(em, &place);
                    self.write(em, &p, value);
                }
                None => {
                    let base = em.var(b);
                    em.store(base, deref(), value);
                }
            },
            Field::Named(_) | Field::Element { .. } | Field::Length => {
                let f = self.field_symbol(&dst.field);
                match self.pointer_base(em, ins, b) {
                    Some(p) => self.write(em, &p.with(f), value),
                    None => {
                        let base = em.var(b);
                        em.store(base, f, value);
                    }
                }
            }
        }
    }

    /// When input `b` of `ins` is a pointer, the place a field access through it starts
    /// from: the traced place, or `b.D`. `None` when `b` is an object.
    fn pointer_base(&mut self, em: &mut Emitter<'_>, ins: &BoundIns, b: Var) -> Option<PlacePath> {
        if !self.input_is_pointer(ins, b) {
            return None;
        }
        Some(match self.resolved(ins, b) {
            Some(a) => self.place_path(em, &a),
            None => PlacePath {
                root: em.var(b),
                fields: vec![deref()],
            },
        })
    }

    /// The class `newarr` or a multi-dimensional array constructor allocates: the array type
    /// itself when the token names one, else an array of the element type it names.
    fn array_cls(&self, token: cil_reader::MetadataToken) -> String {
        if let Ok(Some(cil_reader::Entity::TypeSpec(ts))) = self.module.lookup_token(token)
            && let Ok(sig) = ts.signature()
            && matches!(
                sig.unwrap_modifiers(),
                TypeSig::SzArray(_) | TypeSig::Array { .. }
            )
        {
            return self.names.spell(&sig, None);
        }
        let elem = TypeSig::Named {
            token,
            is_value_type: false,
        };
        format!("{}[]", self.names.spell(&elem, None))
    }
}
