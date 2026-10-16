//! Names (translation §3): the contract between the frontend, CHA, the model files and SARIF.
//! Every spelling the frontend emits comes from here.
//!
//! | Thing       | Spelling                                  |
//! | ----------- | ----------------------------------------- |
//! | class       | `[Asm]Ns.Outer/Inner`, open definition    |
//! | simple name | metadata name, `` ``n`` for generic methods |
//! | descriptor  | `(P1,P2)->Ret`, types as in [`ModNames::spell`] |
//! | function id | `{cls}::{simple}{descriptor}`             |
//! | field       | `<{cls}::{name}>`                         |
//!
//! `Asm` is the simple assembly name, canonicalized through type forwarders: a reference to
//! `[System.Runtime]System.Object` names `[System.Private.CoreLib]System.Object` when the
//! `System.Runtime` facade can be read. The translation doc puts that rewrite in `finish`;
//! doing it when a name is built gives the same names without a second walk over the IR.

use std::cell::RefCell;

use cil_reader::model::{
    AssemblyCache, AssemblyName, MemberRefSig, MethodSig, TypeDef, TypeDefOrRef, TypeSig,
};
use cil_reader::token::{MetadataToken, TokenType};
use cil_reader::{Entity, Module};
use hashbrown::HashMap;

/// The class of a method or field the frontend cannot name: a member reference whose parent is
/// a generic parameter, say. Still a class, so a row keyed on it is well formed.
pub const UNKNOWN_CLASS: &str = "[?]?";

/// The field a delegate carries (translation §7): the function pointer `ldftn` made. A fixed
/// internal symbol, so models can name it. A delegate is its own target (`calls.rs`), so there
/// is no field for that.
pub const DELEGATE_METHOD: &str = "<delegate::method>";

/// Array element and length fields, the same symbols as Dex and JVM.
pub const ELEMENT: &str = "[]";
pub const LENGTH: &str = "length";

/// How many forwarders [`Namer::canonical_assembly`] follows before giving up on a cycle.
const MAX_FORWARDS: usize = 8;

/// The names of a method: what CHA keys on, and the function id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MethodKey {
    pub cls: String,
    pub simple: String,
    pub desc: String,
}

impl MethodKey {
    /// `{cls}::{simple}{descriptor}`.
    pub fn fid(&self) -> String {
        format!("{}::{}{}", self.cls, self.simple, self.desc)
    }
}

/// Assembly canonicalization, shared by every module of an import. Forwarders come from the
/// assemblies the cache can load: the processed ones and the metadata-only references.
pub struct Namer<'c, 'l> {
    cache: &'c AssemblyCache<'l>,
    /// `(assembly, full name)` -> the assembly the type really lives in.
    forwards: RefCell<HashMap<(String, String), String>>,
}

impl<'c, 'l> Namer<'c, 'l> {
    pub fn new(cache: &'c AssemblyCache<'l>) -> Self {
        Namer {
            cache,
            forwards: RefCell::new(HashMap::new()),
        }
    }

    pub fn cache(&self) -> &'c AssemblyCache<'l> {
        self.cache
    }

    /// The assembly `full_name` (a top-level `Ns.Type`) lives in when referenced through
    /// `asm`: `asm` itself unless `asm` forwards it.
    pub fn canonical_assembly(&self, asm: &str, full_name: &str) -> String {
        let key = (asm.to_string(), full_name.to_string());
        if let Some(found) = self.forwards.borrow().get(&key) {
            return found.clone();
        }
        let mut current = asm.to_string();
        for _ in 0..MAX_FORWARDS {
            match self.forwarded_to(&current, full_name) {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }
        self.forwards.borrow_mut().insert(key, current.clone());
        current
    }

    /// The assembly `asm` forwards `full_name` to, if it does.
    fn forwarded_to(&self, asm: &str, full_name: &str) -> Option<String> {
        let module = self.cache.assembly(&AssemblyName::named(asm))?;
        for et in module.exported_types() {
            if !et.is_forwarder().ok()? || et.declaring_type().ok()?.is_some() {
                continue;
            }
            if et.full_name().ok()? == full_name {
                return et.scope_name().ok().map(|s| s.into_owned());
            }
        }
        None
    }
}

/// The simple name of a module's assembly; for a netmodule, its file name without extension.
pub fn assembly_of(module: &Module<'_>) -> String {
    if let Ok(Some(a)) = module.assembly_name() {
        return a.name.into_owned();
    }
    let name = module.name().map(|n| n.into_owned()).unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => name,
    }
}

/// Naming within one module: tokens and signatures resolve against it.
pub struct ModNames<'n, 'm, 'c, 'l> {
    pub module: &'m Module<'m>,
    pub namer: &'n Namer<'c, 'l>,
    /// This module's simple assembly name.
    pub asm: String,
}

