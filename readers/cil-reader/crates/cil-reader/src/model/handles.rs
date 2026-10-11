//! Cecil-shaped handles: `Copy` views `(module, rid)` over the raw tables.
//!
//! Everything resolves through the module; no object graph, no cycles
//! (contrast `TypeDefinition`/`MethodDefinition` in Cecil).

use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::model::data::{ModuleData, Range};
use crate::model::marshal::MarshalInfo;
use crate::model::names::{
    GenericContext, method_params_full_name, sig_full_name, token_full_name, typedef_full_name,
    typeref_full_name,
};
use crate::model::sig::{ElementType, MethodSig, PropertySig, SigReader, TypeSig};
use crate::raw::heaps::Utf16Str;
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident, $raw:ty, $tt:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy)]
        pub struct $name<'a> {
            pub(crate) m: &'a ModuleData<'a>,
            pub(crate) rid: u32,
        }

        impl<'a> $name<'a> {
            pub fn rid(&self) -> u32 {
                self.rid
            }

            pub fn token(&self) -> MetadataToken {
                MetadataToken::new($tt, self.rid)
            }

            pub(crate) fn raw(&self) -> Result<$raw> {
                self.m.md.row::<$raw>(self.rid)
            }
        }

        impl PartialEq for $name<'_> {
            fn eq(&self, other: &Self) -> bool {
                std::ptr::eq(self.m, other.m) && self.rid == other.rid
            }
        }
        impl Eq for $name<'_> {}

        impl std::fmt::Debug for $name<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}(rid {})", stringify!($name), self.rid)
            }
        }
    };
}

handle!(
    /// A type defined in this module (Cecil `TypeDefinition`).
    TypeDef, TypeDefRaw, TokenType::TypeDef
);
handle!(
    /// A reference to a type in another scope (Cecil `TypeReference`).
    TypeRef, TypeRefRaw, TokenType::TypeRef
);
handle!(
    /// A type described by a signature blob (Cecil `TypeSpecification` forms).
    TypeSpec, TypeSpecRaw, TokenType::TypeSpec
);
handle!(
    /// A field definition (Cecil `FieldDefinition`).
    FieldDef, FieldRaw, TokenType::Field
);
handle!(
    /// A method definition (Cecil `MethodDefinition`).
    MethodDef, MethodRaw, TokenType::Method
);
handle!(
    /// A member reference (Cecil `FieldReference`/`MethodReference` via MemberRef).
    MemberRef, MemberRefRaw, TokenType::MemberRef
);
handle!(
    /// A generic method instantiation (Cecil `GenericInstanceMethod`).
    MethodSpec, MethodSpecRaw, TokenType::MethodSpec
);
handle!(
    /// A property definition (Cecil `PropertyDefinition`).
    PropertyDef, PropertyRaw, TokenType::Property
);
handle!(
    /// An event definition (Cecil `EventDefinition`).
    EventDef, EventRaw, TokenType::Event
);
handle!(
    /// A generic parameter (Cecil `GenericParameter`).
    GenericParam, GenericParamRaw, TokenType::GenericParam
);
handle!(
    /// An exported type (Cecil `ExportedType`).
    ExportedType, ExportedTypeRaw, TokenType::ExportedType
);
handle!(
    /// A stand-alone signature (used by `calli` call sites and local vars).
    StandAloneSig, StandAloneSigRaw, TokenType::Signature
);

/// A TypeDef, TypeRef or TypeSpec — the targets of `TypeDefOrRef` coded
/// indices (Cecil passes `TypeReference` everywhere instead).
#[derive(Clone, Copy, PartialEq)]
pub enum TypeDefOrRef<'a> {
    Def(TypeDef<'a>),
    Ref(TypeRef<'a>),
    Spec(TypeSpec<'a>),
}

impl<'a> TypeDefOrRef<'a> {
    pub(crate) fn from_token(
        m: &'a ModuleData<'a>,
        token: MetadataToken,
    ) -> Result<Option<TypeDefOrRef<'a>>> {
        if token.rid() == 0 {
            return Ok(None);
        }
        Ok(Some(match token.token_type() {
            Some(TokenType::TypeDef) => TypeDefOrRef::Def(TypeDef {
                m,
                rid: token.rid(),
            }),
            Some(TokenType::TypeRef) => TypeDefOrRef::Ref(TypeRef {
                m,
                rid: token.rid(),
            }),
            Some(TokenType::TypeSpec) => TypeDefOrRef::Spec(TypeSpec {
                m,
                rid: token.rid(),
            }),
            _ => return Err(Error::Unsupported("token is not a TypeDefOrRef")),
        }))
    }

    pub fn token(&self) -> MetadataToken {
        match self {
            TypeDefOrRef::Def(t) => t.token(),
            TypeDefOrRef::Ref(t) => t.token(),
            TypeDefOrRef::Spec(t) => t.token(),
        }
    }

    pub fn name(&self) -> Result<String> {
        match self {
            TypeDefOrRef::Def(t) => t.name().map(Cow::into_owned),
            TypeDefOrRef::Ref(t) => t.name().map(Cow::into_owned),
            TypeDefOrRef::Spec(t) => t.name(),
        }
    }

    pub fn full_name(&self) -> Result<String> {
        self.full_name_ctx(GenericContext::default())
    }

    pub fn full_name_ctx(&self, ctx: GenericContext) -> Result<String> {
        let (m, token) = match self {
            TypeDefOrRef::Def(t) => (t.m, t.token()),
            TypeDefOrRef::Ref(t) => (t.m, t.token()),
            TypeDefOrRef::Spec(t) => (t.m, t.token()),
        };
        let mut out = String::new();
        token_full_name(m, token, ctx, &mut out)?;
        Ok(out)
    }

    /// The namespace and name Cecil's `IsTypeOf` compares, where they can be
    /// a plain type's: a TypeSpec reads as the type its signature names, and
    /// a generic instance, pinned or sentinel type takes its element type's.
    /// `None` for shapes whose name carries a suffix (arrays, pointers,
    /// by-refs, modifiers) or that name no type (primitives, generic
    /// parameters, function pointers).
    pub fn type_of_name(&self) -> Result<Option<(Cow<'a, str>, Cow<'a, str>)>> {
        let mut sig = match self {
            TypeDefOrRef::Def(t) => return Ok(Some((t.namespace()?, t.name()?))),
            TypeDefOrRef::Ref(t) => return Ok(Some((t.namespace()?, t.name()?))),
            TypeDefOrRef::Spec(t) => t.signature()?,
        };
        // A TypeSpec can name another TypeSpec; bound the chain by the table.
        for _ in 0..=self.m().md.row_count(Table::TypeSpec) {
            let token = loop {
                sig = match sig {
                    TypeSig::Named { token, .. } | TypeSig::GenericInst { ty: token, .. } => {
                        break token;
                    }
                    TypeSig::Pinned(inner) | TypeSig::Sentinel(inner) => *inner,
                    _ => return Ok(None),
                }
            };
            match TypeDefOrRef::from_token(self.m(), token)? {
                Some(TypeDefOrRef::Spec(t)) => sig = t.signature()?,
                Some(other) => return other.type_of_name(),
                None => return Ok(None),
            }
        }
        Err(Error::Unsupported("TypeSpec cycle"))
    }

    fn m(&self) -> &'a ModuleData<'a> {
        match self {
            TypeDefOrRef::Def(t) => t.m,
            TypeDefOrRef::Ref(t) => t.m,
            TypeDefOrRef::Spec(t) => t.m,
        }
    }

    pub fn as_def(&self) -> Option<TypeDef<'a>> {
        match self {
            TypeDefOrRef::Def(t) => Some(*t),
            _ => None,
        }
    }
}

/// `PropertyAttributes.SpecialName` and `EventAttributes.SpecialName`.
const SPECIAL_NAME: u16 = 0x200;

