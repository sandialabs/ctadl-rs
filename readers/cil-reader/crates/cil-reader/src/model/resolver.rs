//! Cross-assembly resolution (optional M7).
//!
//! Port of `BaseAssemblyResolver` (directory probing) and `MetadataResolver`
//! (type/field/method resolution incl. type forwarders and base-type walks).
//! `resolve()`-style APIs return `Option`, mirroring Cecil's null results; a
//! type-forwarder loop is an error (Cecil throws).

use crate::error::{Error, Result};
use crate::model::handles::{FieldDef, MemberRefSig, MethodDef, TypeDef};
use crate::model::module::Module;
use crate::model::names::GenericContext;
use crate::model::names::type_full_name;
use crate::model::sig::{MethodSig, SigReader, TypeSig, calling_convention};
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// Provides raw image bytes for assemblies (by simple name) and module files
/// (by file name, for netmodules).
pub trait AssemblyLoader {
    fn load_assembly(&self, simple_name: &str) -> Option<Vec<u8>>;
    fn load_file(&self, file_name: &str) -> Option<Vec<u8>>;
}

/// `BaseAssemblyResolver`'s directory probing: `<dir>/<name>.dll` then
/// `<dir>/<name>.exe`.
pub struct DirectoryLoader {
    pub dirs: Vec<PathBuf>,
}

impl DirectoryLoader {
    pub fn new(dirs: Vec<PathBuf>) -> DirectoryLoader {
        DirectoryLoader { dirs }
    }
}

impl AssemblyLoader for DirectoryLoader {
    fn load_assembly(&self, simple_name: &str) -> Option<Vec<u8>> {
        for dir in &self.dirs {
            for ext in ["dll", "exe"] {
                let path = dir.join(format!("{simple_name}.{ext}"));
                if let Ok(data) = std::fs::read(&path) {
                    return Some(data);
                }
            }
        }
        None
    }

    fn load_file(&self, file_name: &str) -> Option<Vec<u8>> {
        for dir in &self.dirs {
            let path = dir.join(file_name);
            if let Ok(data) = std::fs::read(&path) {
                return Some(data);
            }
        }
        None
    }
}

// A cache entry owns the image bytes and the module reading them. The module
// field is declared first so it drops before the bytes it borrows (Module has
// no Drop, but keep the order defensive).
struct CacheEntry {
    module: Option<Module<'static>>,
    _bytes: Box<[u8]>,
}

/// Owns every resolved module, handing out `&Module` references that live as
/// long as the cache. Append-only: entries are never removed or replaced, and
/// each lives in its own box, so returned references stay valid across later
/// insertions (the `FrozenMap` pattern). Not `Sync`.
pub struct AssemblyCache<'l> {
    loader: &'l dyn AssemblyLoader,
    entries: UnsafeCell<HashMap<String, Box<CacheEntry>>>,
}

impl<'l> AssemblyCache<'l> {
    pub fn new(loader: &'l dyn AssemblyLoader) -> AssemblyCache<'l> {
        AssemblyCache {
            loader,
            entries: UnsafeCell::new(HashMap::new()),
        }
    }

    /// The module for `simple_name`, loading it on first use. `None` if the
    /// loader can't find it or it isn't a valid image.
    pub fn assembly(&self, simple_name: &str) -> Option<&Module<'_>> {
        self.entry(&format!("asm:{simple_name}"), || {
            self.loader.load_assembly(simple_name)
        })
    }

    /// The module for a file name (netmodules).
    pub fn file(&self, file_name: &str) -> Option<&Module<'_>> {
        self.entry(&format!("file:{file_name}"), || {
            self.loader.load_file(file_name)
        })
    }

    fn entry(&self, key: &str, load: impl FnOnce() -> Option<Vec<u8>>) -> Option<&Module<'_>> {
        // SAFETY: entries are only inserted, never removed or overwritten;
        // each CacheEntry is boxed so its address is stable; the returned
        // reference points into the box, not the map. The map is only touched
        // inside this short scope (loading happens with no live map borrow).
        unsafe {
            if let Some(entry) = (*self.entries.get()).get(key) {
                return entry.module.as_ref();
            }
        }
        let bytes: Box<[u8]> = load()?.into_boxed_slice();
        // SAFETY of the lifetime cast: the module borrows `bytes`, which lives
        // in the same box and is never dropped or moved while the cache lives.
        let static_bytes: &'static [u8] = unsafe { &*std::ptr::from_ref::<[u8]>(&bytes) };
        let module = Module::read(static_bytes).ok();
        let entry = Box::new(CacheEntry {
            module,
            _bytes: bytes,
        });
        unsafe {
            let map = &mut *self.entries.get();
            map.entry(key.to_string()).or_insert(entry).module.as_ref()
        }
    }
}

