//! VMT rows: methods, parents, interfaces, abstract methods, override aliases and P/Invoke,
//! and the walks over loaded metadata that both the aliases and virtual-call lowering
//! (`calls.rs`) need.
//!
//! | Source                                   | Row                                       |
//! | ---------------------------------------- | ----------------------------------------- |
//! | each `TypeDef`                           | `hierarchy[cls] = [base] ++ interfaces`   |
//! | `TypeDef` with the interface flag        | `interfaces.push(cls)`                    |
//! | each method that is not `abstract`       | `methods.push((cls, simple, desc, fid))`  |
//! | each `abstract` method                   | `abstract_methods.push((cls, simple, desc))` |
//! | each `pinvoke` method                    | `pinvokes.push((.., module, entry_point))` |
//! | override aliases                         | `methods.push((cls, base simple, base desc, fid))` |
//!
//! Every non-abstract method gets a row, runtime-implemented ones (a delegate's `Invoke`)
//! included: like a Dex `native` method, a bodyless implementation still needs a row for a
//! virtual call to resolve to it.
//!
//! # Override aliases
//!
//! CHA keys on `(simple name, descriptor)`. Java gets overrides across generics for free,
//! because erasure and `javac`'s bridge methods make the descriptors match. CIL has neither:
//!
//! ```text
//! class B<T> { public virtual void M(T x) {} }                 // B`1::M(!0)->System.Void
//! class D : B<string> { public override void M(string x) {} }  // D::M(System.String)->System.Void
//! ```
//!
//! so a ``callvirt B`1<string>::M(!0)`` would never reach `D::M`. An explicit implementation
//! (`void IFoo.Bar()`, a `MethodImpl` row) has a different name from the method it
//! implements. [`aliases`] adds, under `D`'s class, a row with the *base's* key for each such
//! override, so CHA walking up from the receiver's class finds `D::M` under the key the call
//! site uses. A plain override that matches exactly makes a duplicate row, which is harmless.

use cil_reader::model::{MethodDef, MethodSig, TypeDef};
use cil_reader::token::MetadataToken;
use cil_reader::{Entity, Module};

use crate::names::{ModNames, Namer, simple_name};

/// How many base types a walk follows.
const MAX_DEPTH: usize = 64;

/// A type definition found in loaded metadata, with the generic arguments it was reached
/// with, spelled in the terms of the type the walk started from. `None` when the type is not
/// generic or was reached as an open definition.
#[derive(Clone)]
pub struct Found<'c> {
    pub module: &'c Module<'c>,
    pub td: TypeDef<'c>,
    pub args: Option<Vec<String>>,
}

/// A virtual method seen from the type a walk started at.
#[derive(Debug, Clone)]
pub struct VirtualMethod {
    /// The declaring class.
    pub cls: String,
    pub name: String,
    pub simple: String,
    /// The descriptor in the walk's root's terms: the declaring type's generic parameters
    /// replaced by the arguments it was reached with.
    pub inst_desc: String,
    /// The descriptor as the declaring type spells it: the key a call through that type uses.
    pub own_desc: String,
    pub fid: String,
    pub newslot: bool,
    pub is_abstract: bool,
}

/// Resolves a TypeDef, TypeRef or TypeSpec token of `module` to its definition. `vars` spells
/// `module`'s own `!n` in the root's terms, for the arguments of a generic instance.
pub fn resolve_type<'c>(
    namer: &Namer<'c, '_>,
    module: &'c Module<'c>,
    token: MetadataToken,
    vars: Option<&[String]>,
) -> Option<Found<'c>> {
    let names = ModNames::new(module, namer);
    let (def_token, args) = match module.lookup_token(token).ok()?? {
        Entity::TypeDef(_) | Entity::TypeRef(_) => (token, None),
        Entity::TypeSpec(ts) => match ts.signature().ok()?.unwrap_modifiers() {
            cil_reader::model::TypeSig::GenericInst { ty, args, .. } => (
                *ty,
                Some(args.iter().map(|a| names.spell(a, vars)).collect()),
            ),
            cil_reader::model::TypeSig::Named { token, .. } => (*token, None),
            _ => return None,
        },
        _ => return None,
    };
    let resolved = module.resolve_type(def_token, namer.cache()).ok()??;
    Some(Found {
        module: resolved.module,
        td: resolved.ty,
        args,
    })
}