impl<'n, 'm, 'c, 'l> ModNames<'n, 'm, 'c, 'l> {
    pub fn new(module: &'m Module<'m>, namer: &'n Namer<'c, 'l>) -> Self {
        ModNames {
            module,
            namer,
            asm: assembly_of(module),
        }
    }

    // ---- classes -------------------------------------------------------------------------

    /// `[Asm]Ns.Outer/Inner` for a type this module defines.
    pub fn typedef_cls(&self, td: &TypeDef<'_>) -> String {
        let full = td.full_name().unwrap_or_else(|_| "?".to_string());
        format!("[{}]{full}", self.asm)
    }

    /// The class a TypeDef, TypeRef or TypeSpec token names, erased to its open definition.
    /// `None` for a generic parameter or a token that names nothing.
    pub fn token_cls(&self, token: MetadataToken) -> Option<String> {
        self.token_cls_depth(token, 0)
    }

    fn token_cls_depth(&self, token: MetadataToken, depth: u32) -> Option<String> {
        if depth > 16 {
            return None;
        }
        match self.module.lookup_token(token).ok()?? {
            Entity::TypeDef(td) => Some(self.typedef_cls(&td)),
            Entity::TypeRef(tr) => {
                let full = tr.full_name().ok()?;
                let asm = self.typeref_assembly(token)?;
                let top = full.split('/').next().unwrap_or(&full);
                let asm = self.namer.canonical_assembly(&asm, top);
                Some(format!("[{asm}]{full}"))
            }
            Entity::TypeSpec(ts) => self.sig_cls(&ts.signature().ok()?, depth + 1),
            _ => None,
        }
    }

    /// The class a signature type names, erased: a generic instance names its definition, a
    /// primitive its core-library type, and an array itself, spelled as a descriptor type
    /// with no assembly (`System.Int32[]`), which is the class `newarr` allocates.
    pub fn sig_cls(&self, sig: &TypeSig, depth: u32) -> Option<String> {
        match sig.unwrap_modifiers() {
            TypeSig::Named { token, .. } => self.token_cls_depth(*token, depth),
            TypeSig::GenericInst { ty, .. } => self.token_cls_depth(*ty, depth),
            TypeSig::Var(_) | TypeSig::MVar(_) => None,
            TypeSig::String => Some(self.core_cls("System.String")),
            TypeSig::Object => Some(self.core_cls("System.Object")),
            other => primitive_name(other)
                .map(|p| self.core_cls(p))
                .or_else(|| Some(self.spell(other, None))),
        }
    }

    /// A `System` type of the core library, as this module would reference it.
    fn core_cls(&self, full: &str) -> String {
        let asm = self.core_library();
        let asm = self.namer.canonical_assembly(&asm, full);
        format!("[{asm}]{full}")
    }

    /// The core library this module references: a definition of `System.Object` here, else
    /// the first core-library assembly reference.
    fn core_library(&self) -> String {
        const CORE: [&str; 5] = [
            "System.Private.CoreLib",
            "System.Runtime",
            "mscorlib",
            "netstandard",
            "System.Core",
        ];
        if CORE.contains(&self.asm.as_str())
            && self
                .module
                .get_type("System.Object")
                .ok()
                .flatten()
                .is_some()
        {
            return self.asm.clone();
        }
        if let Ok(refs) = self.module.assembly_references() {
            for name in CORE {
                if refs.iter().any(|r| r.name == name) {
                    return name.to_string();
                }
            }
        }
        "System.Runtime".to_string()
    }

    /// The assembly a TypeRef's resolution scope names: an AssemblyRef's name; this assembly
    /// for a ModuleRef or the module itself; for a null scope, the assembly this module's
    /// `ExportedType` table sends the type to. A nested reference takes its outer type's.
    fn typeref_assembly(&self, token: MetadataToken) -> Option<String> {
        let mut tr = self.module.type_ref(token.rid())?;
        let mut steps = 0;
        while let Some(outer) = tr.declaring_type().ok()? {
            steps += 1;
            if steps > 64 {
                return None;
            }
            tr = outer;
        }
        let scope = tr.scope_token().ok()?;
        match scope.token_type() {
            Some(TokenType::AssemblyRef) => self
                .module
                .assembly_reference(scope.rid())
                .ok()
                .map(|a| a.name.into_owned()),
            Some(TokenType::ModuleRef | TokenType::Module) => Some(self.asm.clone()),
            _ if scope.rid() == 0 => {
                let full = tr.full_name().ok()?;
                for et in self.module.exported_types() {
                    if et.full_name().ok().as_deref() == Some(full.as_str()) {
                        return et.scope_name().ok().map(|s| s.into_owned());
                    }
                }
                Some(self.asm.clone())
            }
            _ => Some(self.asm.clone()),
        }
    }

