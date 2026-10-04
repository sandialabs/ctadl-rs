//! `Module`: the root of the Cecil-shaped object model.

use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::handles::*;
use crate::model::util::sha1;
use crate::raw::heaps::Guid;
use crate::raw::pe::{ModuleKind, PeImage, TargetArchitecture};
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};

/// A loaded module (Cecil `ModuleDefinition`). Borrows the image bytes; all
/// handles resolve through it.
pub struct Module<'a> {
    pub(crate) data: ModuleData<'a>,
}

/// `Mono.Cecil.AssemblyAttributes`.
pub mod assembly_attributes {
    pub const PUBLIC_KEY: u32 = 0x1;
    pub const RETARGETABLE: u32 = 0x100;
    pub const WINDOWS_RUNTIME: u32 = 0x200;
    pub const DISABLE_JIT_COMPILE_OPTIMIZER: u32 = 0x4000;
    pub const ENABLE_JIT_COMPILE_TRACKING: u32 = 0x8000;
}

/// An assembly name — definition (Assembly table) or reference (AssemblyRef
/// table). Cecil `AssemblyNameDefinition`/`AssemblyNameReference`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssemblyName<'a> {
    pub name: &'a str,
    pub culture: &'a str,
    /// (major, minor, build, revision)
    pub version: (u16, u16, u16, u16),
    pub attributes: u32,
    /// Only for definitions and refs with the PublicKey flag.
    pub public_key: &'a [u8],
    /// Only for refs without the PublicKey flag.
    pub public_key_token: &'a [u8],
    pub hash: &'a [u8],
    /// Definitions only.
    pub hash_algorithm: u32,
    pub token: MetadataToken,
}

impl AssemblyName<'_> {
    pub fn is_retargetable(&self) -> bool {
        self.attributes & assembly_attributes::RETARGETABLE != 0
    }

    pub fn is_windows_runtime(&self) -> bool {
        self.attributes & assembly_attributes::WINDOWS_RUNTIME != 0
    }

    pub fn has_public_key(&self) -> bool {
        self.attributes & assembly_attributes::PUBLIC_KEY != 0
    }

    /// The stored token, or one computed from the public key (SHA-1, last 8
    /// bytes reversed — `AssemblyNameReference.PublicKeyToken`).
    pub fn computed_public_key_token(&self) -> Vec<u8> {
        if !self.public_key_token.is_empty() {
            return self.public_key_token.to_vec();
        }
        if self.public_key.is_empty() {
            return Vec::new();
        }
        let hash = sha1(self.public_key);
        let mut token: Vec<u8> = hash[hash.len() - 8..].to_vec();
        token.reverse();
        token
    }

    /// `AssemblyNameReference.FullName`.
    pub fn full_name(&self) -> String {
        let mut s = String::new();
        s.push_str(self.name);
        s.push_str(", Version=");
        let (a, b, c, d) = self.version;
        s.push_str(&format!("{a}.{b}.{c}.{d}"));
        s.push_str(", Culture=");
        s.push_str(if self.culture.is_empty() {
            "neutral"
        } else {
            self.culture
        });
        s.push_str(", PublicKeyToken=");
        let token = self.computed_public_key_token();
        if token.is_empty() {
            s.push_str("null");
        } else {
            for b in token {
                s.push_str(&format!("{b:02x}"));
            }
        }
        if self.is_retargetable() {
            s.push_str(", Retargetable=Yes");
        }
        s
    }
}

/// `Mono.Cecil.ManifestResourceAttributes` visibility.
pub mod manifest_resource_attributes {
    pub const VISIBILITY_MASK: u32 = 0x7;
    pub const PUBLIC: u32 = 0x1;
    pub const PRIVATE: u32 = 0x2;
}

/// A manifest resource (Cecil `EmbeddedResource` / `LinkedResource` /
/// `AssemblyLinkedResource`).
pub enum Resource<'a> {
    Embedded {
        name: &'a str,
        attributes: u32,
        /// Offset into the CLI resources directory.
        offset: u32,
    },
    Linked {
        name: &'a str,
        attributes: u32,
        file: &'a str,
        hash: &'a [u8],
    },
    AssemblyLinked {
        name: &'a str,
        attributes: u32,
        assembly: AssemblyName<'a>,
    },
}

