//! Cross-assembly resolution (optional M7).
//!
//! Port of `BaseAssemblyResolver` (directory probing) and `MetadataResolver`
//! (type/field/method resolution incl. type forwarders and base-type walks).
//! `resolve()`-style APIs return `Option`, mirroring Cecil's null results; a
//! type-forwarder loop is an error (Cecil throws).

use crate::error::{Error, Result};
use crate::model::handles::{ExportedType, FieldDef, MemberRefSig, MethodDef, TypeDef};
use crate::model::module::{AssemblyName, Module};
use crate::model::names::GenericContext;
use crate::model::names::type_full_name;
use crate::model::sig::{MethodSig, TypeSig, calling_convention};
use crate::model::typeparser::{ParsedType, parse_int32, split_full_name};
use crate::raw::pe::{TargetRuntime, parse_runtime};
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};
use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Provides raw image bytes for assemblies (by reference, as Cecil's
/// `IAssemblyResolver` gets them) and module files (by file name, for
/// netmodules).
pub trait AssemblyLoader {
    fn load_assembly(&self, name: &AssemblyName<'_>) -> Option<Vec<u8>>;
    fn load_file(&self, file_name: &str) -> Option<Vec<u8>>;
}

/// `BaseAssemblyResolver`'s directory probing by simple name:
/// `<dir>/<name>.dll` then `<dir>/<name>.exe` (`.winmd` then `.dll` for a
/// Windows Runtime reference), in each directory. A file that is not a CLI
/// image is passed over, as `SearchDirectory` catches
/// `BadImageFormatException`.
pub struct DirectoryLoader {
    pub dirs: Vec<PathBuf>,
}

impl DirectoryLoader {
    pub fn new(dirs: Vec<PathBuf>) -> DirectoryLoader {
        DirectoryLoader { dirs }
    }
}

impl AssemblyLoader for DirectoryLoader {
    fn load_assembly(&self, name: &AssemblyName<'_>) -> Option<Vec<u8>> {
        let extensions = if name.is_windows_runtime() {
            ["winmd", "dll"]
        } else {
            ["dll", "exe"]
        };
        for dir in &self.dirs {
            for ext in extensions {
                let path = dir.join(format!("{}.{ext}", name.name));
                if let Ok(data) = std::fs::read(&path)
                    && Module::read(&data).is_ok()
                {
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
    /// Whether resolving through an assembly the loader can't find is an
    /// error rather than a null result.
    throws_on_miss: bool,
}

/// The error resolution gives, from an [`AssemblyCache::throwing`] cache,
/// for an assembly its loader can't find: Cecil's
/// `AssemblyResolutionException`.
pub const ASSEMBLY_NOT_RESOLVED: Error = Error::Unsupported("assembly not resolved");

impl<'l> AssemblyCache<'l> {
    /// A cache whose misses resolve to nothing, as an `IAssemblyResolver`
    /// that returns null does.
    pub fn new(loader: &'l dyn AssemblyLoader) -> AssemblyCache<'l> {
        AssemblyCache {
            loader,
            entries: UnsafeCell::new(HashMap::new()),
            throws_on_miss: false,
        }
    }

    /// A cache whose misses are [`ASSEMBLY_NOT_RESOLVED`], as
    /// `DefaultAssemblyResolver` throws `AssemblyResolutionException`.
    /// Custom-attribute decoding tells this apart from a type that is not
    /// found, as Cecil does.
    pub fn throwing(loader: &'l dyn AssemblyLoader) -> AssemblyCache<'l> {
        AssemblyCache {
            throws_on_miss: true,
            ..AssemblyCache::new(loader)
        }
    }

    /// [`AssemblyCache::assembly`] for resolution: a miss is `None`, or an
    /// error from a throwing cache.
    fn resolve_assembly(&self, name: &AssemblyName<'_>) -> Result<Option<&Module<'_>>> {
        match self.assembly(name) {
            None if self.throws_on_miss => Err(ASSEMBLY_NOT_RESOLVED),
            found => Ok(found),
        }
    }

    /// The module for the assembly reference `name`, loading it on first
    /// use; cached by full name, as `DefaultAssemblyResolver` caches. `None`
    /// if the loader can't find it, or it isn't a valid image or an
    /// assembly's (Cecil's `ReadModule(file).Assembly` is null for a
    /// netmodule). A miss is not cached: Cecil's resolver throws on one.
    pub fn assembly(&self, name: &AssemblyName<'_>) -> Option<&Module<'_>> {
        self.entry(&format!("asm:{}", name.full_name()), || {
            self.loader.load_assembly(name)
        })
        .filter(|module| matches!(module.assembly_name(), Ok(Some(_))))
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
        let module = self_module(self);
        let name = match token.token_type() {
            Some(TokenType::TypeDef) => {
                // Cecil resolves a definition like any reference, by name:
                // of two top-level types with one name, either resolves to
                // the last.
                let Some(mut ty) = self.type_def(token.rid()) else {
                    return Ok(None);
                };
                let mut chain = vec![(ty.namespace()?.to_string(), ty.name()?.to_string())];
                while let Some(outer) = ty.declaring_type()? {
                    if chain.len() > self.metadata().row_count(Table::TypeDef) as usize {
                        return Err(Error::Unsupported("nested type cycle"));
                    }
                    chain.push((outer.namespace()?.to_string(), outer.name()?.to_string()));
                    ty = outer;
                }
                chain.reverse();
                TypeName {
                    module,
                    chain,
                    scope: Scope::Module,
                }
            }
            Some(TokenType::TypeSpec) => {
                return self.resolve_type_sig(&self.m().type_spec_sig(token.rid(), 0)?, cache);
            }
            Some(TokenType::TypeRef) => TypeName::of_type_ref(module, token.rid())?,
            _ => return Ok(None),
        };
        name.resolve(cache, &mut Vec::new())
    }

    /// Resolves a decoded signature type to its definition (`Resolve` on a
    /// `TypeSpecification`): function pointers and generic parameters yield
    /// `None`, everything else resolves through the element token.
    pub fn resolve_type_sig<'c>(
        &'c self,
        sig: &TypeSig,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        // GetElementType down to a named element; FnPtr and generic
        // parameters resolve to null in Cecil. An element that is a TypeSpec
        // continues with that TypeSpec's signature, below the element's
        // level, so a cycle of TypeSpecs ends at the nesting limit.
        let mut found = element(sig, 1);
        loop {
            found = match found {
                Some((Element::Token(token), level))
                    if token.token_type() == Some(TokenType::TypeSpec) =>
                {
                    element(&self.m().type_spec_sig(token.rid(), level)?, level + 1)
                }
                Some((Element::Token(token), _)) => return self.resolve_type(token, cache),
                Some((Element::Primitive(name), _)) => return self.resolve_primitive(name, cache),
                None => return Ok(None),
            };
        }
    }

    /// Resolves the `TypeSystem` type for a primitive: in a core library,
    /// its own `System` type (or the reference its exported type makes);
    /// elsewhere, `System.<name>` in the first core-library reference, or in
    /// `mscorlib` when there is none.
    fn resolve_primitive<'c>(
        &'c self,
        name: &str,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        let module = self_module(self);
        if self.is_core_library()? {
            // CoreTypeSystem.LookupType: the first TypeDef row with the name,
            // which Resolve then looks up by name like any reference.
            for ty in self.all_types() {
                if ty.name()? == name && ty.namespace()? == "System" {
                    return self.resolve_type(ty.token(), cache);
                }
            }
            for et in self.exported_types() {
                if et.name()? == name && et.namespace()? == "System" {
                    return TypeName::of_exported(module, et)?.resolve(cache, &mut Vec::new());
                }
            }
            return Err(Error::Unsupported("core library type not found"));
        }
        TypeName {
            module,
            chain: vec![("System".to_string(), name.to_string())],
            scope: self.core_library_scope()?,
        }
        .resolve(cache, &mut Vec::new())
    }

    /// `Mixin.IsCoreLibrary(ModuleDefinition)`: an assembly with a core
    /// library's name and no assembly references.
    fn is_core_library(&self) -> Result<bool> {
        Ok(self
            .assembly_name()?
            .is_some_and(|a| CORE_LIBRARIES.contains(&a.name.as_ref()))
            && self.metadata().row_count(Table::AssemblyRef) == 0)
    }

    /// `TypeSystem.CoreLibrary`: the module itself for a core library, else
    /// its first core-library reference, else the `mscorlib` reference Cecil
    /// adds.
    fn core_library_scope(&self) -> Result<Scope> {
        if self.is_core_library()? {
            return Ok(Scope::Module);
        }
        let md = self.metadata();
        for (rid, row) in md.table::<AssemblyRefRaw>().rows() {
            if CORE_LIBRARIES.contains(&md.strings().get(row?.name)?.as_ref()) {
                return Ok(Scope::Assembly(rid));
            }
        }
        Ok(Scope::Mscorlib)
    }

    /// Resolves a type named in a blob (a custom attribute's enum or
    /// `typeof` argument) that `TypeParser.ParseType` did not find in this
    /// module: its reference is scoped by `GetMetadataScope`, the core
    /// library when the name has no assembly part, else the assembly it
    /// names.
    pub(crate) fn resolve_parsed_reference<'c>(
        &'c self,
        parsed: &ParsedType,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        let scope = match parsed.assembly.as_deref() {
            None | Some("") => self.core_library_scope()?,
            Some(assembly) => self.assembly_scope(&ParsedAssemblyName::parse(assembly)?)?,
        };
        let (ns, name) = split_full_name(&parsed.fullname);
        let mut chain = vec![(ns.to_string(), name.to_string())];
        // TypeParser.GetTypeReference: each nested name is a reference with
        // no namespace, declared by the previous one.
        chain.extend(
            parsed
                .nested_names
                .iter()
                .map(|n| (String::new(), n.clone())),
        );
        TypeName {
            module: self_module(self),
            chain,
            scope,
        }
        .resolve(cache, &mut Vec::new())
    }

    /// `TryGetAssemblyNameReference`: the first assembly reference equal to
    /// `name` in name, version, culture and public key token, else `name`
    /// itself.
    fn assembly_scope(&self, name: &ParsedAssemblyName) -> Result<Scope> {
        for rid in 1..=self.metadata().row_count(Table::AssemblyRef) {
            let reference = self.assembly_reference(rid)?;
            if reference.name == name.name
                && reference.version == name.version
                && reference.culture == name.culture
                && reference.computed_public_key_token() == name.public_key_token
            {
                return Ok(Scope::Assembly(rid));
            }
        }
        Ok(Scope::Reference(name.clone()))
    }

    /// Resolves a namespace/name pair in this module, following type
    /// forwarders (`MetadataResolver.GetTypeDefinition` + exported types).
    pub fn resolve_type_name<'c>(
        &'c self,
        namespace: &str,
        name: &str,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<ResolvedType<'c>>> {
        TypeName {
            module: self_module(self),
            chain: vec![(namespace.to_string(), name.to_string())],
            scope: Scope::Module,
        }
        .resolve(cache, &mut Vec::new())
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
                // Cecil gives up before walking the bases when the declaring
                // type has no members of the kind.
                match mr.signature()? {
                    MemberRefSig::Field(_) if !declaring.ty.has_fields()? => Ok(None),
                    MemberRefSig::Method(_) if !declaring.ty.has_methods()? => Ok(None),
                    MemberRefSig::Field(field_sig) => {
                        let shape = self.type_shape(&field_sig)?;
                        find_field(declaring, &name, &shape, cache)
                    }
                    MemberRefSig::Method(method_sig) => {
                        let shapes = MethodShapes::of(self, &method_sig)?;
                        find_method(declaring, &name, &shapes, cache)
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// A module-independent key for a signature type, equal for two types
    /// exactly when `MetadataResolver.AreSame` holds: names and generic
    /// positions, not scopes; arrays by rank alone.
    fn type_shape(&self, sig: &TypeSig) -> Result<String> {
        let mut out = String::new();
        self.write_shape(sig, 0, &mut out)?;
        Ok(out)
    }

    /// Writes the shape of `sig`, a node below `depth` enclosing levels.
    fn write_shape(&self, sig: &TypeSig, depth: u32, out: &mut String) -> Result<()> {
        // This node's level; its tokens' TypeSpecs and its children are below it.
        let level = depth + 1;
        match sig {
            // AreSame(ArrayType) compares ranks, so bounds, and SZARRAY
            // against a rank-1 ARRAY, do not matter.
            TypeSig::SzArray(element) => {
                self.write_shape(element, level, out)?;
                out.push_str("[rank 1]");
            }
            TypeSig::Array {
                element,
                dimensions,
            } => {
                self.write_shape(element, level, out)?;
                out.push_str(&format!("[rank {}]", dimensions.len()));
            }
            TypeSig::Ptr(inner) => {
                self.write_shape(inner, level, out)?;
                out.push('*');
            }
            TypeSig::ByRef(inner) => {
                self.write_shape(inner, level, out)?;
                out.push('&');
            }
            TypeSig::Pinned(inner) => {
                self.write_shape(inner, level, out)?;
                out.push_str(" pinned");
            }
            TypeSig::Sentinel(inner) => {
                out.push_str("...");
                self.write_shape(inner, level, out)?;
            }
            TypeSig::CModReq { modifier, inner } | TypeSig::CModOpt { modifier, inner } => {
                self.write_shape(inner, level, out)?;
                out.push_str(if matches!(sig, TypeSig::CModReq { .. }) {
                    " modreq("
                } else {
                    " modopt("
                });
                self.write_token_shape(*modifier, level, out)?;
                out.push(')');
            }
            TypeSig::GenericInst { ty, args, .. } => {
                self.write_token_shape(*ty, level, out)?;
                out.push('<');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.write_shape(arg, level, out)?;
                }
                out.push('>');
            }
            TypeSig::Named { token, .. } => self.write_token_shape(*token, level, out)?,
            // AreSame(FunctionPointerType) also compares HasThis and the
            // calling convention, which the full name leaves out.
            TypeSig::FnPtr(method) => {
                out.push_str(&format!(
                    "method(this={},cc=0x{:02x}) ",
                    method.has_this, method.calling_convention
                ));
                self.write_shape(&method.return_type, level, out)?;
                out.push_str(" *(");
                for (i, param) in method.params.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.write_shape(param, level, out)?;
                }
                out.push(')');
            }
            // Primitives and generic parameters: the full name, with unbound
            // generic parameters.
            _ => out.push_str(&self.type_sig_full_name(sig, GenericContext::default())?),
        }
        Ok(())
    }

    /// A TypeDefOrRef token's shape, for a token of a node at level `depth`;
    /// a TypeSpec is the type it holds.
    fn write_token_shape(&self, token: MetadataToken, depth: u32, out: &mut String) -> Result<()> {
        if token.token_type() == Some(TokenType::TypeSpec) {
            let sig = self.m().type_spec_sig(token.rid(), depth)?;
            return self.write_shape(&sig, depth, out);
        }
        let named = TypeSig::Named {
            token,
            is_value_type: false,
        };
        out.push_str(&self.type_sig_full_name(&named, GenericContext::default())?);
        Ok(())
    }
}

fn self_module<'c, 's>(module: &'c Module<'s>) -> &'c Module<'c> {
    // Covariant lifetime collapse: Module<'s> with 's: 'c coerces to
    // Module<'c>.
    module
}

/// `CommonTypeSystem.GetCoreLibraryReference`'s fallback: mscorlib, of the
/// module's runtime's version, with the ECMA key's token.
fn mscorlib_reference(module: &Module<'_>) -> AssemblyName<'static> {
    const TOKEN: &[u8] = &[0xb7, 0x7a, 0x5c, 0x56, 0x19, 0x34, 0xe0, 0x89];
    let version = match parse_runtime(module.runtime_version()) {
        TargetRuntime::Net1_0 | TargetRuntime::Net1_1 => (1, 0, 0, 0),
        TargetRuntime::Net2_0 => (2, 0, 0, 0),
        TargetRuntime::Net4_0 => (4, 0, 0, 0),
    };
    AssemblyName {
        version,
        public_key_token: TOKEN,
        ..AssemblyName::named("mscorlib")
    }
}

/// Assembly names Cecil takes for the core library (`Mixin.IsCoreLibrary`).
const CORE_LIBRARIES: &[&str] = &[
    "mscorlib",
    "System.Runtime",
    "System.Private.CoreLib",
    "netstandard",
];

/// What a signature type resolves through (`GetElementType`): a named type,
/// or a primitive, which Cecil reads as its `TypeSystem` type.
enum Element {
    Token(MetadataToken),
    Primitive(&'static str),
}

/// The element type of `sig`, a node at level `level`, and the level of the
/// node it is (or whose token it is).
fn element(sig: &TypeSig, level: u32) -> Option<(Element, u32)> {
    Some((
        Element::Primitive(match sig {
            TypeSig::Named { token, .. } | TypeSig::GenericInst { ty: token, .. } => {
                return Some((Element::Token(*token), level));
            }
            TypeSig::Ptr(inner)
            | TypeSig::ByRef(inner)
            | TypeSig::Pinned(inner)
            | TypeSig::SzArray(inner)
            | TypeSig::Sentinel(inner)
            | TypeSig::Array { element: inner, .. }
            | TypeSig::CModReq { inner, .. }
            | TypeSig::CModOpt { inner, .. } => return element(inner, level + 1),
            TypeSig::Void => "Void",
            TypeSig::Boolean => "Boolean",
            TypeSig::Char => "Char",
            TypeSig::I1 => "SByte",
            TypeSig::U1 => "Byte",
            TypeSig::I2 => "Int16",
            TypeSig::U2 => "UInt16",
            TypeSig::I4 => "Int32",
            TypeSig::U4 => "UInt32",
            TypeSig::I8 => "Int64",
            TypeSig::U8 => "UInt64",
            TypeSig::R4 => "Single",
            TypeSig::R8 => "Double",
            TypeSig::String => "String",
            TypeSig::Object => "Object",
            TypeSig::IntPtr => "IntPtr",
            TypeSig::UIntPtr => "UIntPtr",
            TypeSig::TypedByRef => "TypedReference",
            // Function pointers and generic parameters resolve to null.
            TypeSig::FnPtr(_) | TypeSig::Var(_) | TypeSig::MVar(_) => return None,
        }),
        level,
    ))
}

/// A type reference as `MetadataResolver.Resolve` sees it: the module it was
/// read from, its names (outermost first) and the outermost type's scope,
/// which Cecil's nested references share.
struct TypeName<'c> {
    module: &'c Module<'c>,
    chain: Vec<(String, String)>,
    scope: Scope,
}

/// `IMetadataScope`, or null.
#[derive(Clone)]
enum Scope {
    Module,
    /// An AssemblyRef rid of the reference's module.
    Assembly(u32),
    /// The `mscorlib` reference Cecil adds to a module with no
    /// core-library reference.
    Mscorlib,
    /// An assembly named in a blob that no assembly reference matches.
    Reference(ParsedAssemblyName),
    ModuleRef(String),
    None,
}

/// `AssemblyNameReference.Parse`: an assembly name as a type name in a blob
/// spells it.
#[derive(Clone)]
struct ParsedAssemblyName {
    name: String,
    culture: String,
    version: (u16, u16, u16, u16),
    public_key_token: Vec<u8>,
}

impl ParsedAssemblyName {
    /// The name, then `key=value` parts split on `,` and `=`. Keys match
    /// ignoring case; unknown keys are skipped, and a part without exactly
    /// one `=` is an error, as Cecil throws. Version components beyond a
    /// `u16`, which metadata can't hold, are an error too.
    fn parse(full_name: &str) -> Result<ParsedAssemblyName> {
        let mut tokens = full_name.split(',');
        let mut parsed = ParsedAssemblyName {
            name: tokens.next().unwrap_or("").trim().to_string(),
            culture: String::new(),
            version: (0, 0, 0, 0),
            public_key_token: Vec::new(),
        };
        for token in tokens {
            let parts: Vec<&str> = token.trim().split('=').collect();
            let [key, value] = parts[..] else {
                return Err(Error::Unsupported("malformed assembly name"));
            };
            match key.to_lowercase().as_str() {
                "version" => parsed.version = parse_version(value)?,
                "culture" => {
                    parsed.culture = if value == "neutral" { "" } else { value }.to_string();
                }
                "publickeytoken" if value != "null" => {
                    parsed.public_key_token = (0..value.len() / 2)
                        .map(|i| {
                            value
                                .get(i * 2..i * 2 + 2)
                                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                                .ok_or(Error::Unsupported("malformed public key token"))
                        })
                        .collect::<Result<_>>()?;
                }
                _ => {}
            }
        }
        Ok(parsed)
    }

    fn as_name(&self) -> AssemblyName<'_> {
        AssemblyName {
            culture: Cow::Borrowed(&self.culture),
            version: self.version,
            public_key_token: &self.public_key_token,
            ..AssemblyName::named(&self.name)
        }
    }
}

/// `new Version(string)` then `Mixin.CheckVersion`: two to four
/// `int.Parse` components, missing ones zero.
fn parse_version(s: &str) -> Result<(u16, u16, u16, u16)> {
    let parts = s
        .split('.')
        .map(|p| parse_int32(p).and_then(|v| u16::try_from(v).ok()))
        .collect::<Option<Vec<u16>>>()
        .ok_or(Error::Unsupported("malformed assembly version"))?;
    match parts[..] {
        [a, b] => Ok((a, b, 0, 0)),
        [a, b, c] => Ok((a, b, c, 0)),
        [a, b, c, d] => Ok((a, b, c, d)),
        _ => Err(Error::Unsupported("malformed assembly version")),
    }
}

/// Exported types being resolved, by module address and rid: Cecil's
/// `ExportedType.Resolve` reentrancy guard.
type Resolving = Vec<(usize, u32)>;

impl<'c> TypeName<'c> {
    /// A TypeRef as Cecil reads it: a TypeRef scope makes it nested in that
    /// TypeRef, and the outermost one's scope is the chain's.
    fn of_type_ref(module: &'c Module<'c>, rid: u32) -> Result<TypeName<'c>> {
        let md = module.metadata();
        let mut chain = Vec::new();
        let mut current = rid;
        let scope = loop {
            let row = md.row::<TypeRefRaw>(current)?;
            chain.push((
                md.strings().get(row.namespace)?.to_string(),
                md.strings().get(row.name)?.to_string(),
            ));
            let token = row.scope;
            let in_range = |table| (1..=md.row_count(table)).contains(&token.rid());
            match token.token_type() {
                Some(TokenType::TypeRef) if token.rid() != current && in_range(Table::TypeRef) => {
                    if chain.len() > md.row_count(Table::TypeRef) as usize {
                        return Err(Error::Unsupported("TypeRef scope cycle"));
                    }
                    current = token.rid();
                }
                // A TypeRef naming itself, or no TypeRef: Cecil scopes it to
                // the module.
                Some(TokenType::TypeRef) | Some(TokenType::Module) => break Scope::Module,
                Some(TokenType::AssemblyRef) if in_range(Table::AssemblyRef) => {
                    break Scope::Assembly(token.rid());
                }
                Some(TokenType::ModuleRef) if in_range(Table::ModuleRef) => {
                    let row = md.row::<ModuleRefRaw>(token.rid())?;
                    break Scope::ModuleRef(md.strings().get(row.name)?.to_string());
                }
                Some(TokenType::AssemblyRef) | Some(TokenType::ModuleRef) => break Scope::None,
                _ => return Err(Error::Unsupported("unsupported ResolutionScope")),
            }
        };
        chain.reverse();
        Ok(TypeName {
            module,
            chain,
            scope,
        })
    }

    /// `ExportedType.CreateReference`: the exported type's names, scoped by
    /// the outermost one's implementation.
    fn of_exported(module: &'c Module<'c>, et: ExportedType<'c>) -> Result<TypeName<'c>> {
        let nesting = et.nesting()?;
        let implementation = nesting[nesting.len() - 1].implementation()?;
        let chain = nesting
            .iter()
            .rev()
            .map(|et| Ok((et.namespace()?.to_string(), et.name()?.to_string())))
            .collect::<Result<Vec<_>>>()?;
        let md = module.metadata();
        let scope = match implementation.token_type() {
            Some(TokenType::AssemblyRef) => Scope::Assembly(implementation.rid()),
            // GetModuleReferenceFromFile: the module reference of the file's
            // name.
            Some(TokenType::File) => {
                let row = md.row::<FileRaw>(implementation.rid())?;
                Scope::ModuleRef(md.strings().get(row.name)?.to_string())
            }
            _ => {
                return Err(Error::Unsupported(
                    "unsupported ExportedType implementation",
                ));
            }
        };
        Ok(TypeName {
            module,
            chain,
            scope,
        })
    }

    /// The declaring type's reference.
    fn declaring(&self) -> TypeName<'c> {
        TypeName {
            module: self.module,
            chain: self.chain[..self.chain.len() - 1].to_vec(),
            scope: self.scope.clone(),
        }
    }

    /// `MetadataResolver.Resolve(TypeReference)`.
    fn resolve(
        &self,
        cache: &'c AssemblyCache<'_>,
        resolving: &mut Resolving,
    ) -> Result<Option<ResolvedType<'c>>> {
        let target = match &self.scope {
            Scope::None => return Ok(None),
            Scope::Module => self.module,
            Scope::Assembly(rid) => {
                match cache.resolve_assembly(&self.module.assembly_reference(*rid)?)? {
                    Some(target) => target,
                    None => return Ok(None),
                }
            }
            Scope::Mscorlib => match cache.resolve_assembly(&mscorlib_reference(self.module))? {
                Some(target) => target,
                None => return Ok(None),
            },
            Scope::Reference(name) => match cache.resolve_assembly(&name.as_name())? {
                Some(target) => target,
                None => return Ok(None),
            },
            Scope::ModuleRef(name) => match self.assembly_module(name, cache)? {
                Some(target) => target,
                None => return Ok(None),
            },
        };
        self.get_type(target, cache, resolving)
    }

    /// The module a ModuleRef scope names: the first of the assembly's
    /// modules (`Assembly.Modules`: this one, then each File with metadata)
    /// with that Module-table name. `None` outside an assembly; an error,
    /// as Cecil throws, when a module file is missing or none has the name.
    fn assembly_module(
        &self,
        name: &str,
        cache: &'c AssemblyCache<'_>,
    ) -> Result<Option<&'c Module<'c>>> {
        if self.module.assembly_name()?.is_none() {
            return Ok(None);
        }
        let md = self.module.metadata();
        let mut modules = vec![self.module];
        for (_, row) in md.table::<FileRaw>().rows() {
            let row = row?;
            if row.flags != 0 {
                continue; // ContainsNoMetaData
            }
            let file = md.strings().get(row.name)?;
            modules.push(
                cache
                    .file(&file)
                    .ok_or(Error::Unsupported("netmodule not found"))?,
            );
        }
        for module in modules {
            if module.name()? == name {
                return Ok(Some(module));
            }
        }
        Err(Error::Unsupported(
            "module reference names no module of the assembly",
        ))
    }

    /// `MetadataResolver.GetType`: the definition in `module`, else the
    /// first exported type with the innermost name.
    fn get_type(
        &self,
        module: &'c Module<'c>,
        cache: &'c AssemblyCache<'_>,
        resolving: &mut Resolving,
    ) -> Result<Option<ResolvedType<'c>>> {
        if let Some(found) = self.get_type_definition(module, cache, resolving)? {
            return Ok(Some(found));
        }
        let (ns, name) = self.chain.last().unwrap();
        for et in module.exported_types() {
            if et.name()? == name.as_str() && et.namespace()? == ns.as_str() {
                return resolve_exported(module, et, cache, resolving);
            }
        }
        Ok(None)
    }

    /// `MetadataResolver.GetTypeDefinition`: a top-level type by name, or a
    /// nested one in its resolved declaring type.
    fn get_type_definition(
        &self,
        module: &'c Module<'c>,
        cache: &'c AssemblyCache<'_>,
        resolving: &mut Resolving,
    ) -> Result<Option<ResolvedType<'c>>> {
        let (ns, name) = self.chain.last().unwrap();
        if self.chain.len() == 1 {
            return Ok(module
                .get_type_by(ns, name)?
                .map(|ty| ResolvedType { module, ty }));
        }
        let Some(outer) = self.declaring().resolve(cache, resolving)? else {
            return Ok(None);
        };
        Ok(outer
            .ty
            .nested_type(&type_full_name(ns, name))?
            .map(|ty| ResolvedType {
                module: outer.module,
                ty,
            }))
    }
}

/// `ExportedType.Resolve`, which throws when it reenters itself.
fn resolve_exported<'c>(
    module: &'c Module<'c>,
    et: ExportedType<'c>,
    cache: &'c AssemblyCache<'_>,
    resolving: &mut Resolving,
) -> Result<Option<ResolvedType<'c>>> {
    let key = (std::ptr::from_ref(module) as usize, et.token().rid());
    if resolving.contains(&key) {
        return Err(Error::Unsupported("exported type circularity"));
    }
    resolving.push(key);
    let resolved =
        TypeName::of_exported(module, et).and_then(|name| name.resolve(cache, resolving));
    resolving.pop();
    resolved
}

/// Field lookup with base-type walking (`MetadataResolver.GetField`).
fn find_field<'c>(
    declaring: ResolvedType<'c>,
    name: &str,
    shape: &str,
    cache: &'c AssemblyCache<'_>,
) -> Result<Option<ResolvedMember<'c>>> {
    let mut current = Some(declaring);
    let mut walked = HashSet::new();
    while let Some(rt) = current {
        walk_once(&mut walked, rt)?;
        for field in rt.ty.fields()? {
            if field.name()? != name {
                continue;
            }
            let field_shape = rt.module.type_shape(&field.field_type()?)?;
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
            param_shapes.push(module.type_shape(p)?);
        }
        Ok(MethodShapes {
            has_this: sig.has_this,
            is_vararg,
            generic_arity: sig.generic_arity,
            return_shape: module.type_shape(&sig.return_type)?,
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
    let mut walked = HashSet::new();
    while let Some(rt) = current {
        walk_once(&mut walked, rt)?;
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

/// Records `rt` as a type the base walk has reached, failing if it has
/// already: a type that is its own base, directly or through others, in
/// this module or another. Cecil's `GetField` and `GetMethod` follow such a
/// cycle forever.
fn walk_once(walked: &mut HashSet<(usize, u32)>, rt: ResolvedType<'_>) -> Result<()> {
    let module = std::ptr::from_ref(rt.module).addr();
    if walked.insert((module, rt.ty.token().rid())) {
        Ok(())
    } else {
        Err(Error::Unsupported("base type cycle"))
    }
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