fn without_deleted<T>(members: Vec<T>, is_deleted: impl Fn(&T) -> Result<bool>) -> Result<Vec<T>> {
    let mut kept = Vec::with_capacity(members.len());
    for member in members {
        if !is_deleted(&member)? {
            kept.push(member);
        }
    }
    Ok(kept)
}

/// `Mono.Cecil.TypeAttributes` masks used by predicates.
pub mod type_attributes {
    pub const VISIBILITY_MASK: u32 = 0x7;
    pub const LAYOUT_MASK: u32 = 0x18;
    pub const SEMANTICS_MASK: u32 = 0x20;
    pub const ABSTRACT: u32 = 0x80;
    pub const SEALED: u32 = 0x100;
    pub const SPECIAL_NAME: u32 = 0x400;
    pub const IMPORT: u32 = 0x1000;
    pub const SERIALIZABLE: u32 = 0x2000;
    pub const WINDOWS_RUNTIME: u32 = 0x4000;
    pub const BEFORE_FIELD_INIT: u32 = 0x0010_0000;
    pub const RT_SPECIAL_NAME: u32 = 0x800;
    pub const HAS_SECURITY: u32 = 0x0004_0000;
}

impl<'a> TypeDef<'a> {
    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn namespace(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.namespace)
    }

    pub fn attributes(&self) -> Result<u32> {
        Ok(self.raw()?.flags)
    }

    pub fn full_name(&self) -> Result<String> {
        let mut out = String::new();
        typedef_full_name(self.m, self.rid, &mut out)?;
        Ok(out)
    }

    /// `MetadataReader.IsNested`.
    pub fn is_nested(&self) -> Result<bool> {
        Ok(matches!(
            self.attributes()? & type_attributes::VISIBILITY_MASK,
            2..=7
        ))
    }

    pub fn is_interface(&self) -> Result<bool> {
        Ok(self.attributes()? & type_attributes::SEMANTICS_MASK != 0)
    }

    pub fn is_abstract(&self) -> Result<bool> {
        Ok(self.attributes()? & type_attributes::ABSTRACT != 0)
    }

    pub fn is_sealed(&self) -> Result<bool> {
        Ok(self.attributes()? & type_attributes::SEALED != 0)
    }

    /// `TypeDefinition.IsValueType`: base is System.ValueType or System.Enum,
    /// except System.Enum itself.
    pub fn is_value_type(&self) -> Result<bool> {
        let Some(base) = self.base_type()? else {
            return Ok(false);
        };
        let Some((ns, name)) = base.type_of_name()? else {
            return Ok(false);
        };
        if ns != "System" {
            return Ok(false);
        }
        Ok(name == "Enum"
            || (name == "ValueType" && !(self.namespace()? == "System" && self.name()? == "Enum")))
    }

    pub fn is_enum(&self) -> Result<bool> {
        let Some(base) = self.base_type()? else {
            return Ok(false);
        };
        Ok(base
            .type_of_name()?
            .is_some_and(|(ns, name)| ns == "System" && name == "Enum"))
    }

    pub fn base_type(&self) -> Result<Option<TypeDefOrRef<'a>>> {
        TypeDefOrRef::from_token(self.m, self.raw()?.extends)
    }

    /// The enclosing type, for a type with a nested visibility.
    pub fn declaring_type(&self) -> Result<Option<TypeDef<'a>>> {
        Ok(self
            .m
            .declaring_type_rid(self.rid)?
            .map(|rid| TypeDef { m: self.m, rid }))
    }

    pub fn has_nested_types(&self) -> Result<bool> {
        Ok(self
            .m
            .nested_maps()?
            .nested
            .get(&self.rid)
            .is_some_and(|v| !v.is_empty()))
    }

    /// `GetNestedType`: the first nested type whose `TypeFullName`, its
    /// namespace and name joined by a dot (or the name alone, with no
    /// namespace), is `full_name`.
    pub fn nested_type(&self, full_name: &str) -> Result<Option<TypeDef<'a>>> {
        for nested in self.nested_types()? {
            if nested.type_full_name_is(full_name)? {
                return Ok(Some(nested));
            }
        }
        Ok(None)
    }

    fn type_full_name_is(&self, full_name: &str) -> Result<bool> {
        let (ns, name) = (self.namespace()?, self.name()?);
        Ok(if ns.is_empty() {
            name == full_name
        } else {
            full_name
                .strip_prefix(&*ns)
                .and_then(|rest| rest.strip_prefix('.'))
                == Some(&*name)
        })
    }

    /// The types whose NestedClass row names this one, without rids that name
    /// no TypeDef row: `ReadNestedTypes` drops the null that
    /// `GetTypeDefinition` returns past the table, and throws on rid 0.
    pub fn nested_types(&self) -> Result<Vec<TypeDef<'a>>> {
        Ok(self
            .m
            .nested_maps()?
            .nested
            .get(&self.rid)
            .map(|rids| {
                rids.iter()
                    .filter(|&&rid| self.m.is_type_def_rid(rid))
                    .map(|&rid| TypeDef { m: self.m, rid })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn has_interfaces(&self) -> Result<bool> {
        Ok(self.m.interfaces_map()?.contains_key(&self.rid))
    }

    pub fn interfaces(&self) -> Result<Vec<InterfaceImpl<'a>>> {
        let Some(list) = self.m.interfaces_map()?.get(&self.rid) else {
            return Ok(Vec::new());
        };
        list.iter()
            .map(|&(rid, token)| {
                Ok(InterfaceImpl {
                    token: MetadataToken::new(TokenType::InterfaceImpl, rid),
                    interface: TypeDefOrRef::from_token(self.m, token)?
                        .ok_or(Error::Unsupported("null interface token"))?,
                })
            })
            .collect()
    }

    /// `(packing_size, class_size)` from the ClassLayout table.
    pub fn class_layout(&self) -> Result<Option<(u16, u32)>> {
        Ok(self.m.class_layouts()?.get(&self.rid).copied())
    }

    /// `HasFields` of a type Cecil has not read: whether its field range,
    /// deleted fields included, is non-empty.
    pub fn has_fields(&self) -> Result<bool> {
        Ok(self.m.fields_range(self.rid)?.length > 0)
    }

    /// `HasMethods` of a type Cecil has not read, as [`TypeDef::has_fields`].
    pub fn has_methods(&self) -> Result<bool> {
        Ok(self.m.methods_range(self.rid)?.length > 0)
    }

    /// The type's fields, without deleted ones (see [`FieldDef::is_deleted`]).
    pub fn fields(&self) -> Result<Vec<FieldDef<'a>>> {
        let range = self.m.fields_range(self.rid)?;
        without_deleted(
            resolve_member_range(self.m, range, Table::FieldPtr, |rid| FieldDef {
                m: self.m,
                rid,
            })?,
            FieldDef::is_deleted,
        )
    }

    /// The type's methods, without deleted ones (see [`MethodDef::is_deleted`]).
    pub fn methods(&self) -> Result<Vec<MethodDef<'a>>> {
        let range = self.m.methods_range(self.rid)?;
        without_deleted(
            resolve_member_range(self.m, range, Table::MethodPtr, |rid| MethodDef {
                m: self.m,
                rid,
            })?,
            MethodDef::is_deleted,
        )
    }

    pub fn has_properties(&self) -> Result<bool> {
        Ok(self
            .m
            .properties_ranges()?
            .get(&self.rid)
            .is_some_and(|r| r.length > 0))
    }

    pub fn properties(&self) -> Result<Vec<PropertyDef<'a>>> {
        let Some(&range) = self.m.properties_ranges()?.get(&self.rid) else {
            return Ok(Vec::new());
        };
        without_deleted(
            resolve_member_range(self.m, range, Table::PropertyPtr, |rid| PropertyDef {
                m: self.m,
                rid,
            })?,
            PropertyDef::is_deleted,
        )
    }

    pub fn has_events(&self) -> Result<bool> {
        Ok(self
            .m
            .events_ranges()?
            .get(&self.rid)
            .is_some_and(|r| r.length > 0))
    }

    pub fn events(&self) -> Result<Vec<EventDef<'a>>> {
        let Some(&range) = self.m.events_ranges()?.get(&self.rid) else {
            return Ok(Vec::new());
        };
        without_deleted(
            resolve_member_range(self.m, range, Table::EventPtr, |rid| EventDef {
                m: self.m,
                rid,
            })?,
            EventDef::is_deleted,
        )
    }

    pub fn has_generic_params(&self) -> Result<bool> {
        has_generic_params(self.m, self.token())
    }

    pub fn generic_params(&self) -> Result<Vec<GenericParam<'a>>> {
        generic_params(self.m, self.token())
    }

    pub fn generic_context(&self) -> GenericContext {
        GenericContext::for_type(self.token())
    }
}