impl<'a> ModuleData<'a> {
    /// `Module::assembly_name`.
    pub(crate) fn assembly_name(&self) -> Result<Option<AssemblyName<'a>>> {
        if self.md.row_count(Table::Assembly) == 0 {
            return Ok(None);
        }
        let row = self.md.row::<AssemblyRaw>(1)?;
        let strings = self.md.strings();
        let blobs = self.md.blobs();
        let public_key = blobs.get(row.public_key)?;
        Ok(Some(AssemblyName {
            name: strings.get(row.name)?,
            culture: strings.get(row.culture)?,
            version: (
                row.major_version,
                row.minor_version,
                row.build_number,
                row.revision_number,
            ),
            attributes: with_public_key_flag(row.flags, public_key),
            public_key,
            public_key_token: &[],
            hash: &[],
            hash_algorithm: row.hash_alg_id,
            token: MetadataToken::new(TokenType::Assembly, 1),
        }))
    }
}

/// `flags` with the PublicKey flag set exactly when `public_key` is
/// non-empty, as Cecil's `AssemblyNameReference.PublicKey` setter leaves it.
fn with_public_key_flag(flags: u32, public_key: &[u8]) -> u32 {
    if public_key.is_empty() {
        flags & !assembly_attributes::PUBLIC_KEY
    } else {
        flags | assembly_attributes::PUBLIC_KEY
    }
}

impl<'a> Resource<'a> {
    pub fn name(&self) -> &'a str {
        match self {
            Resource::Embedded { name, .. }
            | Resource::Linked { name, .. }
            | Resource::AssemblyLinked { name, .. } => name,
        }
    }

    pub fn attributes(&self) -> u32 {
        match self {
            Resource::Embedded { attributes, .. }
            | Resource::Linked { attributes, .. }
            | Resource::AssemblyLinked { attributes, .. } => *attributes,
        }
    }

    pub fn is_public(&self) -> bool {
        self.attributes() & manifest_resource_attributes::VISIBILITY_MASK
            == manifest_resource_attributes::PUBLIC
    }
}

/// A module reference (Cecil `ModuleReference`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleRef<'a> {
    pub name: &'a str,
    pub token: MetadataToken,
}

