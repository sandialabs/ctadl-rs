//! Delegate construction and invocation.
//!
//! These are not separate [`CallKind`]s: recognizing them needs the declaring
//! type's base class, which can live in another assembly. [`classify`] asks
//! a [`TypeOracle`] instead. [`ModuleTypes`] is an oracle over one module and,
//! given an [`AssemblyCache`], over every assembly it can load.

use super::operands;
use super::{CallKind, CallSite, MethodOp, TypeOp};
use crate::model::handles::{TypeDef, TypeDefOrRef};
use crate::model::module::{Entity, lookup_token};
use crate::model::resolver::AssemblyCache;
use crate::model::{MethodBody, Module, TypeSig};
use crate::token::MetadataToken;

/// What a call does with a delegate.
#[derive(Debug, Clone, PartialEq)]
pub enum DelegateOp<S> {
    /// `newobj D::.ctor(object target, native int fnptr)`: the delegate binds
    /// `fnptr` (an ldftn/ldvirtftn result) to `target`.
    Ctor { target: S, fnptr: S },
    /// `D::Invoke` (or `BeginInvoke`): calls the bound method.
    Invoke,
}

/// What [`classify`] needs to know about types beyond one call site.
pub trait TypeOracle {
    /// The type that declares `method`, as its token names it.
    fn declaring_type(&self, method: &MethodOp) -> Option<TypeOp>;
    /// The method's simple name (`.ctor`, `Invoke`).
    fn method_name(&self, method: &MethodOp) -> Option<String>;
    /// Whether `ty` is a delegate type: it derives from
    /// `System.MulticastDelegate`. `None` when that is not known.
    fn is_delegate(&self, ty: &TypeOp) -> Option<bool>;
}

/// Whether `site` constructs or invokes a delegate. When the oracle cannot
/// tell whether the declaring type is a delegate, a constructor of shape
/// `(object, native int)` still counts as one; [`ModuleTypes`] applies the
/// same shape to the constructors of a type it can see but whose base it
/// cannot resolve.
pub fn classify<S: Clone>(site: &CallSite<S>, oracle: &dyn TypeOracle) -> Option<DelegateOp<S>> {
    let method = site.method.as_ref()?;
    let name = oracle.method_name(method)?;
    let is_delegate = oracle
        .declaring_type(method)
        .and_then(|t| oracle.is_delegate(&t));
    match (&site.kind, name.as_str()) {
        (CallKind::Ctor, ".ctor") if site.args.len() == 2 => is_delegate
            .unwrap_or_else(|| is_delegate_ctor_shape(&method.sig.params))
            .then(|| DelegateOp::Ctor {
                target: site.args[0].clone(),
                fnptr: site.args[1].clone(),
            }),
        (CallKind::Virtual { .. } | CallKind::Instance, "Invoke" | "BeginInvoke")
            if is_delegate == Some(true) =>
        {
            Some(DelegateOp::Invoke)
        }
        _ => None,
    }
}

/// `(object, native int)`, the shape of every delegate constructor.
fn is_delegate_ctor_shape(params: &[TypeSig]) -> bool {
    matches!(
        params,
        [a, b] if a.unwrap_modifiers() == &TypeSig::Object
            && matches!(b.unwrap_modifiers(), TypeSig::IntPtr | TypeSig::UIntPtr)
    )
}

/// A [`TypeOracle`] over a module and, with a cache, the assemblies it
/// references.
pub struct ModuleTypes<'m, 's, 'l> {
    module: &'m Module<'s>,
    cache: Option<&'m AssemblyCache<'l>>,
}

impl<'m, 's, 'l> ModuleTypes<'m, 's, 'l> {
    /// An oracle that sees only `module`.
    pub fn new(module: &'m Module<'s>) -> Self {
        ModuleTypes {
            module,
            cache: None,
        }
    }

    /// An oracle that resolves references through `cache`.
    pub fn with_cache(module: &'m Module<'s>, cache: &'m AssemblyCache<'l>) -> Self {
        ModuleTypes {
            module,
            cache: Some(cache),
        }
    }
}

impl TypeOracle for ModuleTypes<'_, '_, '_> {
    fn declaring_type(&self, method: &MethodOp) -> Option<TypeOp> {
        operands::method(self.module.m(), method.token)
            .ok()?
            .declaring
    }

    fn method_name(&self, method: &MethodOp) -> Option<String> {
        method_name(self.module, method.token, 0)
    }

    fn is_delegate(&self, ty: &TypeOp) -> Option<bool> {
        is_delegate(self.module, self.cache, ty.token, 0)
    }
}

impl TypeOracle for MethodBody<'_> {
    fn declaring_type(&self, method: &MethodOp) -> Option<TypeOp> {
        ModuleTypes::new(self.m.module()).declaring_type(method)
    }

    fn method_name(&self, method: &MethodOp) -> Option<String> {
        ModuleTypes::new(self.m.module()).method_name(method)
    }

    fn is_delegate(&self, ty: &TypeOp) -> Option<bool> {
        ModuleTypes::new(self.m.module()).is_delegate(ty)
    }
}

fn method_name(module: &Module<'_>, token: MetadataToken, depth: u32) -> Option<String> {
    Some(match lookup_token(module.m(), token).ok()?? {
        Entity::Method(d) => d.name().ok()?.into_owned(),
        Entity::MemberRef(r) => r.name().ok()?.into_owned(),
        Entity::MethodSpec(s) if depth == 0 => {
            return method_name(module, s.method_token().ok()?, 1);
        }
        _ => return None,
    })
}