    /// The class of a TypeDefOrRef handle, erased.
    pub fn tdor_cls(&self, t: &TypeDefOrRef<'_>) -> Option<String> {
        self.token_cls(t.token())
    }

    // ---- descriptors ---------------------------------------------------------------------

    /// A type as descriptors spell it (translation §3.2): no assembly, generic parameters by
    /// position, modifiers dropped. With `vars`, each type parameter `!n` is spelled
    /// `vars[n]` instead, which is how a base type's signature is read in a subclass.
    pub fn spell(&self, sig: &TypeSig, vars: Option<&[String]>) -> String {
        let mut out = String::new();
        self.spell_into(sig, vars, &mut out, 0);
        out
    }

    fn spell_into(&self, sig: &TypeSig, vars: Option<&[String]>, out: &mut String, depth: u32) {
        if depth > 64 {
            out.push('?');
            return;
        }
        if let Some(p) = primitive_name(sig) {
            out.push_str(p);
            return;
        }
        match sig {
            TypeSig::Named { token, .. } => self.spell_token(*token, vars, out, depth),
            TypeSig::GenericInst { ty, args, .. } => {
                self.spell_token(*ty, vars, out, depth);
                out.push('<');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.spell_into(a, vars, out, depth + 1);
                }
                out.push('>');
            }
            TypeSig::Var(n) => match vars.and_then(|v| v.get(*n as usize)) {
                Some(s) => out.push_str(s),
                None => {
                    out.push('!');
                    out.push_str(&n.to_string());
                }
            },
            TypeSig::MVar(n) => {
                out.push_str("!!");
                out.push_str(&n.to_string());
            }
            TypeSig::SzArray(e) => {
                self.spell_into(e, vars, out, depth + 1);
                out.push_str("[]");
            }
            TypeSig::Array {
                element,
                dimensions,
            } => {
                self.spell_into(element, vars, out, depth + 1);
                out.push('[');
                for _ in 1..dimensions.len().max(1) {
                    out.push(',');
                }
                out.push(']');
            }
            TypeSig::ByRef(e) => {
                self.spell_into(e, vars, out, depth + 1);
                out.push('&');
            }
            TypeSig::Ptr(e) => {
                self.spell_into(e, vars, out, depth + 1);
                out.push('*');
            }
            TypeSig::FnPtr(m) => {
                out.push_str("method ");
                self.spell_into(&m.return_type, vars, out, depth + 1);
                out.push_str("*(");
                for (i, p) in m.params.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.spell_into(p, vars, out, depth + 1);
                }
                out.push(')');
            }
            TypeSig::CModReq { inner, .. }
            | TypeSig::CModOpt { inner, .. }
            | TypeSig::Pinned(inner)
            | TypeSig::Sentinel(inner) => self.spell_into(inner, vars, out, depth + 1),
            // Primitives are spelled above.
            TypeSig::Void
            | TypeSig::Boolean
            | TypeSig::Char
            | TypeSig::I1
            | TypeSig::U1
            | TypeSig::I2
            | TypeSig::U2
            | TypeSig::I4
            | TypeSig::U4
            | TypeSig::I8
            | TypeSig::U8
            | TypeSig::R4
            | TypeSig::R8
            | TypeSig::String
            | TypeSig::Object
            | TypeSig::IntPtr
            | TypeSig::UIntPtr
            | TypeSig::TypedByRef => unreachable!("primitive_name covers it"),
        }
    }

    /// `Ns.Outer/Inner` for a TypeDef or TypeRef, a TypeSpec's signature otherwise.
    fn spell_token(
        &self,
        token: MetadataToken,
        vars: Option<&[String]>,
        out: &mut String,
        depth: u32,
    ) {
        match self.module.lookup_token(token).ok().flatten() {
            Some(Entity::TypeDef(td)) => out.push_str(&td.full_name().unwrap_or_default()),
            Some(Entity::TypeRef(tr)) => out.push_str(&tr.full_name().unwrap_or_default()),
            Some(Entity::TypeSpec(ts)) => match ts.signature() {
                Ok(sig) => self.spell_into(&sig, vars, out, depth + 1),
                Err(_) => out.push('?'),
            },
            _ => out.push('?'),
        }
    }

    /// `(P1,P2)->Ret`. A vararg signature ends its parameters with `...`: a definition's
    /// has none past it, and a call site's extras after the sentinel are left out, so both
    /// spell the same descriptor.
    pub fn descriptor(&self, sig: &MethodSig, vars: Option<&[String]>) -> String {
        let mut out = String::from("(");
        let vararg = sig.calling_convention == cil_reader::model::sig::calling_convention::VAR_ARG;
        let mut first = true;
        for p in &sig.params {
            if p.is_sentinel() {
                break;
            }
            if !first {
                out.push(',');
            }
            first = false;
            self.spell_into(p, vars, &mut out, 0);
        }
        if vararg {
            if !first {
                out.push(',');
            }
            out.push_str("...");
        }
        out.push_str(")->");
        self.spell_into(&sig.return_type, vars, &mut out, 0);
        out
    }

    // ---- members -------------------------------------------------------------------------

    /// The key of a method this module names: a MethodDef, a method MemberRef, or a
    /// MethodSpec (erased to the method it instantiates).
    pub fn method_key(&self, token: MetadataToken) -> Option<MethodKey> {
        match self.module.lookup_token(token).ok()?? {
            Entity::Method(md) => {
                let cls = match md.declaring_type().ok()? {
                    Some(td) => self.typedef_cls(&td),
                    None => format!("[{}]<Module>", self.asm),
                };
                let sig = md.signature().ok()?;
                Some(MethodKey {
                    cls,
                    simple: simple_name(&md.name().ok()?, sig.generic_arity),
                    desc: self.descriptor(&sig, None),
                })
            }
            Entity::MemberRef(mr) => {
                let MemberRefSig::Method(sig) = mr.signature().ok()? else {
                    return None;
                };
                let class = mr.class_token().ok()?;
                if class.token_type() == Some(TokenType::Method) {
                    // A vararg call to a definition: the definition's key.
                    return self.method_key(class);
                }
                let cls = match class.token_type() {
                    Some(TokenType::ModuleRef) => format!("[{}]<Module>", self.asm),
                    _ => self
                        .token_cls(class)
                        .unwrap_or_else(|| UNKNOWN_CLASS.to_string()),
                };
                Some(MethodKey {
                    cls,
                    simple: simple_name(&mr.name().ok()?, sig.generic_arity),
                    desc: self.descriptor(&sig, None),
                })
            }
            Entity::MethodSpec(ms) => {
                let inner = ms.method_token().ok()?;
                if inner.token_type() == Some(TokenType::MethodSpec) {
                    return None;
                }
                self.method_key(inner)
            }
            _ => None,
        }
    }

    /// `<{cls}::{name}>` for a field definition or field MemberRef.
    pub fn field_symbol(&self, token: MetadataToken) -> Option<String> {
        match self.module.lookup_token(token).ok()?? {
            Entity::Field(fd) => {
                let cls = match fd.declaring_type().ok()? {
                    Some(td) => self.typedef_cls(&td),
                    None => format!("[{}]<Module>", self.asm),
                };
                Some(format!("<{cls}::{}>", fd.name().ok()?))
            }
            Entity::MemberRef(mr) => {
                let cls = self
                    .token_cls(mr.class_token().ok()?)
                    .unwrap_or_else(|| UNKNOWN_CLASS.to_string());
                Some(format!("<{cls}::{}>", mr.name().ok()?))
            }
            _ => None,
        }
    }

    /// What an `ldtoken` operand names: a class, a function id or a field symbol.
    pub fn token_name(&self, token: MetadataToken) -> String {
        let named = match token.token_type() {
            Some(TokenType::TypeDef | TokenType::TypeRef | TokenType::TypeSpec) => {
                self.token_cls(token)
            }
            Some(TokenType::Field) => self.field_symbol(token),
            Some(TokenType::Method | TokenType::MethodSpec) => {
                self.method_key(token).map(|k| k.fid())
            }
            Some(TokenType::MemberRef) => self
                .method_key(token)
                .map(|k| k.fid())
                .or_else(|| self.field_symbol(token)),
            _ => None,
        };
        named.unwrap_or_else(|| format!("token:{:08x}", token.raw()))
    }
}