/// A type resolved to its definition, possibly in another module.
#[derive(Clone, Copy)]
pub struct ResolvedType<'c> {
    pub module: &'c Module<'c>,
    pub ty: TypeDef<'c>,
}

pub enum ResolvedMember<'c> {
    Field(&'c Module<'c>, FieldDef<'c>),
    Method(&'c Module<'c>, MethodDef<'c>),
}

impl<'s> Module<'s> {
    /// `MetadataResolver.Resolve(TypeReference)`: resolve a TypeDef, TypeRef
    /// or TypeSpec token to its definition. `None` mirrors Cecil's null
    /// (function pointers, generic parameters, unresolvable scopes).
    pub fn resolve_type<'c>(
        &'c self,
        token: MetadataToken,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        let mut visited = Vec::new();
        self.resolve_type_inner(token, cache, &mut visited)
    }

    fn resolve_type_inner<'c>(
        &'c self,
        token: MetadataToken,
        cache: &'c AssemblyCache<'_>,
        visited: &mut Vec<String>,
    ) -> Result<Option<ResolvedType<'c>>> {
        match token.token_type() {
            Some(TokenType::TypeDef) => Ok(self.type_def(token.rid()).map(|ty| ResolvedType {
                module: self_module(self),
                ty,
            })),
            Some(TokenType::TypeSpec) => {
                // GetElementType down to a named element; FnPtr and generic
                // parameters resolve to null in Cecil.
                let row = self.metadata().row::<TypeSpecRaw>(token.rid())?;
                let blob = self.metadata().blobs().get(row.signature)?;
                let sig = SigReader::new(blob).type_sig()?;
                match element_token(&sig) {
                    Some(element) => self.resolve_type_inner(element, cache, visited),
                    None => Ok(None),
                }
            }
            Some(TokenType::TypeRef) => self.resolve_type_ref(token.rid(), cache, visited),
            _ => Ok(None),
        }
    }

    fn resolve_type_ref<'c>(
        &'c self,
        rid: u32,
        cache: &'c AssemblyCache<'_>,
        visited: &mut Vec<String>,
    ) -> Result<Option<ResolvedType<'c>>> {
        // Collect the nested-name chain (root first).
        let mut chain: Vec<(String, String)> = Vec::new();
        let mut current = rid;
        let scope = loop {
            let row = self.metadata().row::<TypeRefRaw>(current)?;
            let ns = self.metadata().strings().get(row.namespace)?.to_string();
            let name = self.metadata().strings().get(row.name)?.to_string();
            chain.push((ns, name));
            if row.scope.token_type() == Some(TokenType::TypeRef) && row.scope.rid() != current {
                current = row.scope.rid();
            } else {
                break row.scope;
            }
        };
        chain.reverse();

        let target: Option<&'c Module<'c>> = match scope.token_type() {
            Some(TokenType::AssemblyRef) => {
                let asm = self.metadata().row::<AssemblyRefRaw>(scope.rid())?;
                let name = self.metadata().strings().get(asm.name)?;
                cache.assembly(name)
            }
            Some(TokenType::ModuleRef) => {
                let mr = self.metadata().row::<ModuleRefRaw>(scope.rid())?;
                let name = self.metadata().strings().get(mr.name)?;
                cache.file(name)
            }
            // Module scope (including the obfuscated self-referencing TypeRef
            // case, whose scope Cecil treats as the module).
            _ => Some(self_module(self)),
        };
        let Some(target) = target else {
            return Ok(None);
        };
        find_type_in(target, &chain, cache, visited)
    }

    /// Resolves a decoded signature type to its definition (`Resolve` on a
    /// `TypeSpecification`): function pointers and generic parameters yield
    /// `None`, everything else resolves through the element token.
    pub fn resolve_type_sig<'c>(
        &'c self,
        sig: &TypeSig,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        match element_token(sig) {
            Some(token) => self.resolve_type(token, cache),
            None => Ok(None),
        }
    }

    /// Resolves a namespace/name pair in this module, following type
    /// forwarders (`MetadataResolver.GetTypeDefinition` + exported types).
    pub fn resolve_type_name<'c>(
        &'c self,
        namespace: &str,
        name: &str,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        let chain = vec![(namespace.to_string(), name.to_string())];
        let mut visited = Vec::new();
        find_type_in(self_module(self), &chain, cache, &mut visited)
    }

    /// `MetadataResolver.Resolve(FieldReference/MethodReference)` for a
    /// MemberRef token (or a direct Field/MethodDef token).
    pub fn resolve_member<'c>(
        &'c self,
        token: MetadataToken,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedMember<'c>>> {
        match token.token_type() {
            Some(TokenType::Field) => Ok(self
                .field_def(token.rid())
                .map(|f| ResolvedMember::Field(self_module(self), f))),
            Some(TokenType::Method) => Ok(self
                .method_def(token.rid())
                .map(|m| ResolvedMember::Method(self_module(self), m))),
            Some(TokenType::MemberRef) => {
                let mr = crate::model::handles::MemberRef {
                    m: &self.data,
                    rid: token.rid(),
                };
                let class = mr.class_token()?;
                let declaring = match class.token_type() {
                    Some(TokenType::Method) => {
                        // vararg call to a MethodDef
                        return Ok(self
                            .method_def(class.rid())
                            .map(|m| ResolvedMember::Method(self_module(self), m)));
                    }
                    _ => self.resolve_type(class, cache)?,
                };
                let Some(declaring) = declaring else {
                    return Ok(None);
                };
                let name = mr.name()?;
                match mr.signature()? {
                    MemberRefSig::Field(field_sig) => {
                        let shape = self.type_shape(&field_sig)?;
                        find_field(declaring, name, &shape, cache)
                    }
                    MemberRefSig::Method(method_sig) => {
                        let shapes = MethodShapes::of(self, &method_sig)?;
                        find_method(declaring, name, &shapes, cache)
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// A module-independent structural key for a signature type: its Cecil
    /// full name rendered with an unbound generic context. Mirrors
    /// `MetadataResolver.AreSame`, which compares names/positions but not
    /// scopes.
    fn type_shape(&self, sig: &TypeSig) -> Result<String> {
        self.type_sig_full_name(sig, GenericContext::default())
    }
}

fn self_module<'c, 's>(module: &'c Module<'s>) -> &'c Module<'c> {
    // Covariant lifetime collapse: Module<'s> with 's: 'c coerces to
    // Module<'c>.
    module
}

/// The named element a TypeSpec resolves through (`GetElementType` chains).
fn element_token(sig: &TypeSig) -> Option<MetadataToken> {
    match sig {
        TypeSig::Named { token, .. } => Some(*token),
        TypeSig::GenericInst { ty, .. } => Some(*ty),
        TypeSig::Ptr(inner)
        | TypeSig::ByRef(inner)
        | TypeSig::Pinned(inner)
        | TypeSig::SzArray(inner)
        | TypeSig::Sentinel(inner) => element_token(inner),
        TypeSig::Array { element, .. } => element_token(element),
        TypeSig::CModReq { inner, .. } | TypeSig::CModOpt { inner, .. } => element_token(inner),
        // FnPtr, Var/MVar and primitives resolve to null here (primitives
        // would need corlib name lookup; Cecil goes through the TypeRef, which
        // the signature carries only as an element type for named types).
        _ => None,
    }
}

/// `MetadataResolver.GetTypeDefinition` + exported-type (forwarder) handling.
fn find_type_in<'c>(
    module: &'c Module<'c>,
    chain: &[(String, String)],
    cache: &'c AssemblyCache<'_>,
    visited: &mut Vec<String>,
) -> Result<Option<ResolvedType<'c>>> {
    // Direct definition.
    let mut current: Option<TypeDef<'c>> = None;
    for (i, (ns, name)) in chain.iter().enumerate() {
        current = match current {
            None => module.get_type_by(ns, name)?,
            Some(ty) => ty.nested_type(&type_full_name(ns, name))?,
        };
        if current.is_none() {
            if i == 0 {
                break; // fall through to exported types
            }
            return Ok(None);
        }
    }
    if let Some(ty) = current {
        return Ok(Some(ResolvedType { module, ty }));
    }

    // Exported types: type forwarders re-export the root name to another
    // assembly; nested chains follow the ExportedType parent links.
    let (root_ns, root_name) = &chain[0];
    for et in module.exported_types() {
        if et.name()? != root_name.as_str()
            || et.namespace()? != root_ns.as_str()
            || et.declaring_type()?.is_some()
        {
            continue;
        }
        let imp = et.implementation()?;
        match imp.token_type() {
            Some(TokenType::AssemblyRef) => {
                let asm = module.metadata().row::<AssemblyRefRaw>(imp.rid())?;
                let name = module.metadata().strings().get(asm.name)?.to_string();
                // Forwarder-loop guard (Cecil throws on circular forwarders).
                let key = format!("{name}!{root_ns}.{root_name}");
                if visited.contains(&key) {
                    return Err(Error::Unsupported("type forwarder loop"));
                }
                visited.push(key);
                let Some(target) = cache.assembly(&name) else {
                    return Ok(None);
                };
                return find_type_in(target, chain, cache, visited);
            }
            Some(TokenType::File) => {
                let file = module.metadata().row::<FileRaw>(imp.rid())?;
                let name = module.metadata().strings().get(file.name)?;
                let Some(target) = cache.file(name) else {
                    return Ok(None);
                };
                return find_type_in(target, chain, cache, visited);
            }
            _ => continue,
        }
    }
    Ok(None)
}

/// Field lookup with base-type walking (`MetadataResolver.GetField`).
fn find_field<'c>(
    declaring: ResolvedType<'c>,
    name: &str,
    shape: &str,
    cache: &'c AssemblyCache<'_>,
) -> Result<Option<ResolvedMember<'c>>> {
    let mut current = Some(declaring);
    while let Some(rt) = current {
        for field in rt.ty.fields()? {
            if field.name()? != name {
                continue;
            }
            let field_shape = rt
                .module
                .type_sig_full_name(&field.field_type()?, GenericContext::default())?;
            if field_shape == shape {
                return Ok(Some(ResolvedMember::Field(rt.module, field)));
            }
        }
        current = base_of(rt, cache)?;
    }
    Ok(None)
}

