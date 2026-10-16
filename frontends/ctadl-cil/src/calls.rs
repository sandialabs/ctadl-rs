//! Calls (translation §7). Each call is one `CallAssign { style, rets: [retval, throwval],
//! args }`, with copy-in/copy-out around it for operands that hold a resolved address (§6.3),
//! `excout := excout, throwval` after it when its exception may leave the method (§8,
//! `method.rs`), and `site.result := retval`.
//!
//! | `CallKind`                        | `args`             | `style`                                 |
//! | --------------------------------- | ------------------ | --------------------------------------- |
//! | `Static`                          | `site.args`        | `DirectCall { [fid(m)] }`               |
//! | `Instance`                        | `[recv] ++ args`   | `DirectCall { [fid(m)] }`               |
//! | `Ctor`                            | `[recv] ++ args`   | `DirectCall { [fid(m)] }`               |
//! | `Virtual { constrained: None }`   | `site.args`        | `CilCall { recv, cls, simple, desc }`   |
//! | `Virtual { constrained: Some(T) }`| as above           | `CilCall { cls: T, .. }` if `T` declares or inherits `m`; else unconstrained |
//! | `Indirect { callee }`             | `[recv?] ++ args`  | `FuncPtrCall { callee, desc(sig) }`     |
//!
//! `fid(m)` is the method a `call` reaches: the definition the token resolves to in loaded
//! metadata, which for a method inherited through the named class is the base's. A `callvirt`
//! of a method that is not virtual (C# emits `callvirt` for the null check), or that is
//! `final`, or whose declaring type is sealed, is a `DirectCall`; so is a constrained call on
//! a value type, whose implementation the metadata names.
//!
//! The receiver of a value type's method is an address, and is copied in and out like any
//! other: a resolved `&loc` by its place, and the fresh struct of a value-type `newobj`
//! through the `New`'s variable (the doc's table passes it as is, but the constructor writes
//! through `this.D`).
//!
//! Delegates, after [`delegate::classify`]:
//!
//! | Classified as                     | Lowering                                                 |
//! | --------------------------------- | -------------------------------------------------------- |
//! | `Ctor { target, fnptr }`          | `recv := target`, `store recv.<delegate::method> := fnptr`, then the ctor call |
//! | `Invoke`                          | `m := load recv.<delegate::method>`, `FuncPtrCall { m }` with `[recv] ++ args` |
//!
//! Two departures from the doc's table. First, the delegate *is* its target, where the doc
//! stores the target in a field `<delegate::target>` and passes that field's value at the
//! call. A closure's captured variables are fields of its target (the display class), and
//! the bound method reads them as `this.x`; through a field, the caller's flow would need the
//! path `.<delegate::target>.x`, which no statement spells and the index does not admit
//! (`path_composition_store.tnt`), so a closure's captured value was lost. With the delegate
//! being the target, `this.x` is the target's own `x`, and a lambda's writes to captured
//! variables reach the method that captured them. Two delegates over one target share its
//! `<delegate::method>`, which only adds call targets.
//!
//! Second, the doc passes the target as argument 0 to every bound method, "an extra the
//! callee ignores" for a static one. Arguments are positional, so that would hand a static
//! method the target as its first parameter. A delegate bound to a static method stores a
//! pointer to a thunk instead, `ctadl$delegate$<fid>`, whose parameter 0 is the ignored
//! target and which calls the method with the rest.

use cil_reader::Entity;
use cil_reader::flow::delegate::{self, DelegateOp, ModuleTypes};
use cil_reader::flow::{BoundIns, CallKind, CallSite, InsId, MethodOp, TypeOp, Var};
use cil_reader::model::{ResolvedMember, TypeSig};
use cil_reader::token::TokenType;
use ctadl_ir::call::CallObject;
use ctadl_ir::mir::FieldRef;
use ctadl_ir::*;

use crate::hierarchy::resolve_type;
use crate::method::{Emitter, Lower, parameters};
use crate::names::{DELEGATE_METHOD, MethodKey, ModNames};
use crate::ptr::PlacePath;

/// How a `callvirt` lowers.
enum Virtual {
    Dispatch(CallStyle),
    /// A direct call to this method, the receiver first among the arguments.
    Direct(MethodKey, Vec<ParameterType>),
}

/// What a call resolves to in loaded metadata.
struct Target {
    key: MethodKey,
    params: Vec<ParameterType>,
    is_virtual: bool,
    is_final: bool,
    sealed: bool,
}