impl<'c> Found<'c> {
    /// The base type, its generic arguments in the root's terms.
    pub fn base(&self, namer: &Namer<'c, '_>) -> Option<Found<'c>> {
        let base = self.td.base_type().ok()??;
        resolve_type(namer, self.module, base.token(), self.args.as_deref())
    }

    /// The interfaces this type lists, their generic arguments in the root's terms.
    pub fn interfaces(&self, namer: &Namer<'c, '_>) -> Vec<Found<'c>> {
        let Ok(list) = self.td.interfaces() else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|i| i.interface.as_ref())
            .filter_map(|i| resolve_type(namer, self.module, i.token(), self.args.as_deref()))
            .collect()
    }

    /// The type's virtual methods.
    pub fn virtual_methods(&self, namer: &Namer<'c, '_>) -> Vec<VirtualMethod> {
        let names = ModNames::new(self.module, namer);
        let Ok(methods) = self.td.methods() else {
            return Vec::new();
        };
        methods
            .iter()
            .filter(|m| m.is_virtual().unwrap_or(false))
            .filter_map(|m| virtual_method(&names, m, self.args.as_deref()))
            .collect()
    }

    pub fn is_sealed(&self) -> bool {
        self.td.is_sealed().unwrap_or(false) || self.td.is_value_type().unwrap_or(false)
    }

    /// The implementation a virtual call of `name` with descriptor `inst_desc` (in this
    /// type's terms) reaches on an object of exactly this type: the most derived declaration
    /// up the base chain.
    pub fn find_implementation(
        &self,
        namer: &Namer<'c, '_>,
        name: &str,
        inst_desc: &str,
    ) -> Option<VirtualMethod> {
        let mut current = Some(self.clone());
        for _ in 0..MAX_DEPTH {
            let t = current?;
            if let Some(m) = t
                .virtual_methods(namer)
                .into_iter()
                .find(|m| m.name == name && m.inst_desc == inst_desc && !m.is_abstract)
            {
                return Some(m);
            }
            current = t.base(namer);
        }
        None
    }
}

fn virtual_method(
    names: &ModNames<'_, '_, '_, '_>,
    m: &MethodDef<'_>,
    vars: Option<&[String]>,
) -> Option<VirtualMethod> {
    let name = m.name().ok()?.into_owned();
    let sig: MethodSig = m.signature().ok()?;
    let key = names.method_key(m.token())?;
    Some(VirtualMethod {
        cls: key.cls.clone(),
        simple: simple_name(&name, sig.generic_arity),
        name,
        inst_desc: names.descriptor(&sig, vars),
        own_desc: key.desc.clone(),
        fid: key.fid(),
        newslot: m.is_newslot().unwrap_or(false),
        is_abstract: m.is_abstract().unwrap_or(false),
    })
}

/// The override-alias rows of one type, as `(base simple, base desc, fid)` under the
/// type's own class. Also counts the base types and interfaces whose metadata could not be
/// read, which make no aliases.
pub struct Aliases {
    pub rows: Vec<(String, String, String)>,
    pub missing_metadata: Vec<String>,
}