/// How many base types [`is_delegate`] follows.
const MAX_BASES: u32 = 64;

fn is_delegate(
    module: &Module<'_>,
    cache: Option<&AssemblyCache<'_>>,
    token: MetadataToken,
    depth: u32,
) -> Option<bool> {
    if depth > MAX_BASES {
        return None;
    }
    let ty = TypeDefOrRef::lookup(module.m(), token)?;
    let (ns, name) = ty.type_of_name().ok()??;
    if ns == "System" && matches!(&*name, "MulticastDelegate" | "Delegate") {
        // The base classes themselves, not delegate types.
        return Some(false);
    }
    match ty {
        TypeDefOrRef::Def(def) => is_delegate_def(module, cache, def, depth),
        TypeDefOrRef::Spec(spec) => {
            let element = match spec.signature().ok()? {
                TypeSig::GenericInst { ty, .. } | TypeSig::Named { token: ty, .. } => ty,
                _ => return Some(false),
            };
            is_delegate(module, cache, element, depth + 1)
        }
        TypeDefOrRef::Ref(_) => {
            let resolved = module.resolve_type(token, cache?).ok()??;
            is_delegate_def(resolved.module, cache, resolved.ty, depth + 1)
        }
    }
}

fn is_delegate_def(
    module: &Module<'_>,
    cache: Option<&AssemblyCache<'_>>,
    def: TypeDef<'_>,
    depth: u32,
) -> Option<bool> {
    let Some(base) = def.base_type().ok()? else {
        return Some(false);
    };
    let by_base = match base.type_of_name().ok()? {
        Some((ns, name)) if ns == "System" && name == "MulticastDelegate" => Some(true),
        Some((ns, name)) if ns == "System" && matches!(&*name, "Object" | "ValueType" | "Enum") => {
            Some(false)
        }
        _ => is_delegate(module, cache, base.token(), depth + 1),
    };
    // A base that does not resolve: a type with a delegate's constructor is
    // taken for one.
    by_base.or_else(|| {
        let methods = def.methods().ok()?;
        methods
            .iter()
            .any(|m| {
                m.name().is_ok_and(|n| n == ".ctor")
                    && m.signature()
                        .is_ok_and(|s| is_delegate_ctor_shape(&s.params))
            })
            .then_some(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Prefixes, Slot};
    use crate::model::MethodSig;

    struct Oracle {
        name: &'static str,
        delegate: Option<bool>,
    }

    impl TypeOracle for Oracle {
        fn declaring_type(&self, _: &MethodOp) -> Option<TypeOp> {
            Some(TypeOp {
                token: MetadataToken(0x0200_0002),
            })
        }
        fn method_name(&self, _: &MethodOp) -> Option<String> {
            Some(self.name.to_string())
        }
        fn is_delegate(&self, _: &TypeOp) -> Option<bool> {
            self.delegate
        }
    }

    fn site(kind: CallKind<Slot>, params: Vec<TypeSig>) -> CallSite<Slot> {
        let sig = MethodSig {
            has_this: true,
            explicit_this: false,
            calling_convention: 0,
            generic_arity: 0,
            return_type: TypeSig::Void,
            params,
        };
        CallSite {
            kind,
            method: Some(MethodOp {
                token: MetadataToken(0x0600_0001),
                sig: sig.clone(),
                generic_args: vec![],
            }),
            args: (0..sig.params.len() as u8).map(Slot::In).collect(),
            sig,
            receiver: None,
            result: None,
            thrown: Slot::Thrown,
            byref_args: vec![],
            prefixes: Prefixes::default(),
        }
    }

    #[test]
    fn constructors() {
        let shaped = site(CallKind::Ctor, vec![TypeSig::Object, TypeSig::IntPtr]);
        let ctor = Some(DelegateOp::Ctor {
            target: Slot::In(0),
            fnptr: Slot::In(1),
        });
        for delegate in [Some(true), None] {
            let o = Oracle {
                name: ".ctor",
                delegate,
            };
            assert_eq!(classify(&shaped, &o), ctor, "{delegate:?}");
        }
        // Known not to be a delegate: no, whatever the shape.
        let o = Oracle {
            name: ".ctor",
            delegate: Some(false),
        };
        assert_eq!(classify(&shaped, &o), None);
        // Unknown type and another shape: no.
        let o = Oracle {
            name: ".ctor",
            delegate: None,
        };
        let other = site(CallKind::Ctor, vec![TypeSig::String, TypeSig::I4]);
        assert_eq!(classify(&other, &o), None);
    }

    #[test]
    fn invocations() {
        let call = site(CallKind::Virtual { constrained: None }, vec![TypeSig::I4]);
        for name in ["Invoke", "BeginInvoke"] {
            let o = Oracle {
                name,
                delegate: Some(true),
            };
            assert_eq!(classify(&call, &o), Some(DelegateOp::Invoke));
        }
        for (name, delegate) in [
            ("Invoke", None),
            ("Invoke", Some(false)),
            ("Run", Some(true)),
        ] {
            let o = Oracle { name, delegate };
            assert_eq!(classify(&call, &o), None, "{name} {delegate:?}");
        }
    }
}