/// Resolves a member range through an optional Ptr indirection table
/// (`MetadataReader.ReadPointers`), without the rids that name no member:
/// the part of the range past the table it indexes, and a Ptr row naming
/// rid 0 or a rid past the member table. Cecil reads those at the wrong
/// offset, as whatever bytes are there. A range into rows the header claims
/// but the tables stream does not hold is an error, as Cecil throws reading
/// them; it fails before making a handle for each.
fn resolve_member_range<'a, H>(
    m: &'a ModuleData<'a>,
    range: Range,
    ptr_table: Table,
    make: impl Fn(u32) -> H,
) -> Result<Vec<H>> {
    let target = match ptr_table {
        Table::FieldPtr => Table::Field,
        Table::MethodPtr => Table::Method,
        Table::EventPtr => Table::Event,
        _ => Table::Property,
    };
    let ptr_rows = m.md.row_count(ptr_table);
    let target_rows = m.md.row_count(target);
    let indexed = if ptr_rows != 0 { ptr_table } else { target };
    let end = (range.start + range.length).min(m.md.row_count(indexed).saturating_add(1));
    if end > m.md.rows_present(indexed).saturating_add(1) {
        return Err(Error::bad(0, "member list runs past the tables stream"));
    }
    let mut out = Vec::with_capacity(range.length.min(4096) as usize);
    for rid in range.start..end {
        let actual = if ptr_rows != 0 {
            match ptr_table {
                Table::FieldPtr => m.md.row::<FieldPtrRaw>(rid)?.field,
                Table::MethodPtr => m.md.row::<MethodPtrRaw>(rid)?.method,
                Table::EventPtr => m.md.row::<EventPtrRaw>(rid)?.event,
                _ => m.md.row::<PropertyPtrRaw>(rid)?.property,
            }
        } else {
            rid
        };
        if (1..=target_rows).contains(&actual) {
            out.push(make(actual));
        }
    }
    Ok(out)
}

fn has_generic_params(m: &ModuleData<'_>, owner: MetadataToken) -> Result<bool> {
    Ok(m.generic_param_ranges()?
        .get(&owner)
        .is_some_and(|ranges| ranges.iter().any(|r| r.length > 0)))
}

fn generic_params<'a>(
    m: &'a ModuleData<'a>,
    owner: MetadataToken,
) -> Result<Vec<GenericParam<'a>>> {
    let Some(ranges) = m.generic_param_ranges()?.get(&owner) else {
        return Ok(Vec::new());
    };
    Ok(ranges
        .iter()
        .flat_map(|r| r.iter())
        .map(|rid| GenericParam { m, rid })
        .collect())
}

#[derive(Clone, Copy, PartialEq)]
pub struct InterfaceImpl<'a> {
    pub token: MetadataToken,
    pub interface: TypeDefOrRef<'a>,
}

impl<'a> TypeRef<'a> {
    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn namespace(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.namespace)
    }

    pub fn full_name(&self) -> Result<String> {
        let mut out = String::new();
        typeref_full_name(self.m, self.rid, &mut out)?;
        Ok(out)
    }

    /// The raw ResolutionScope token.
    pub fn scope_token(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.scope)
    }

    /// The scope's display name (assembly ref name, module ref name, module
    /// name, or the outermost declaring TypeRef's scope for nested
    /// references). Cecil's `Scope` recurses through `DeclaringType`, and
    /// overflows its stack on a cycle; here a cycle is an error.
    pub fn scope_name(&self) -> Result<String> {
        let mut outermost = *self;
        let mut steps = 0;
        while let Some(declaring) = outermost.declaring_type()? {
            steps += 1;
            if steps == self.m.md.row_count(Table::TypeRef) {
                return Err(Error::Unsupported("TypeRef scope cycle"));
            }
            outermost = declaring;
        }
        let scope = outermost.raw()?.scope;
        match scope.token_type() {
            Some(TokenType::AssemblyRef) => {
                let row = self.m.md.row::<AssemblyRefRaw>(scope.rid())?;
                Ok(self.m.md.strings().get(row.name)?.to_string())
            }
            Some(TokenType::ModuleRef) => {
                let row = self.m.md.row::<ModuleRefRaw>(scope.rid())?;
                Ok(self.m.md.strings().get(row.name)?.to_string())
            }
            _ => {
                let row = self.m.md.row::<ModuleRaw>(1)?;
                Ok(self.m.md.strings().get(row.name)?.to_string())
            }
        }
    }

    /// The primitive this reference names, as Cecil's
    /// `MetadataSystem.TryProcessPrimitiveTypeReference` gives it an element
    /// type: a type in namespace `System`, whose scope (through any declaring
    /// TypeRefs) is an assembly reference, named after a primitive. ilasm and
    /// other non-Roslyn producers write `valuetype [mscorlib]System.Int32`.
    pub fn primitive(&self) -> Result<Option<TypeSig>> {
        if self.namespace()? != "System" {
            return Ok(None);
        }
        // Follow declaring TypeRefs to the root scope, as Cecil's
        // DeclaringType.Scope does; a cycle has no assembly scope.
        let mut scope = self.raw()?.scope;
        let mut current = self.rid;
        for _ in 0..self.m.md.row_count(Table::TypeRef) {
            let Some(declaring) = self.m.typeref_declaring_rid(current, scope) else {
                break;
            };
            current = declaring;
            scope = (TypeRef {
                m: self.m,
                rid: current,
            })
            .raw()?
            .scope;
        }
        if scope.token_type() != Some(TokenType::AssemblyRef) {
            return Ok(None);
        }
        Ok(Some(match &*self.name()? {
            "Void" => TypeSig::Void,
            "Boolean" => TypeSig::Boolean,
            "Char" => TypeSig::Char,
            "SByte" => TypeSig::I1,
            "Byte" => TypeSig::U1,
            "Int16" => TypeSig::I2,
            "UInt16" => TypeSig::U2,
            "Int32" => TypeSig::I4,
            "UInt32" => TypeSig::U4,
            "Int64" => TypeSig::I8,
            "UInt64" => TypeSig::U8,
            "Single" => TypeSig::R4,
            "Double" => TypeSig::R8,
            "String" => TypeSig::String,
            "TypedReference" => TypeSig::TypedByRef,
            "IntPtr" => TypeSig::IntPtr,
            "UIntPtr" => TypeSig::UIntPtr,
            "Object" => TypeSig::Object,
            _ => return Ok(None),
        }))
    }

    pub fn declaring_type(&self) -> Result<Option<TypeRef<'a>>> {
        let scope = self.raw()?.scope;
        Ok(self
            .m
            .typeref_declaring_rid(self.rid, scope)
            .map(|rid| TypeRef { m: self.m, rid }))
    }
}

impl<'a> TypeSpec<'a> {
    pub fn signature(&self) -> Result<TypeSig> {
        let blob = self.m.md.blobs().view(self.raw()?.signature)?;
        SigReader::new(blob).type_sig()
    }

    pub fn name(&self) -> Result<String> {
        // Close enough for display purposes: the full name's last segment
        // semantics are not needed by the read tests; use full_name.
        self.full_name()
    }

    pub fn full_name(&self) -> Result<String> {
        self.full_name_ctx(GenericContext::default())
    }

    pub fn full_name_ctx(&self, ctx: GenericContext) -> Result<String> {
        let mut out = String::new();
        sig_full_name(self.m, &self.signature()?, ctx, &mut out)?;
        Ok(out)
    }
}