/// Computes `td`'s override aliases:
///
/// 1. each virtual method that is not `newslot` against every matching virtual method up the
///    base chain, with the bases' generic arguments substituted;
/// 2. each method of each implemented interface (with their own super-interfaces) against
///    the type's virtual methods and those it inherits, by name and substituted signature;
/// 3. each `MethodImpl` row, `(declaration -> body)`.
///
/// An interface method a `MethodImpl` row covers is not matched implicitly, since an explicit
/// implementation wins over a same-named public method.
pub fn aliases<'c>(namer: &Namer<'c, '_>, module: &'c Module<'c>, td: TypeDef<'c>) -> Aliases {
    let names = ModNames::new(module, namer);
    let root = Found {
        module,
        td,
        args: None,
    };
    let mut rows = Vec::new();
    let mut missing_metadata = Vec::new();
    let own = root.virtual_methods(namer);

    // 3. MethodImpl rows, first: they decide which interface methods are already covered.
    let mut explicit: Vec<(String, String)> = Vec::new();
    if let Ok(methods) = td.methods() {
        for m in &methods {
            let Some(body) = names.method_key(m.token()) else {
                continue;
            };
            for decl in m.overrides().unwrap_or_default() {
                if let Some(key) = names.method_key(decl) {
                    explicit.push((key.simple.clone(), key.desc.clone()));
                    rows.push((key.simple, key.desc, body.fid()));
                }
            }
        }
    }

    // 1. The base chain.
    let mut chain: Vec<Found<'c>> = Vec::new();
    {
        let mut current = root.clone();
        for _ in 0..MAX_DEPTH {
            match current.td.base_type() {
                Ok(Some(base)) => match current.base(namer) {
                    Some(b) => {
                        chain.push(b.clone());
                        current = b;
                    }
                    None => {
                        missing_metadata.push(
                            ModNames::new(current.module, namer)
                                .tdor_cls(&base)
                                .unwrap_or_default(),
                        );
                        break;
                    }
                },
                _ => break,
            }
        }
    }
    let chain_methods: Vec<Vec<VirtualMethod>> =
        chain.iter().map(|b| b.virtual_methods(namer)).collect();
    for m in own.iter().filter(|m| !m.newslot) {
        for methods in &chain_methods {
            let Some(b) = methods
                .iter()
                .find(|b| b.name == m.name && b.inst_desc == m.inst_desc)
            else {
                continue;
            };
            rows.push((b.simple.clone(), b.own_desc.clone(), m.fid.clone()));
            if b.newslot {
                // `b` starts a slot of its own: `m` overrides nothing further up.
                break;
            }
        }
    }

    // 2. Interfaces, implicit implementation by name and substituted signature. The
    // implementation may be the type's own or one it inherits.
    let mut implementors: Vec<&VirtualMethod> = own.iter().collect();
    for methods in &chain_methods {
        implementors.extend(methods.iter());
    }
    let mut seen = Vec::new();
    let mut queue = root.interfaces(namer);
    for b in &chain {
        queue.extend(b.interfaces(namer));
    }
    let mut interfaces_named = 0usize;
    if let Ok(list) = td.interfaces() {
        interfaces_named = list.len();
    }
    if queue.len() < interfaces_named {
        missing_metadata.push(format!(
            "{} interface(s) of {}",
            interfaces_named - queue.len(),
            names.typedef_cls(&td)
        ));
    }
    while let Some(iface) = queue.pop() {
        let id = (
            iface.td.token(),
            iface.args.clone(),
            iface.module.name().ok().map(|n| n.into_owned()),
        );
        if seen.contains(&id) || seen.len() > 256 {
            continue;
        }
        seen.push(id);
        queue.extend(iface.interfaces(namer));
        for b in iface.virtual_methods(namer) {
            if explicit.contains(&(b.simple.clone(), b.own_desc.clone())) {
                continue;
            }
            if let Some(m) = implementors
                .iter()
                .find(|m| m.name == b.name && m.inst_desc == b.inst_desc && !m.is_abstract)
            {
                rows.push((b.simple.clone(), b.own_desc.clone(), m.fid.clone()));
            }
        }
    }

    rows.sort();
    rows.dedup();
    Aliases {
        rows,
        missing_metadata,
    }
}