impl Lower<'_, '_, '_, '_, '_> {
    pub fn call(&mut self, em: &mut Emitter<'_>, at: InsId, ins: &BoundIns, site: &CallSite<Var>) {
        let oracle = ModuleTypes::with_cache(self.module, self.namer.cache());
        match delegate::classify(site, &oracle) {
            Some(DelegateOp::Invoke) => return self.delegate_invoke(em, at, ins, site),
            Some(DelegateOp::Ctor { target, fnptr }) => {
                if let Some(recv) = site.receiver {
                    let r = em.var(recv);
                    let t = self.value(em, ins, target);
                    em.assign(r.clone(), [t]);
                    let f = match self.static_fnptrs.get(&fnptr).cloned() {
                        Some(fid) => {
                            let thunk = self.out.thunk(&fid);
                            Exp::new_object_ref(CallObject::FunctionPtr(thunk.as_str().into()))
                        }
                        None => em.exp(fnptr),
                    };
                    em.store(r, FieldRef::symbol(DELEGATE_METHOD), f);
                }
            }
            None => {}
        }
        self.ordinary_call(em, ins, site);
    }

    fn ordinary_call(&mut self, em: &mut Emitter<'_>, ins: &BoundIns, site: &CallSite<Var>) {
        let mut copy_outs = Vec::new();
        let recv = site.receiver.map(|r| {
            if Some(r) == self.new_value {
                let p = PlacePath {
                    root: em.var(r),
                    fields: Vec::new(),
                };
                self.copy_in_place(em, p, &mut copy_outs)
            } else {
                self.copy_in(em, ins, r, &mut copy_outs)
            }
        });
        let mut args: ThinVec<Exp> = site
            .args
            .iter()
            .map(|a| Exp::Variable(self.copy_in(em, ins, *a, &mut copy_outs)))
            .collect();
        let target = site.method.as_ref().and_then(|m| self.target(m));
        let direct = |this: &mut Self, key: &MethodKey, params: Vec<ParameterType>| {
            let fid = this.out.callee(key, params);
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(thin_vec![fid]),
            }
        };
        let none = || CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(ThinVec::new()),
        };
        let style = match &site.kind {
            CallKind::Static => match &target {
                Some(t) => direct(self, &t.key, t.params.clone()),
                None => none(),
            },
            CallKind::Instance | CallKind::Ctor => {
                if let Some(r) = recv.clone() {
                    args.insert(0, Exp::Variable(r));
                }
                match &target {
                    Some(t) => direct(self, &t.key, t.params.clone()),
                    None => none(),
                }
            }
            CallKind::Virtual { constrained } => {
                match (site.method.as_ref(), &target, recv.clone()) {
                    (Some(m), Some(t), Some(r)) => {
                        match self.virtual_style(m, t, *constrained, r.clone()) {
                            Virtual::Dispatch(style) => style,
                            Virtual::Direct(key, params) => {
                                args.insert(0, Exp::Variable(r));
                                direct(self, &key, params)
                            }
                        }
                    }
                    (_, _, r) => {
                        if let Some(r) = r {
                            args.insert(0, Exp::Variable(r));
                        }
                        none()
                    }
                }
            }
            CallKind::Indirect { callee } => {
                if let Some(r) = recv.clone() {
                    args.insert(0, Exp::Variable(r));
                }
                CallStyle::FuncPtrCall {
                    callee: AccessPath::without_fields(em.var(*callee)),
                    signature: Some(self.names.descriptor(&site.sig, None)),
                }
            }
        };
        self.emit_call(em, style, args, site.result, copy_outs);
    }

    fn emit_call(
        &mut self,
        em: &mut Emitter<'_>,
        style: CallStyle,
        args: ThinVec<Exp>,
        result: Option<Var>,
        copy_outs: Vec<(VariableRef, PlacePath)>,
    ) {
        let retval = em.retval();
        let thrown = em.throwval();
        em.push(StatementKind::CallAssign {
            style,
            rets: thin_vec![retval.clone(), thrown],
            args,
        });
        self.raise_out(em);
        if let Some(r) = result {
            let d = em.var(r);
            em.assign(d, [Exp::Variable(retval)]);
        }
        self.copy_out(em, copy_outs);
    }

    /// How a `callvirt` of `m`, which resolves to `t`, lowers.
    fn virtual_style(
        &mut self,
        m: &MethodOp,
        t: &Target,
        constrained: Option<TypeOp>,
        recv: VariableRef,
    ) -> Virtual {
        if let Some(c) = constrained
            && let Some(found) = resolve_type(self.namer, self.module, c.token, None)
            && let Some(cls) = self.names.token_cls(c.token)
        {
            let name = t.key.simple.split("``").next().unwrap_or(&t.key.simple);
            let inst_desc = self.instantiated_desc(m);
            if let Some(imp) = found.find_implementation(self.namer, name, &inst_desc) {
                if found.is_sealed() {
                    let key = MethodKey {
                        cls: imp.cls.clone(),
                        simple: imp.simple.clone(),
                        desc: imp.own_desc.clone(),
                    };
                    return Virtual::Direct(key, t.params.clone());
                }
                return Virtual::Dispatch(CallStyle::CilCall {
                    receiver: recv,
                    cls: cls.as_str().into(),
                    simple_name: imp.simple.as_str().into(),
                    descriptor: imp.own_desc.as_str().into(),
                });
            }
        }
        if !t.is_virtual || t.is_final || t.sealed {
            return Virtual::Direct(t.key.clone(), t.params.clone());
        }
        // CHA keys on the method as the call site names it, not on the definition it
        // resolves to: an override alias or an inherited row makes the two meet.
        let key = self
            .names
            .method_key(m.token)
            .unwrap_or_else(|| t.key.clone());
        self.out.callee(&key, parameters(&m.sig));
        Virtual::Dispatch(CallStyle::CilCall {
            receiver: recv,
            cls: key.cls.as_str().into(),
            simple_name: key.simple.as_str().into(),
            descriptor: key.desc.as_str().into(),
        })
    }

    /// The method's descriptor with its declaring type's generic arguments substituted: what
    /// a constrained call compares implementations against.
    fn instantiated_desc(&self, m: &MethodOp) -> String {
        let mut token = m.token;
        if let Ok(Some(Entity::MethodSpec(ms))) = self.module.lookup_token(token)
            && let Ok(inner) = ms.method_token()
        {
            token = inner;
        }
        let vars: Option<Vec<String>> = match self.module.lookup_token(token) {
            Ok(Some(Entity::MemberRef(mr))) => mr.class_token().ok().and_then(|c| {
                if c.token_type() != Some(TokenType::TypeSpec) {
                    return None;
                }
                match self.module.lookup_token(c).ok()?? {
                    Entity::TypeSpec(ts) => match ts.signature().ok()? {
                        TypeSig::GenericInst { args, .. } => {
                            Some(args.iter().map(|a| self.names.spell(a, None)).collect())
                        }
                        _ => None,
                    },
                    _ => None,
                }
            }),
            _ => None,
        };
        self.names.descriptor(&m.sig, vars.as_deref())
    }

    /// What `m` resolves to in loaded metadata, else its key as the token names it.
    fn target(&mut self, m: &MethodOp) -> Option<Target> {
        let mut token = m.token;
        if let Ok(Some(Entity::MethodSpec(ms))) = self.module.lookup_token(token)
            && let Ok(inner) = ms.method_token()
        {
            token = inner;
        }
        let resolved = self
            .module
            .resolve_member(token, self.namer.cache())
            .ok()
            .flatten();
        if let Some(ResolvedMember::Method(module, md)) = resolved {
            let names = ModNames::new(module, self.namer);
            if let (Some(key), Ok(sig)) = (names.method_key(md.token()), md.signature()) {
                let sealed = md.declaring_type().ok().flatten().is_some_and(|t| {
                    t.is_sealed().unwrap_or(false) || t.is_value_type().unwrap_or(false)
                });
                return Some(Target {
                    key,
                    params: parameters(&sig),
                    is_virtual: md.is_virtual().unwrap_or(false),
                    is_final: md.is_final().unwrap_or(false),
                    sealed,
                });
            }
        }
        // Not in loaded metadata: the method as the token names it. Calls through a virtual
        // stay virtual; the stub stands for the method.
        let key = self.names.method_key(m.token)?;
        Some(Target {
            key,
            params: parameters(&m.sig),
            is_virtual: true,
            is_final: false,
            sealed: false,
        })
    }

    /// `callvirt D::Invoke(args)`: a call through the function pointer the delegate holds,
    /// with the delegate, which is its target, as argument 0.
    fn delegate_invoke(
        &mut self,
        em: &mut Emitter<'_>,
        _at: InsId,
        ins: &BoundIns,
        site: &CallSite<Var>,
    ) {
        let Some(recv) = site.receiver else {
            return self.ordinary_call(em, ins, site);
        };
        let r = em.var(recv);
        let m = em.fresh("dm");
        em.load(m.clone(), r.clone(), FieldRef::symbol(DELEGATE_METHOD));
        let mut copy_outs = Vec::new();
        let mut args: ThinVec<Exp> = thin_vec![Exp::Variable(r)];
        for a in &site.args {
            args.push(Exp::Variable(self.copy_in(em, ins, *a, &mut copy_outs)));
        }
        let style = CallStyle::FuncPtrCall {
            callee: AccessPath::without_fields(m),
            signature: Some(self.names.descriptor(&site.sig, None)),
        };
        self.emit_call(em, style, args, site.result, copy_outs);
    }
}