struct MethodShapes {
    has_this: bool,
    is_vararg: bool,
    generic_arity: u32,
    return_shape: String,
    /// Fixed parameters only (up to the sentinel for vararg calls).
    param_shapes: Vec<String>,
}

impl MethodShapes {
    fn of(module: &Module<'_>, sig: &MethodSig) -> Result<MethodShapes> {
        let mut param_shapes = Vec::new();
        let mut is_vararg = sig.calling_convention == calling_convention::VAR_ARG;
        for p in &sig.params {
            if p.is_sentinel() {
                is_vararg = true;
                break;
            }
            param_shapes.push(module.type_sig_full_name(p, GenericContext::default())?);
        }
        Ok(MethodShapes {
            has_this: sig.has_this,
            is_vararg,
            generic_arity: sig.generic_arity,
            return_shape: module.type_sig_full_name(&sig.return_type, GenericContext::default())?,
            param_shapes,
        })
    }
}

/// Method lookup with base-type walking (`MetadataResolver.GetMethod`).
fn find_method<'c>(
    declaring: ResolvedType<'c>,
    name: &str,
    reference: &MethodShapes,
    cache: &'c AssemblyCache<'_>,
) -> Result<Option<ResolvedMember<'c>>> {
    let mut current = Some(declaring);
    while let Some(rt) = current {
        for method in rt.ty.methods()? {
            if method.name()? != name {
                continue;
            }
            let sig = method.signature()?;
            let candidate = MethodShapes::of(rt.module, &sig)?;
            if candidate.generic_arity != reference.generic_arity
                || candidate.has_this != reference.has_this
                || candidate.is_vararg != reference.is_vararg
                || candidate.return_shape != reference.return_shape
            {
                continue;
            }
            if reference.is_vararg {
                // IsVarArgCallTo: the reference's fixed parameters must match
                // the definition's full parameter list.
                if candidate.param_shapes != reference.param_shapes {
                    continue;
                }
            } else if candidate.param_shapes != reference.param_shapes {
                continue;
            }
            return Ok(Some(ResolvedMember::Method(rt.module, method)));
        }
        current = base_of(rt, cache)?;
    }
    Ok(None)
}

fn base_of<'c>(
    rt: ResolvedType<'c>,
    cache: &'c AssemblyCache<'_>,
) -> Result<Option<ResolvedType<'c>>> {
    let Some(base) = rt.ty.base_type()? else {
        return Ok(None);
    };
    rt.module.resolve_type(base.token(), cache)
}