/// `Mono.Cecil.FieldAttributes` masks.
pub mod field_attributes {
    pub const FIELD_ACCESS_MASK: u16 = 0x7;
    pub const STATIC: u16 = 0x10;
    pub const INIT_ONLY: u16 = 0x20;
    pub const LITERAL: u16 = 0x40;
    pub const NOT_SERIALIZED: u16 = 0x80;
    pub const SPECIAL_NAME: u16 = 0x200;
    pub const PINVOKE_IMPL: u16 = 0x2000;
    pub const RT_SPECIAL_NAME: u16 = 0x400;
    pub const HAS_FIELD_MARSHAL: u16 = 0x1000;
    pub const HAS_DEFAULT: u16 = 0x8000;
    pub const HAS_FIELD_RVA: u16 = 0x100;
}

impl<'a> FieldDef<'a> {
    /// Cecil's `IsDeleted`: a SpecialName member named `_Deleted`, which
    /// edit-and-continue leaves behind. Cecil reads it, but leaves it out of
    /// its type's members.
    pub fn is_deleted(&self) -> Result<bool> {
        Ok(self.attributes()? & field_attributes::SPECIAL_NAME != 0 && self.name()? == "_Deleted")
    }

    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn attributes(&self) -> Result<u16> {
        Ok(self.raw()?.flags)
    }

    pub fn is_static(&self) -> Result<bool> {
        Ok(self.attributes()? & field_attributes::STATIC != 0)
    }

    pub fn is_literal(&self) -> Result<bool> {
        Ok(self.attributes()? & field_attributes::LITERAL != 0)
    }

    pub fn field_type(&self) -> Result<TypeSig> {
        let blob = self.m.md.blobs().view(self.raw()?.signature)?;
        SigReader::new(blob).field_sig()
    }

    pub fn declaring_type(&self) -> Result<Option<TypeDef<'a>>> {
        Ok(self
            .m
            .member_declaring_type(self.rid, true)?
            .map(|rid| TypeDef { m: self.m, rid }))
    }

    pub fn full_name(&self) -> Result<String> {
        // FieldReference.FullName: "<type> <declaring>::<name>".
        let ctx = self.generic_context()?;
        let mut out = String::new();
        sig_full_name(self.m, &self.field_type()?, ctx, &mut out)?;
        out.push(' ');
        if let Some(decl) = self.declaring_type()? {
            typedef_full_name(self.m, decl.rid, &mut out)?;
            out.push_str("::");
        }
        out.push_str(&self.name()?);
        Ok(out)
    }

    fn generic_context(&self) -> Result<GenericContext> {
        Ok(match self.declaring_type()? {
            Some(t) => GenericContext::for_type(t.token()),
            None => GenericContext::default(),
        })
    }

    /// Field offset from the FieldLayout table.
    pub fn offset(&self) -> Result<Option<u32>> {
        Ok(self.m.field_layouts()?.get(&self.rid).copied())
    }

    /// `FieldDefinition.RVA`: the FieldRVA row's RVA, or `None` where Cecil
    /// reports 0: no row, an RVA of 0, or a field type of no known size
    /// (`MetadataReader.ReadFieldRVA`).
    pub fn rva(&self) -> Result<Option<u32>> {
        let Some(&rva) = self.m.field_rvas()?.get(&self.rid) else {
            return Ok(None);
        };
        if rva == 0 || self.field_type_size(&self.field_type()?)? == 0 {
            return Ok(None);
        }
        Ok(Some(rva))
    }

    /// The field's initial data (FieldRVA), sized by the field type
    /// (`MetadataReader.GetFieldTypeSize`; type-layout-sized valuetypes only
    /// resolve in-module).
    pub fn initial_value(&self) -> Result<Option<&'a [u8]>> {
        let Some(rva) = self.rva()? else {
            return Ok(None);
        };
        let size = self.field_type_size(&self.field_type()?)?;
        // Cecil's GetFieldInitializeValue returns an empty array when the RVA
        // does not land in any section.
        Ok(Some(self.m.image.slice_at_rva(rva, size).unwrap_or(&[])))
    }

    fn field_type_size(&self, ty: &TypeSig) -> Result<usize> {
        Ok(match ty {
            TypeSig::Boolean | TypeSig::U1 | TypeSig::I1 => 1,
            TypeSig::U2 | TypeSig::I2 | TypeSig::Char => 2,
            TypeSig::U4 | TypeSig::I4 | TypeSig::R4 => 4,
            TypeSig::U8 | TypeSig::I8 | TypeSig::R8 => 8,
            // Cecil uses IntPtr.Size (runtime pointer size); we are reading a
            // fixed image, use 8 as CoreCLR-on-64-bit would.
            TypeSig::Ptr(_) | TypeSig::FnPtr(_) => 8,
            TypeSig::CModReq { inner, .. } | TypeSig::CModOpt { inner, .. } => {
                self.field_type_size(inner)?
            }
            TypeSig::Named { token, .. } if token.token_type() == Some(TokenType::TypeRef) => {
                match (TypeRef {
                    m: self.m,
                    rid: token.rid(),
                })
                .primitive()?
                {
                    Some(primitive) => self.field_type_size(&primitive)?,
                    None => 0,
                }
            }
            // A generic instance resolves to its generic type, as Cecil's
            // GenericInstanceType.Resolve() does.
            TypeSig::Named { token, .. } | TypeSig::GenericInst { ty: token, .. }
                if token.token_type() == Some(TokenType::TypeDef) =>
            {
                let td = TypeDef {
                    m: self.m,
                    rid: token.rid(),
                };
                td.class_layout()?
                    .map(|(_, size)| size as usize)
                    .unwrap_or(0)
            }
            _ => 0,
        })
    }

    pub fn has_constant(&self) -> Result<bool> {
        Ok(self.m.constants()?.contains_key(&self.token()))
    }

    pub fn constant(&self) -> Result<Option<ConstantValue<'a>>> {
        read_constant(self.m, self.token())
    }

    pub fn marshal_blob(&self) -> Result<Option<&'a [u8]>> {
        marshal_blob(self.m, self.token())
    }

    pub fn has_marshal_info(&self) -> Result<bool> {
        Ok(self.m.field_marshals()?.contains_key(&self.token()))
    }

    pub fn marshal_info(&self) -> Result<Option<MarshalInfo<'a>>> {
        match self.marshal_blob()? {
            Some(blob) => Ok(Some(MarshalInfo::parse(blob)?)),
            None => Ok(None),
        }
    }
}

/// A constant from the Constant table (Cecil surfaces these as `object`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstantValue<'a> {
    Boolean(bool),
    Char(u16),
    I1(i8),
    U1(u8),
    I2(i16),
    U2(u16),
    I4(i32),
    U4(u32),
    I8(i64),
    U8(u64),
    R4(f32),
    R8(f64),
    String(Utf16Str<'a>),
    /// ELEMENT_TYPE_CLASS / null string constants.
    Null,
}