/// The metadata name with a generic method's arity appended: `Select``2`.
pub fn simple_name(name: &str, generic_arity: u32) -> String {
    if generic_arity > 0 {
        format!("{name}``{generic_arity}")
    } else {
        name.to_string()
    }
}

/// Cecil's full names for the primitive element types.
fn primitive_name(sig: &TypeSig) -> Option<&'static str> {
    Some(match sig {
        TypeSig::Void => "System.Void",
        TypeSig::Boolean => "System.Boolean",
        TypeSig::Char => "System.Char",
        TypeSig::I1 => "System.SByte",
        TypeSig::U1 => "System.Byte",
        TypeSig::I2 => "System.Int16",
        TypeSig::U2 => "System.UInt16",
        TypeSig::I4 => "System.Int32",
        TypeSig::U4 => "System.UInt32",
        TypeSig::I8 => "System.Int64",
        TypeSig::U8 => "System.UInt64",
        TypeSig::R4 => "System.Single",
        TypeSig::R8 => "System.Double",
        TypeSig::String => "System.String",
        TypeSig::Object => "System.Object",
        TypeSig::IntPtr => "System.IntPtr",
        TypeSig::UIntPtr => "System.UIntPtr",
        TypeSig::TypedByRef => "System.TypedReference",
        _ => return None,
    })
}