/// Any entity a metadata token can resolve to (Cecil `LookupToken`).
pub enum Entity<'a> {
    TypeDef(TypeDef<'a>),
    TypeRef(TypeRef<'a>),
    TypeSpec(TypeSpec<'a>),
    Field(FieldDef<'a>),
    Method(MethodDef<'a>),
    MemberRef(MemberRef<'a>),
    MethodSpec(MethodSpec<'a>),
}

impl<'a> Module<'a> {
    /// Parses the image and locates the metadata (Cecil
    /// `ModuleDefinition.ReadModule`, deferred mode).
    pub fn read(bytes: &'a [u8]) -> Result<Module<'a>> {
        let image = PeImage::parse(bytes)?;
        let md = image.metadata()?;
        Ok(Module {
            data: ModuleData {
                image,
                md,
                lazy: Default::default(),
            },
        })
    }

    fn m(&self) -> &ModuleData<'a> {
        &self.data
    }

    pub fn image(&self) -> &PeImage<'a> {
        &self.data.image
    }

    pub fn metadata(&self) -> &crate::raw::Metadata<'a> {
        &self.data.md
    }

    /// The module name from the Module table.
    pub fn name(&self) -> Result<&'a str> {
        let row = self.data.md.row::<ModuleRaw>(1)?;
        self.data.md.strings().get(row.name)
    }

    pub fn mvid(&self) -> Result<Guid> {
        let row = self.data.md.row::<ModuleRaw>(1)?;
        self.data.md.guids().get(row.mvid)
    }

    /// `ModuleDefinition.Kind`: the image kind, downgraded to `NetModule` when
    /// there is no Assembly row (`AssemblyReader.ReadAssembly`).
    pub fn kind(&self) -> ModuleKind {
        if self.data.md.row_count(Table::Assembly) == 0 {
            ModuleKind::NetModule
        } else {
            self.data.image.kind
        }
    }

    pub fn architecture(&self) -> TargetArchitecture {
        self.data.image.architecture
    }

    pub fn attributes(&self) -> u32 {
        self.data.image.attributes
    }

    /// `ModuleDefinition.Characteristics`: the PE optional header's
    /// DllCharacteristics, not the COFF Characteristics.
    #[allow(clippy::misnamed_getters, reason = "Cecil's name for this field")]
    pub fn characteristics(&self) -> u16 {
        self.data.image.dll_characteristics
    }

    pub fn runtime_version(&self) -> &'a str {
        self.data.image.runtime_version
    }

    pub fn has_debug_header(&self) -> bool {
        !self.data.image.debug_header.is_empty()
    }

    /// The assembly manifest, if this module has one.
    pub fn assembly_name(&self) -> Result<Option<AssemblyName<'a>>> {
        self.data.assembly_name()
    }

    pub fn assembly_references(&self) -> Result<Vec<AssemblyName<'a>>> {
        let strings = self.data.md.strings();
        let blobs = self.data.md.blobs();
        self.data
            .md
            .table::<AssemblyRefRaw>()
            .rows()
            .map(|(rid, row)| {
                let row = row?;
                let key_or_token = blobs.get(row.public_key_or_token)?;
                let has_key = row.flags & assembly_attributes::PUBLIC_KEY != 0;
                Ok(AssemblyName {
                    name: strings.get(row.name)?,
                    culture: strings.get(row.culture)?,
                    version: (
                        row.major_version,
                        row.minor_version,
                        row.build_number,
                        row.revision_number,
                    ),
                    // Cecil sets PublicKey only when the flag is set, and the
                    // setter then clears the flag if the key is empty.
                    attributes: if has_key {
                        with_public_key_flag(row.flags, key_or_token)
                    } else {
                        row.flags
                    },
                    public_key: if has_key { key_or_token } else { &[] },
                    public_key_token: if has_key { &[] } else { key_or_token },
                    hash: blobs.get(row.hash_value)?,
                    hash_algorithm: 0,
                    token: MetadataToken::new(TokenType::AssemblyRef, rid),
                })
            })
            .collect()
    }

    pub fn module_references(&self) -> Result<Vec<ModuleRef<'a>>> {
        let strings = self.data.md.strings();
        self.data
            .md
            .table::<ModuleRefRaw>()
            .rows()
            .map(|(rid, row)| {
                Ok(ModuleRef {
                    name: strings.get(row?.name)?,
                    token: MetadataToken::new(TokenType::ModuleRef, rid),
                })
            })
            .collect()
    }

    /// `MetadataReader.ReadResources`. Rows whose implementation kind is
    /// unsupported are skipped, as in Cecil.
    pub fn resources(&self) -> Result<Vec<Resource<'a>>> {
        let strings = self.data.md.strings();
        let blobs = self.data.md.blobs();
        let mut out = Vec::new();
        for (_, row) in self.data.md.table::<ManifestResourceRaw>().rows() {
            let row = row?;
            let name = strings.get(row.name)?;
            let imp = row.implementation;
            if imp.rid() == 0 {
                out.push(Resource::Embedded {
                    name,
                    attributes: row.flags,
                    offset: row.offset,
                });
            } else if imp.token_type() == Some(TokenType::AssemblyRef) {
                let refs = self.assembly_references()?;
                let Some(assembly) = refs.into_iter().nth(imp.rid() as usize - 1) else {
                    continue;
                };
                out.push(Resource::AssemblyLinked {
                    name,
                    attributes: row.flags,
                    assembly,
                });
            } else if imp.token_type() == Some(TokenType::File) {
                let file = self.data.md.row::<FileRaw>(imp.rid())?;
                out.push(Resource::Linked {
                    name,
                    attributes: row.flags,
                    file: strings.get(file.name)?,
                    hash: blobs.get(file.hash_value)?,
                });
            }
        }
        Ok(out)
    }

    /// The data of an embedded resource (`MetadataReader.GetManagedResource`):
    /// a length-prefixed blob at `offset` in the CLI resources directory.
    pub fn resource_data(&self, offset: u32) -> Result<&'a [u8]> {
        let dir = self.data.image.resources_directory;
        if dir.is_zero() {
            return Err(Error::Unsupported("module has no resources directory"));
        }
        let base = self.data.image.resolve_rva(dir.virtual_address)?;
        let mut cur = crate::raw::cursor::Cur::at(self.data.image.data(), base + offset as usize);
        let len = cur.u32()? as usize;
        cur.bytes(len)
    }

    pub fn has_exported_types(&self) -> bool {
        self.data.md.row_count(Table::ExportedType) != 0
    }

    pub fn exported_types(&self) -> impl ExactSizeIterator<Item = ExportedType<'_>> {
        let m = self.m();
        (1..self.data.md.row_count(Table::ExportedType) + 1).map(move |rid| ExportedType { m, rid })
    }

    /// Top-level (non-nested) types, in rid order (Cecil `Module.Types`).
    pub fn types(&self) -> Result<Vec<TypeDef<'_>>> {
        let mut out = Vec::new();
        for t in self.all_types() {
            if !t.is_nested()? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Every TypeDef row, nested included (Rocks `GetAllTypes` analogue,
    /// in rid order).
    pub fn all_types(&self) -> impl ExactSizeIterator<Item = TypeDef<'_>> {
        let m = self.m();
        (1..self.data.md.row_count(Table::TypeDef) + 1).map(move |rid| TypeDef { m, rid })
    }

    pub fn type_def(&self, rid: u32) -> Option<TypeDef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::TypeDef))
            .then(|| TypeDef { m: self.m(), rid })
    }

    pub fn type_ref(&self, rid: u32) -> Option<TypeRef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::TypeRef))
            .then(|| TypeRef { m: self.m(), rid })
    }

    pub fn method_def(&self, rid: u32) -> Option<MethodDef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::Method))
            .then(|| MethodDef { m: self.m(), rid })
    }

    pub fn property_def(&self, rid: u32) -> Option<PropertyDef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::Property))
            .then(|| PropertyDef { m: self.m(), rid })
    }

    pub fn event_def(&self, rid: u32) -> Option<EventDef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::Event))
            .then(|| EventDef { m: self.m(), rid })
    }

    pub fn custom_attribute(
        &self,
        rid: u32,
    ) -> Option<crate::model::custom_attributes::CustomAttribute<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::CustomAttribute))
            .then(|| crate::model::custom_attributes::CustomAttribute { m: self.m(), rid })
    }

    pub fn field_def(&self, rid: u32) -> Option<FieldDef<'_>> {
        (rid >= 1 && rid <= self.data.md.row_count(Table::Field))
            .then(|| FieldDef { m: self.m(), rid })
    }

    /// `ModuleDefinition.EntryPoint`.
    pub fn entry_point(&self) -> Result<Option<MethodDef<'_>>> {
        let token = MetadataToken(self.data.image.entry_point_token);
        if token.rid() == 0 || token.token_type() != Some(TokenType::Method) {
            return Ok(None);
        }
        Ok(self.method_def(token.rid()))
    }

    /// `ModuleDefinition.GetType(fullName)`: nested types separated by `/`,
    /// each matched on its namespace and name (`GetNestedType`). A `/` at
    /// the start is part of a top-level name.
    pub fn get_type(&self, full_name: &str) -> Result<Option<TypeDef<'_>>> {
        if let Some((outer, rest)) = full_name
            .split_once('/')
            .filter(|(outer, _)| !outer.is_empty())
        {
            let Some(mut ty) = self.get_type(outer)? else {
                return Ok(None);
            };
            for segment in rest.split('/') {
                let Some(nested) = ty.nested_type(segment)? else {
                    return Ok(None);
                };
                ty = nested;
            }
            return Ok(Some(ty));
        }
        let (ns, name) = match full_name.rsplit_once('.') {
            Some((ns, name)) => (ns, name),
            None => ("", full_name),
        };
        self.get_type_by(ns, name)
    }

    /// `ModuleDefinition.GetType(namespace, name)` (top-level only). Of
    /// several types with the same name, the last: Cecil's name cache keeps
    /// the last one added.
    pub fn get_type_by(&self, namespace: &str, name: &str) -> Result<Option<TypeDef<'_>>> {
        top_level_type(self.m(), namespace, name)
    }

    /// `ModuleDefinition.HasTypeReference`.
    pub fn has_type_reference(&self, scope: Option<&str>, full_name: &str) -> Result<bool> {
        for rid in 1..=self.data.md.row_count(Table::TypeRef) {
            let tr = TypeRef { m: self.m(), rid };
            if tr.full_name()? != full_name {
                continue;
            }
            match scope {
                None => return Ok(true),
                Some(scope) => {
                    if tr.scope_name()? == scope {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Custom attributes attached to any owner token (Cecil
    /// `ICustomAttributeProvider`); rows in CustomAttribute-table order.
    pub fn custom_attributes_of(
        &self,
        owner: MetadataToken,
    ) -> Result<Vec<crate::model::custom_attributes::CustomAttribute<'_>>> {
        let m = self.m();
        let Some(ranges) = m.custom_attribute_ranges()?.get(&owner) else {
            return Ok(Vec::new());
        };
        Ok(ranges
            .iter()
            .flat_map(|r| r.iter())
            .map(|rid| crate::model::custom_attributes::CustomAttribute { m, rid })
            .collect())
    }

    pub fn has_custom_attributes(&self, owner: MetadataToken) -> Result<bool> {
        Ok(self
            .m()
            .custom_attribute_ranges()?
            .get(&owner)
            .is_some_and(|ranges| ranges.iter().any(|r| r.length > 0)))
    }

    /// Security declarations attached to an owner token (TypeDef, MethodDef
    /// or Assembly).
    pub fn security_declarations_of(
        &self,
        owner: MetadataToken,
    ) -> Result<Vec<crate::model::custom_attributes::SecurityDeclaration<'_>>> {
        let m = self.m();
        let Some(ranges) = m.security_ranges()?.get(&owner) else {
            return Ok(Vec::new());
        };
        Ok(ranges
            .iter()
            .flat_map(|r| r.iter())
            .map(|rid| crate::model::custom_attributes::SecurityDeclaration { m, rid })
            .collect())
    }

    /// The Assembly manifest token (owner of assembly-level attributes).
    pub fn assembly_token(&self) -> MetadataToken {
        MetadataToken::new(TokenType::Assembly, 1)
    }

    /// Renders a decoded type signature's Cecil `FullName`.
    pub fn type_sig_full_name(
        &self,
        sig: &crate::model::sig::TypeSig,
        ctx: crate::model::names::GenericContext,
    ) -> Result<String> {
        let mut out = String::new();
        crate::model::names::sig_full_name(self.m(), sig, ctx, &mut out)?;
        Ok(out)
    }

    /// `ModuleDefinition.LookupToken` for the 7 supported token kinds.
    pub fn lookup_token(&self, token: MetadataToken) -> Result<Option<Entity<'_>>> {
        let rid = token.rid();
        if rid == 0 {
            return Ok(None);
        }
        let m = self.m();
        let in_range = |table: Table| rid <= self.data.md.row_count(table);
        Ok(match token.token_type() {
            Some(TokenType::TypeDef) if in_range(Table::TypeDef) => {
                Some(Entity::TypeDef(TypeDef { m, rid }))
            }
            Some(TokenType::TypeRef) if in_range(Table::TypeRef) => {
                Some(Entity::TypeRef(TypeRef { m, rid }))
            }
            Some(TokenType::TypeSpec) if in_range(Table::TypeSpec) => {
                Some(Entity::TypeSpec(TypeSpec { m, rid }))
            }
            Some(TokenType::Field) if in_range(Table::Field) => {
                Some(Entity::Field(FieldDef { m, rid }))
            }
            Some(TokenType::Method) if in_range(Table::Method) => {
                Some(Entity::Method(MethodDef { m, rid }))
            }
            Some(TokenType::MemberRef) if in_range(Table::MemberRef) => {
                Some(Entity::MemberRef(MemberRef { m, rid }))
            }
            Some(TokenType::MethodSpec) if in_range(Table::MethodSpec) => {
                Some(Entity::MethodSpec(MethodSpec { m, rid }))
            }
            _ => None,
        })
    }
}

/// [`Module::get_type_by`] on the module's data.
pub(crate) fn top_level_type<'a>(
    m: &'a ModuleData<'a>,
    namespace: &str,
    name: &str,
) -> Result<Option<TypeDef<'a>>> {
    for rid in (1..=m.md.row_count(Table::TypeDef)).rev() {
        let t = TypeDef { m, rid };
        if t.name()? == name && t.namespace()? == namespace && !t.is_nested()? {
            return Ok(Some(t));
        }
    }
    Ok(None)
}