pub(crate) fn read_constant<'a>(
    m: &'a ModuleData<'a>,
    owner: MetadataToken,
) -> Result<Option<ConstantValue<'a>>> {
    let Some(&(etype, blob_index)) = m.constants()?.get(&owner) else {
        return Ok(None);
    };
    // MetadataReader.ReadConstantValue
    let et = ElementType::from_byte(etype);
    Ok(Some(match et {
        Some(ElementType::Class) | Some(ElementType::Object) => ConstantValue::Null,
        Some(ElementType::String) => {
            // ReadConstantString: UTF-16 bytes. An empty blob (index 0 is the
            // heap's zero-length entry, not an absent blob) is "", not null —
            // a null string constant is ELEMENT_TYPE_CLASS, handled above.
            let blob = m.md.blobs().get_exact(blob_index)?;
            ConstantValue::String(Utf16Str {
                bytes: &blob[..blob.len() & !1],
            })
        }
        _ => {
            let blob = m.md.blobs().view(blob_index)?;
            let mut cur = crate::raw::cursor::Cur::new(blob);
            match et {
                Some(ElementType::Boolean) => ConstantValue::Boolean(cur.u8()? == 1),
                Some(ElementType::I1) => ConstantValue::I1(cur.i8()?),
                Some(ElementType::U1) => ConstantValue::U1(cur.u8()?),
                Some(ElementType::Char) => ConstantValue::Char(cur.u16()?),
                Some(ElementType::I2) => ConstantValue::I2(cur.i16()?),
                Some(ElementType::U2) => ConstantValue::U2(cur.u16()?),
                Some(ElementType::I4) => ConstantValue::I4(cur.i32()?),
                Some(ElementType::U4) => ConstantValue::U4(cur.u32()?),
                Some(ElementType::I8) => ConstantValue::I8(cur.i64()?),
                Some(ElementType::U8) => ConstantValue::U8(cur.u64()?),
                Some(ElementType::R4) => ConstantValue::R4(cur.f32()?),
                Some(ElementType::R8) => ConstantValue::R8(cur.f64()?),
                _ => return Err(Error::Unsupported("unknown constant element type")),
            }
        }
    }))
}

/// The methods of property or event `owner` with exactly the `semantics`
/// attributes, as Cecil attaches them (`MetadataReader.ReadAllSemantics`):
/// only the declaring type's methods count, in method order, each by its
/// last MethodSemantics row. An accessor takes the last of them.
fn semantic_methods<'a>(
    m: &'a ModuleData<'a>,
    owner: MetadataToken,
    declaring: Option<TypeDef<'a>>,
    semantics: u16,
) -> Result<Vec<MethodDef<'a>>> {
    let Some(declaring) = declaring else {
        return Ok(Vec::new());
    };
    let by_method = &m.semantics()?.by_method;
    Ok(declaring
        .methods()?
        .into_iter()
        .filter(|method| by_method.get(&method.rid) == Some(&(semantics, owner)))
        .collect())
}

fn marshal_blob<'a>(m: &'a ModuleData<'a>, owner: MetadataToken) -> Result<Option<&'a [u8]>> {
    let Some(&blob_index) = m.field_marshals()?.get(&owner) else {
        return Ok(None);
    };
    Ok(Some(m.md.blobs().view(blob_index)?))
}

/// `Mono.Cecil.MethodAttributes` masks.
pub mod method_attributes {
    pub const MEMBER_ACCESS_MASK: u16 = 0x7;
    pub const STATIC: u16 = 0x10;
    pub const FINAL: u16 = 0x20;
    pub const VIRTUAL: u16 = 0x40;
    pub const HIDE_BY_SIG: u16 = 0x80;
    pub const NEW_SLOT: u16 = 0x100;
    pub const CHECK_ACCESS_ON_OVERRIDE: u16 = 0x200;
    pub const ABSTRACT: u16 = 0x400;
    pub const SPECIAL_NAME: u16 = 0x800;
    pub const PINVOKE_IMPL: u16 = 0x2000;
    pub const RT_SPECIAL_NAME: u16 = 0x1000;
    pub const HAS_SECURITY: u16 = 0x4000;
}

/// `MethodSemanticsAttributes`.
pub mod method_semantics {
    pub const SETTER: u16 = 0x1;
    pub const GETTER: u16 = 0x2;
    pub const OTHER: u16 = 0x4;
    pub const ADD_ON: u16 = 0x8;
    pub const REMOVE_ON: u16 = 0x10;
    pub const FIRE: u16 = 0x20;
}

/// A method parameter joined from the signature and the Param table
/// (Cecil `ParameterDefinition`).
#[derive(Clone)]
pub struct Parameter<'a> {
    pub(crate) m: &'a ModuleData<'a>,
    /// 0-based position in the parameter list.
    pub index: usize,
    pub name: Cow<'a, str>,
    pub attributes: u16,
    /// The Param row token, if a row exists for this parameter.
    pub token: Option<MetadataToken>,
    pub param_type: TypeSig,
}

impl<'a> Parameter<'a> {
    pub fn has_constant(&self) -> Result<bool> {
        match self.token {
            Some(token) => Ok(self.m.constants()?.contains_key(&token)),
            None => Ok(false),
        }
    }

    pub fn constant(&self) -> Result<Option<ConstantValue<'a>>> {
        match self.token {
            Some(token) => read_constant(self.m, token),
            None => Ok(None),
        }
    }

    pub fn has_marshal_info(&self) -> Result<bool> {
        match self.token {
            Some(token) => Ok(self.m.field_marshals()?.contains_key(&token)),
            None => Ok(false),
        }
    }

    pub fn marshal_info(&self) -> Result<Option<MarshalInfo<'a>>> {
        match self.marshal_blob()? {
            Some(blob) => Ok(Some(MarshalInfo::parse(blob)?)),
            None => Ok(None),
        }
    }

    pub fn marshal_blob(&self) -> Result<Option<&'a [u8]>> {
        match self.token {
            Some(token) => marshal_blob(self.m, token),
            None => Ok(None),
        }
    }
}

pub mod param_attributes {
    pub const IN: u16 = 0x1;
    pub const OUT: u16 = 0x2;
    pub const OPTIONAL: u16 = 0x10;
    pub const HAS_DEFAULT: u16 = 0x1000;
    pub const HAS_FIELD_MARSHAL: u16 = 0x2000;
}

impl<'a> MethodDef<'a> {
    /// Cecil's `IsDeleted`: a SpecialName member named `_Deleted`, which
    /// edit-and-continue leaves behind. Cecil reads it, but leaves it out of
    /// its type's members.
    pub fn is_deleted(&self) -> Result<bool> {
        Ok(self.attributes()? & method_attributes::SPECIAL_NAME != 0 && self.name()? == "_Deleted")
    }

    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn attributes(&self) -> Result<u16> {
        Ok(self.raw()?.flags)
    }

    pub fn impl_attributes(&self) -> Result<u16> {
        Ok(self.raw()?.impl_flags)
    }

    pub fn rva(&self) -> Result<u32> {
        Ok(self.raw()?.rva)
    }

    pub fn is_static(&self) -> Result<bool> {
        Ok(self.attributes()? & method_attributes::STATIC != 0)
    }

    pub fn is_constructor(&self) -> Result<bool> {
        let attrs = self.attributes()?;
        Ok(attrs & method_attributes::RT_SPECIAL_NAME != 0
            && attrs & method_attributes::SPECIAL_NAME != 0
            && matches!(&*self.name()?, ".ctor" | ".cctor"))
    }

    /// `MethodDefinition.HasBody` (`MethodDefinition.cs:139-148`): not
    /// abstract/pinvoke, and impl is managed IL (not Native/Unmanaged/Runtime/
    /// InternalCall). Note Cecil does NOT check the RVA here; reading the body
    /// of an rva-0 method yields an empty body.
    pub fn has_body(&self) -> Result<bool> {
        let raw = self.raw()?;
        Ok(raw.flags & method_attributes::ABSTRACT == 0
            && raw.flags & method_attributes::PINVOKE_IMPL == 0
            && raw.impl_flags & 0x1000 == 0 // InternalCall
            && raw.impl_flags & 0x7 == 0) // Native | OPTIL/Runtime | Unmanaged
    }

    pub fn signature(&self) -> Result<MethodSig> {
        let blob = self.m.md.blobs().view(self.raw()?.signature)?;
        SigReader::new(blob).method_sig()
    }

    pub fn return_type(&self) -> Result<TypeSig> {
        Ok(self.signature()?.return_type)
    }

