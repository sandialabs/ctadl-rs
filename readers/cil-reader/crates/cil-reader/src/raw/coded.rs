//! Coded indices: tag-compressed cross-table references.
//!
//! Port of `Mono.Cecil.Metadata/CodedIndex.cs` and the decode/size logic in
//! `Mono.Cecil.Metadata/Utilities.cs:42-301,564-648`.

use crate::raw::tables::Table;
use crate::token::{MetadataToken, TokenType};

pub const CODED_INDEX_COUNT: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CodedIndexKind {
    TypeDefOrRef = 0,
    HasConstant,
    HasCustomAttribute,
    HasFieldMarshal,
    HasDeclSecurity,
    MemberRefParent,
    HasSemantics,
    MethodDefOrRef,
    MemberForwarded,
    Implementation,
    CustomAttributeType,
    ResolutionScope,
    TypeOrMethodDef,
    HasCustomDebugInformation,
}

use CodedIndexKind::*;
use Table as T;

const HAS_CUSTOM_ATTRIBUTE_TABLES: &[Table] = &[
    T::Method,
    T::Field,
    T::TypeRef,
    T::TypeDef,
    T::Param,
    T::InterfaceImpl,
    T::MemberRef,
    T::Module,
    T::DeclSecurity,
    T::Property,
    T::Event,
    T::StandAloneSig,
    T::ModuleRef,
    T::TypeSpec,
    T::Assembly,
    T::AssemblyRef,
    T::File,
    T::ExportedType,
    T::ManifestResource,
    T::GenericParam,
    T::GenericParamConstraint,
    T::MethodSpec,
];

const HAS_CUSTOM_DEBUG_INFORMATION_TABLES: &[Table] = &[
    T::Method,
    T::Field,
    T::TypeRef,
    T::TypeDef,
    T::Param,
    T::InterfaceImpl,
    T::MemberRef,
    T::Module,
    T::DeclSecurity,
    T::Property,
    T::Event,
    T::StandAloneSig,
    T::ModuleRef,
    T::TypeSpec,
    T::Assembly,
    T::AssemblyRef,
    T::File,
    T::ExportedType,
    T::ManifestResource,
    T::GenericParam,
    T::GenericParamConstraint,
    T::MethodSpec,
    T::Document,
    T::LocalScope,
    T::LocalVariable,
    T::LocalConstant,
    T::ImportScope,
];

impl CodedIndexKind {
    pub const fn bits(self) -> u32 {
        match self {
            TypeDefOrRef | HasConstant | HasDeclSecurity | Implementation | ResolutionScope => 2,
            HasCustomAttribute | HasCustomDebugInformation => 5,
            HasFieldMarshal | HasSemantics | MethodDefOrRef | MemberForwarded | TypeOrMethodDef => {
                1
            }
            MemberRefParent | CustomAttributeType => 3,
        }
    }

    /// The tables a tag value can point to, in tag order. `CustomAttributeType`
    /// is the one irregular case: only tags 2 and 3 are defined, so its list
    /// here is only used for size computation (matching `Utilities.GetSize`).
    pub const fn tables(self) -> &'static [Table] {
        match self {
            TypeDefOrRef => &[T::TypeDef, T::TypeRef, T::TypeSpec],
            HasConstant => &[T::Field, T::Param, T::Property],
            HasCustomAttribute => HAS_CUSTOM_ATTRIBUTE_TABLES,
            HasFieldMarshal => &[T::Field, T::Param],
            HasDeclSecurity => &[T::TypeDef, T::Method, T::Assembly],
            MemberRefParent => &[T::TypeDef, T::TypeRef, T::ModuleRef, T::Method, T::TypeSpec],
            HasSemantics => &[T::Event, T::Property],
            MethodDefOrRef => &[T::Method, T::MemberRef],
            MemberForwarded => &[T::Field, T::Method],
            Implementation => &[T::File, T::AssemblyRef, T::ExportedType],
            CustomAttributeType => &[T::Method, T::MemberRef],
            ResolutionScope => &[T::Module, T::ModuleRef, T::AssemblyRef, T::TypeRef],
            TypeOrMethodDef => &[T::TypeDef, T::Method],
            HasCustomDebugInformation => HAS_CUSTOM_DEBUG_INFORMATION_TABLES,
        }
    }

    fn tag_token_type(self, tag: u32) -> Option<TokenType> {
        if self == CustomAttributeType {
            // Utilities.cs:201-210 — only tags 2 and 3 are defined.
            return match tag {
                2 => Some(TokenType::Method),
                3 => Some(TokenType::MemberRef),
                _ => None,
            };
        }
        self.tables()
            .get(tag as usize)
            .and_then(|t| t.token_type())
    }

    /// Decodes a raw coded-index column value into a token.
    ///
    /// Mirrors `Utilities.GetMetadataToken`: an undefined tag yields
    /// `MetadataToken::ZERO` rather than an error.
    pub fn decode(self, data: u32) -> MetadataToken {
        let rid = data >> self.bits();
        let tag = data & ((1 << self.bits()) - 1);
        match self.tag_token_type(tag) {
            Some(tt) => MetadataToken::new(tt, rid),
            None => MetadataToken::ZERO,
        }
    }

    /// Column width (2 or 4 bytes) given per-table row counts.
    ///
    /// Mirrors `Utilities.GetSize`: 2 bytes iff every referenced table's row
    /// count fits in `16 - bits` bits.
    pub fn size(self, counter: impl Fn(Table) -> u32) -> u8 {
        let max = self.tables().iter().map(|&t| counter(t)).max().unwrap_or(0);
        if max < (1u32 << (16 - self.bits())) { 2 } else { 4 }
    }
}