    pub fn declaring_type(&self) -> Result<Option<TypeDef<'a>>> {
        Ok(self
            .m
            .member_declaring_type(self.rid, false)?
            .map(|rid| TypeDef { m: self.m, rid }))
    }

    pub fn generic_context(&self) -> Result<GenericContext> {
        Ok(GenericContext {
            type_owner: self.declaring_type()?.map(|t| t.token()),
            method_owner: Some(self.rid),
        })
    }

    /// The Param table range for this method (peeking the next row).
    fn param_range(&self) -> Result<Range> {
        let table = self.m.md.table::<MethodRaw>();
        let row = self.raw()?;
        let start = row.param_list;
        if start == 0 {
            return Ok(Range::default());
        }
        let ptr_rows = self.m.md.row_count(Table::ParamPtr);
        let end = if self.rid == table.len() {
            (if ptr_rows != 0 {
                ptr_rows
            } else {
                self.m.md.row_count(Table::Param)
            })
            .saturating_add(1)
        } else {
            table.row(self.rid + 1)?.param_list
        };
        Ok(Range {
            start,
            length: end.saturating_sub(start),
        })
    }

    /// Parameters: signature types joined with Param rows by sequence.
    pub fn parameters(&self) -> Result<Vec<Parameter<'a>>> {
        let sig = self.signature()?;
        let mut params: Vec<Parameter<'a>> = sig
            .params
            .into_iter()
            .enumerate()
            .map(|(i, ty)| Parameter {
                m: self.m,
                index: i,
                name: Cow::Borrowed(""),
                attributes: 0,
                token: None,
                param_type: ty,
            })
            .collect();
        let range = self.param_range()?;
        let has_ptr = self.m.md.row_count(Table::ParamPtr) != 0;
        for rid in range.iter() {
            let actual = if has_ptr {
                self.m.md.row::<ParamPtrRaw>(rid)?.param
            } else {
                rid
            };
            let row = self.m.md.row::<ParamRaw>(actual)?;
            if row.sequence == 0 {
                continue; // return-type parameter row
            }
            if let Some(p) = params.get_mut(row.sequence as usize - 1) {
                p.name = self.m.md.strings().get(row.name)?;
                p.attributes = row.flags;
                p.token = Some(MetadataToken::new(TokenType::Param, actual));
            }
        }
        Ok(params)
    }

    /// The Param row attached to the return type (sequence 0), if present.
    pub fn return_param(&self) -> Result<Option<(MetadataToken, u16)>> {
        let range = self.param_range()?;
        let has_ptr = self.m.md.row_count(Table::ParamPtr) != 0;
        for rid in range.iter() {
            let actual = if has_ptr {
                self.m.md.row::<ParamPtrRaw>(rid)?.param
            } else {
                rid
            };
            let row = self.m.md.row::<ParamRaw>(actual)?;
            if row.sequence == 0 {
                return Ok(Some((
                    MetadataToken::new(TokenType::Param, actual),
                    row.flags,
                )));
            }
        }
        Ok(None)
    }

    /// Marshal info attached to the return type (sequence-0 Param row).
    pub fn return_param_marshal_info(&self) -> Result<Option<MarshalInfo<'a>>> {
        let Some((token, _)) = self.return_param()? else {
            return Ok(None);
        };
        match marshal_blob(self.m, token)? {
            Some(blob) => Ok(Some(MarshalInfo::parse(blob)?)),
            None => Ok(None),
        }
    }

    /// `MethodReference.FullName`: "<ret> <declaring>::<name>(<params>)".
    pub fn full_name(&self) -> Result<String> {
        let ctx = self.generic_context()?;
        let sig = self.signature()?;
        let mut out = String::new();
        sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
        out.push(' ');
        if let Some(decl) = self.declaring_type()? {
            typedef_full_name(self.m, decl.rid, &mut out)?;
            out.push_str("::");
        }
        out.push_str(&self.name()?);
        method_params_full_name(self.m, &sig, ctx, &mut out)?;
        Ok(out)
    }

    pub fn has_generic_params(&self) -> Result<bool> {
        has_generic_params(self.m, self.token())
    }

    pub fn generic_params(&self) -> Result<Vec<GenericParam<'a>>> {
        generic_params(self.m, self.token())
    }

    pub fn has_overrides(&self) -> Result<bool> {
        Ok(self.m.overrides()?.contains_key(&self.rid))
    }

    pub fn overrides(&self) -> Result<Vec<MetadataToken>> {
        Ok(self
            .m
            .overrides()?
            .get(&self.rid)
            .cloned()
            .unwrap_or_default())
    }

    /// PInvoke info (`MethodDefinition.PInvokeInfo`).
    pub fn pinvoke_info(&self) -> Result<Option<PInvokeInfo<'a>>> {
        let Some(&(attrs, name_idx, scope_rid)) = self.m.pinvokes()?.get(&self.rid) else {
            return Ok(None);
        };
        let module_row = self.m.md.row::<ModuleRefRaw>(scope_rid)?;
        Ok(Some(PInvokeInfo {
            attributes: attrs,
            entry_point: self.m.md.strings().get(name_idx)?,
            module: self.m.md.strings().get(module_row.name)?,
        }))
    }

    /// Semantics attributes from the MethodSemantics table (0 if none).
    pub fn semantics(&self) -> Result<u16> {
        Ok(self
            .m
            .semantics()?
            .by_method
            .get(&self.rid)
            .map(|&(attrs, _)| attrs)
            .unwrap_or(0))
    }

    pub fn is_getter(&self) -> Result<bool> {
        Ok(self.semantics()? & method_semantics::GETTER != 0)
    }

    pub fn is_setter(&self) -> Result<bool> {
        Ok(self.semantics()? & method_semantics::SETTER != 0)
    }

    pub fn is_add_on(&self) -> Result<bool> {
        Ok(self.semantics()? & method_semantics::ADD_ON != 0)
    }

    pub fn is_remove_on(&self) -> Result<bool> {
        Ok(self.semantics()? & method_semantics::REMOVE_ON != 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PInvokeInfo<'a> {
    pub attributes: u16,
    pub entry_point: Cow<'a, str>,
    pub module: Cow<'a, str>,
}

impl<'a> PropertyDef<'a> {
    /// Cecil's `IsDeleted`: a SpecialName member named `_Deleted`, which
    /// edit-and-continue leaves behind. Cecil reads it, but leaves it out of
    /// its type's members.
    pub fn is_deleted(&self) -> Result<bool> {
        Ok(self.attributes()? & SPECIAL_NAME != 0 && self.name()? == "_Deleted")
    }

    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn attributes(&self) -> Result<u16> {
        Ok(self.raw()?.flags)
    }

    pub fn signature(&self) -> Result<PropertySig> {
        let blob = self.m.md.blobs().view(self.raw()?.ty)?;
        SigReader::new(blob).property_sig()
    }

    pub fn declaring_type(&self) -> Result<Option<TypeDef<'a>>> {
        // Map ranges index PropertyPtr when it exists.
        let Some(index) = self.m.member_list_index(Table::PropertyPtr, self.rid)? else {
            return Ok(None);
        };
        for (&type_rid, range) in self.m.properties_ranges()? {
            if index >= range.start && index < range.start + range.length {
                return Ok(Some(TypeDef {
                    m: self.m,
                    rid: type_rid,
                }));
            }
        }
        Ok(None)
    }

    fn accessor(&self, semantics: u16) -> Result<Option<MethodDef<'a>>> {
        Ok(self.semantic_methods(semantics)?.pop())
    }

    fn semantic_methods(&self, semantics: u16) -> Result<Vec<MethodDef<'a>>> {
        semantic_methods(self.m, self.token(), self.declaring_type()?, semantics)
    }

    pub fn get_method(&self) -> Result<Option<MethodDef<'a>>> {
        self.accessor(method_semantics::GETTER)
    }

    pub fn set_method(&self) -> Result<Option<MethodDef<'a>>> {
        self.accessor(method_semantics::SETTER)
    }

    /// Methods attached with `Other` semantics, in method order.
    pub fn other_methods(&self) -> Result<Vec<MethodDef<'a>>> {
        self.semantic_methods(method_semantics::OTHER)
    }

    pub fn has_constant(&self) -> Result<bool> {
        Ok(self.m.constants()?.contains_key(&self.token()))
    }

    pub fn constant(&self) -> Result<Option<ConstantValue<'a>>> {
        read_constant(self.m, self.token())
    }

    /// `PropertyDefinition.Parameters`: the getter's parameter types, or the
    /// setter's without the value; none without either. The signature's own
    /// parameters do not count.
    pub fn parameters(&self) -> Result<Vec<TypeSig>> {
        if let Some(getter) = self.get_method()? {
            return Ok(getter.signature()?.params);
        }
        if let Some(setter) = self.set_method()? {
            let mut params = setter.signature()?.params;
            params.pop();
            return Ok(params);
        }
        Ok(Vec::new())
    }

    /// `PropertyDefinition.FullName`: "<type> <declaring>::<name>(<params>)",
    /// with the [`parameters`](Self::parameters).
    pub fn full_name(&self) -> Result<String> {
        let sig = self.signature()?;
        let ctx = match self.declaring_type()? {
            Some(t) => GenericContext::for_type(t.token()),
            None => GenericContext::default(),
        };
        let mut out = String::new();
        sig_full_name(self.m, &sig.property_type, ctx, &mut out)?;
        out.push(' ');
        if let Some(decl) = self.declaring_type()? {
            typedef_full_name(self.m, decl.rid, &mut out)?;
            out.push_str("::");
        }
        out.push_str(&self.name()?);
        out.push('(');
        for (i, p) in self.parameters()?.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            sig_full_name(self.m, p, ctx, &mut out)?;
        }
        out.push(')');
        Ok(out)
    }
}

impl<'a> EventDef<'a> {
    /// Cecil's `IsDeleted`: a SpecialName member named `_Deleted`, which
    /// edit-and-continue leaves behind. Cecil reads it, but leaves it out of
    /// its type's members.
    pub fn is_deleted(&self) -> Result<bool> {
        Ok(self.attributes()? & SPECIAL_NAME != 0 && self.name()? == "_Deleted")
    }

    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn attributes(&self) -> Result<u16> {
        Ok(self.raw()?.flags)
    }

    pub fn event_type(&self) -> Result<Option<TypeDefOrRef<'a>>> {
        TypeDefOrRef::from_token(self.m, self.raw()?.event_type)
    }

    pub fn declaring_type(&self) -> Result<Option<TypeDef<'a>>> {
        // Map ranges index EventPtr when it exists.
        let Some(index) = self.m.member_list_index(Table::EventPtr, self.rid)? else {
            return Ok(None);
        };
        for (&type_rid, range) in self.m.events_ranges()? {
            if index >= range.start && index < range.start + range.length {
                return Ok(Some(TypeDef {
                    m: self.m,
                    rid: type_rid,
                }));
            }
        }
        Ok(None)
    }

    fn accessor(&self, semantics: u16) -> Result<Option<MethodDef<'a>>> {
        Ok(self.semantic_methods(semantics)?.pop())
    }

    fn semantic_methods(&self, semantics: u16) -> Result<Vec<MethodDef<'a>>> {
        semantic_methods(self.m, self.token(), self.declaring_type()?, semantics)
    }

    pub fn add_method(&self) -> Result<Option<MethodDef<'a>>> {
        self.accessor(method_semantics::ADD_ON)
    }

    pub fn remove_method(&self) -> Result<Option<MethodDef<'a>>> {
        self.accessor(method_semantics::REMOVE_ON)
    }

    pub fn invoke_method(&self) -> Result<Option<MethodDef<'a>>> {
        self.accessor(method_semantics::FIRE)
    }

    /// Methods attached with `Other` semantics, in method order.
    pub fn other_methods(&self) -> Result<Vec<MethodDef<'a>>> {
        self.semantic_methods(method_semantics::OTHER)
    }
}

pub mod generic_param_attributes {
    pub const VARIANCE_MASK: u16 = 0x3;
    pub const COVARIANT: u16 = 0x1;
    pub const CONTRAVARIANT: u16 = 0x2;
    pub const REFERENCE_TYPE_CONSTRAINT: u16 = 0x4;
    pub const NOT_NULLABLE_VALUE_TYPE_CONSTRAINT: u16 = 0x8;
    pub const DEFAULT_CONSTRUCTOR_CONSTRAINT: u16 = 0x10;
}

/// One generic-parameter constraint (Cecil `GenericParameterConstraint`).
#[derive(Clone, Copy, PartialEq)]
pub struct GenericConstraint<'a> {
    pub token: MetadataToken,
    pub constraint: TypeDefOrRef<'a>,
}

impl<'a> GenericParam<'a> {
    /// `GenericParameter.Name`: the row's name, or `!n` (`!!n` for a
    /// method's) by position in the owner's parameters when it is empty.
    pub fn name(&self) -> Result<Cow<'a, str>> {
        let name = self.m.md.strings().get(self.raw()?.name)?;
        if !name.is_empty() {
            return Ok(name);
        }
        let owner = self.owner()?;
        let position = generic_params(self.m, owner)?
            .iter()
            .position(|gp| gp.rid == self.rid)
            .unwrap_or(0);
        let prefix = if owner.token_type() == Some(TokenType::Method) {
            "!!"
        } else {
            "!"
        };
        Ok(Cow::Owned(format!("{prefix}{position}")))
    }

    pub fn number(&self) -> Result<u16> {
        Ok(self.raw()?.number)
    }

    pub fn attributes(&self) -> Result<u16> {
        Ok(self.raw()?.flags)
    }

    pub fn owner(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.owner)
    }

    pub fn has_constraints(&self) -> Result<bool> {
        Ok(self.m.generic_constraints()?.contains_key(&self.rid))
    }

    pub fn constraints(&self) -> Result<Vec<GenericConstraint<'a>>> {
        let Some(list) = self.m.generic_constraints()?.get(&self.rid) else {
            return Ok(Vec::new());
        };
        list.iter()
            .map(|&(rid, token)| {
                Ok(GenericConstraint {
                    token: MetadataToken::new(TokenType::GenericParamConstraint, rid),
                    constraint: TypeDefOrRef::from_token(self.m, token)?
                        .ok_or(Error::Unsupported("null constraint token"))?,
                })
            })
            .collect()
    }
}

/// The decoded shape of a MemberRef's signature.
pub enum MemberRefSig {
    Field(TypeSig),
    Method(MethodSig),
}

impl<'a> MemberRef<'a> {
    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    /// The MemberRefParent token.
    pub fn class_token(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.class)
    }

    pub fn signature(&self) -> Result<MemberRefSig> {
        let blob = self.m.md.blobs().view(self.raw()?.signature)?;
        const FIELD_SIG: u8 = 0x6;
        if blob.first() == Some(&FIELD_SIG) {
            let mut r = SigReader::new(blob);
            r.cur.skip(1)?;
            Ok(MemberRefSig::Field(r.type_sig()?))
        } else {
            Ok(MemberRefSig::Method(SigReader::new(blob).method_sig()?))
        }
    }

    /// The declaring type's full name; for a Method parent (vararg call to a
    /// MethodDef), the method's declaring type.
    pub(crate) fn declaring_full_name(&self, ctx: GenericContext, out: &mut String) -> Result<()> {
        let class = self.raw()?.class;
        match class.token_type() {
            Some(TokenType::TypeDef) | Some(TokenType::TypeRef) | Some(TokenType::TypeSpec) => {
                token_full_name(self.m, class, ctx, out)
            }
            Some(TokenType::Method) => {
                let method = MethodDef {
                    m: self.m,
                    rid: class.rid(),
                };
                match method.declaring_type()? {
                    Some(t) => typedef_full_name(self.m, t.rid, out),
                    None => Ok(()),
                }
            }
            Some(TokenType::ModuleRef) => {
                // Global function imported from another module: Cecil models
                // the declaring type as ModuleRef; render its name.
                let row = self.m.md.row::<ModuleRefRaw>(class.rid())?;
                out.push_str(&self.m.md.strings().get(row.name)?);
                Ok(())
            }
            _ => Err(Error::Unsupported("unsupported MemberRef parent")),
        }
    }

    /// The generic context Cecil would use when reading this member's
    /// signature: the declaring type (for Var) — MVar stays unbound. For a
    /// generic-instance declaring type, Vars resolve against the *element*
    /// type's parameters (`GenericInstanceType.IGenericContext.Type` returns
    /// `ElementType`).
    pub(crate) fn generic_context(&self) -> Result<GenericContext> {
        self.sig_context(GenericContext::default())
    }

    /// The signature context given the reading (outer) context. Array-typed
    /// parents leave Cecil's reading context untouched
    /// (`ReadTypeMemberReference`: `if (!declaring_type.IsArray) context =
    /// declaring_type`), so their member signatures resolve against the outer
    /// context.
    pub(crate) fn sig_context(&self, outer: GenericContext) -> Result<GenericContext> {
        let class = self.raw()?.class;
        Ok(match class.token_type() {
            Some(TokenType::TypeDef) => GenericContext::for_type(class),
            Some(TokenType::TypeSpec) => {
                let row = self.m.md.row::<TypeSpecRaw>(class.rid())?;
                let blob = self.m.md.blobs().view(row.signature)?;
                match SigReader::new(blob).type_sig() {
                    Ok(TypeSig::GenericInst { ty, .. }) => GenericContext::for_type(ty),
                    Ok(TypeSig::Array { .. }) | Ok(TypeSig::SzArray(_)) => outer,
                    _ => GenericContext::default(),
                }
            }
            // A vararg call to a MethodDef: Cecil reads the signature against
            // a new MethodReference (`ReadMethodMemberReference`), so Vars
            // resolve against the method's declaring type and MVars stay
            // unbound.
            Some(TokenType::Method) => match (MethodDef {
                m: self.m,
                rid: class.rid(),
            })
            .declaring_type()?
            {
                Some(t) => GenericContext::for_type(t.token()),
                None => GenericContext::default(),
            },
            _ => GenericContext::default(),
        })
    }

    pub fn full_name(&self) -> Result<String> {
        let ctx = self.generic_context()?;
        let mut out = String::new();
        match self.signature()? {
            MemberRefSig::Field(ty) => {
                sig_full_name(self.m, &ty, ctx, &mut out)?;
                out.push(' ');
                self.declaring_full_name(ctx, &mut out)?;
                out.push_str("::");
                out.push_str(&self.name()?);
            }
            MemberRefSig::Method(sig) => {
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                self.declaring_full_name(ctx, &mut out)?;
                out.push_str("::");
                out.push_str(&self.name()?);
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
        }
        Ok(out)
    }
}

impl<'a> MethodSpec<'a> {
    /// The instantiated method (MethodDef or MemberRef token).
    pub fn method_token(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.method)
    }

    pub fn generic_args(&self) -> Result<Vec<TypeSig>> {
        let blob = self.m.md.blobs().view(self.raw()?.instantiation)?;
        SigReader::new(blob).method_spec_sig()
    }

    /// `GenericInstanceMethod.FullName`:
    /// "<ret> <declaring>::<name><args>(<element params>)", everything
    /// rendered against the *element* method's signature.
    pub fn full_name(&self) -> Result<String> {
        let method = self.raw()?.method;
        let args = self.generic_args()?;
        let mut out = String::new();
        match method.token_type() {
            Some(TokenType::Method) => {
                let m = MethodDef {
                    m: self.m,
                    rid: method.rid(),
                };
                let ctx = m.generic_context()?;
                let sig = m.signature()?;
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                if let Some(decl) = m.declaring_type()? {
                    typedef_full_name(self.m, decl.rid, &mut out)?;
                    out.push_str("::");
                }
                out.push_str(&m.name()?);
                generic_args_full_name(self.m, &args, ctx, &mut out)?;
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
            Some(TokenType::MemberRef) => {
                let mr = MemberRef {
                    m: self.m,
                    rid: method.rid(),
                };
                let ctx = mr.generic_context()?;
                let MemberRefSig::Method(sig) = mr.signature()? else {
                    return Err(Error::Unsupported("MethodSpec over a field MemberRef"));
                };
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                mr.declaring_full_name(ctx, &mut out)?;
                out.push_str("::");
                out.push_str(&mr.name()?);
                generic_args_full_name(self.m, &args, ctx, &mut out)?;
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
            _ => return Err(Error::Unsupported("bad MethodSpec method token")),
        }
        Ok(out)
    }
}

fn generic_args_full_name(
    m: &ModuleData<'_>,
    args: &[TypeSig],
    ctx: GenericContext,
    out: &mut String,
) -> Result<()> {
    out.push('<');
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        sig_full_name(m, a, ctx, out)?;
    }
    out.push('>');
    Ok(())
}

impl<'a> StandAloneSig<'a> {
    pub fn blob(&self) -> Result<&'a [u8]> {
        self.m.md.blobs().view(self.raw()?.signature)
    }

    /// The signature decoded as a call site (for `calli`).
    pub fn as_method_sig(&self) -> Result<MethodSig> {
        SigReader::new(self.blob()?).method_sig()
    }

    /// The signature decoded as local variables.
    pub fn as_local_vars(&self) -> Result<Vec<TypeSig>> {
        SigReader::new(self.blob()?).local_var_sig()
    }
}

pub mod exported_type_attributes {
    pub const FORWARDER: u32 = 0x0020_0000;
    pub const PUBLIC: u32 = 0x1;
}

impl<'a> ExportedType<'a> {
    pub fn name(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.name)
    }

    pub fn namespace(&self) -> Result<Cow<'a, str>> {
        self.m.md.strings().get(self.raw()?.namespace)
    }

    pub fn attributes(&self) -> Result<u32> {
        Ok(self.raw()?.flags)
    }

    pub fn is_forwarder(&self) -> Result<bool> {
        Ok(self.attributes()? & exported_type_attributes::FORWARDER != 0)
    }

    pub fn implementation(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.implementation)
    }

    /// The exported type this one is nested in. Cecil reads the table in
    /// order and takes the declaring type from the rows already read, so it
    /// throws on one nested in itself or in a later row; so does this.
    pub fn declaring_type(&self) -> Result<Option<ExportedType<'a>>> {
        let imp = self.implementation()?;
        if imp.token_type() == Some(TokenType::ExportedType) && imp.rid() != 0 {
            if imp.rid() >= self.rid {
                return Err(Error::Unsupported(
                    "exported type nested in itself or a later row",
                ));
            }
            Ok(Some(ExportedType {
                m: self.m,
                rid: imp.rid(),
            }))
        } else {
            Ok(None)
        }
    }

    /// This type and the types it is nested in, innermost first.
    pub(crate) fn nesting(&self) -> Result<Vec<ExportedType<'a>>> {
        let mut chain = vec![*self];
        while let Some(declaring) = chain[chain.len() - 1].declaring_type()? {
            chain.push(declaring);
        }
        Ok(chain)
    }

    pub fn full_name(&self) -> Result<String> {
        // ExportedType.FullName: nested exported types render Declaring/Nested.
        let mut out = String::new();
        for (i, et) in self.nesting()?.iter().rev().enumerate() {
            if i > 0 {
                out.push('/');
            }
            let ns = et.namespace()?;
            if !ns.is_empty() {
                out.push_str(&ns);
                out.push('.');
            }
            out.push_str(&et.name()?);
        }
        Ok(out)
    }

    /// The scope's display name (file name or assembly-ref name), walking up
    /// through nested exported types.
    pub fn scope_name(&self) -> Result<Cow<'a, str>> {
        let outermost = self.nesting()?.pop().expect("nesting includes self");
        let imp = outermost.implementation()?;
        match imp.token_type() {
            Some(TokenType::File) => {
                let row = self.m.md.row::<FileRaw>(imp.rid())?;
                self.m.md.strings().get(row.name)
            }
            Some(TokenType::AssemblyRef) => {
                let row = self.m.md.row::<AssemblyRefRaw>(imp.rid())?;
                self.m.md.strings().get(row.name)
            }
            _ => Err(Error::Unsupported("bad ExportedType implementation")),
        }
    }
}
