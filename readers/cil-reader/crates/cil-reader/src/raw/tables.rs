//! Metadata tables: the `#~` stream, row counts, per-image row sizes, and
//! typed raw-row accessors.
//!
//! The per-table column specifications below reproduce Cecil's row-size switch
//! (`ImageReader.ComputeTableInformations`, `Mono.Cecil.PE/ImageReader.cs:483-740`)
//! as data: each table's row size is the sum of its column sizes for this
//! image's heap-index sizes and row counts.

use crate::error::{Error, Result};
use crate::raw::coded::{CODED_INDEX_COUNT, CodedIndexKind};
use crate::raw::cursor::Cur;
use crate::raw::heaps::{BlobIndex, GuidIndex, StringIndex};
use crate::token::{MetadataToken, TokenType};

pub const TABLE_COUNT: usize = 58;

/// Metadata table ids (`Mono.Cecil.Metadata/TableHeap.cs` `Table`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Table {
    Module = 0x00,
    TypeRef = 0x01,
    TypeDef = 0x02,
    FieldPtr = 0x03,
    Field = 0x04,
    MethodPtr = 0x05,
    Method = 0x06,
    ParamPtr = 0x07,
    Param = 0x08,
    InterfaceImpl = 0x09,
    MemberRef = 0x0a,
    Constant = 0x0b,
    CustomAttribute = 0x0c,
    FieldMarshal = 0x0d,
    DeclSecurity = 0x0e,
    ClassLayout = 0x0f,
    FieldLayout = 0x10,
    StandAloneSig = 0x11,
    EventMap = 0x12,
    EventPtr = 0x13,
    Event = 0x14,
    PropertyMap = 0x15,
    PropertyPtr = 0x16,
    Property = 0x17,
    MethodSemantics = 0x18,
    MethodImpl = 0x19,
    ModuleRef = 0x1a,
    TypeSpec = 0x1b,
    ImplMap = 0x1c,
    FieldRva = 0x1d,
    EncLog = 0x1e,
    EncMap = 0x1f,
    Assembly = 0x20,
    AssemblyProcessor = 0x21,
    AssemblyOs = 0x22,
    AssemblyRef = 0x23,
    AssemblyRefProcessor = 0x24,
    AssemblyRefOs = 0x25,
    File = 0x26,
    ExportedType = 0x27,
    ManifestResource = 0x28,
    NestedClass = 0x29,
    GenericParam = 0x2a,
    MethodSpec = 0x2b,
    GenericParamConstraint = 0x2c,

    Document = 0x30,
    MethodDebugInformation = 0x31,
    LocalScope = 0x32,
    LocalVariable = 0x33,
    LocalConstant = 0x34,
    ImportScope = 0x35,
    StateMachineMethod = 0x36,
    CustomDebugInformation = 0x37,
}

impl Table {
    pub fn from_index(index: usize) -> Option<Table> {
        use Table::*;
        const ALL: [Option<Table>; TABLE_COUNT] = {
            let mut all: [Option<Table>; TABLE_COUNT] = [None; TABLE_COUNT];
            let list = [
                Module,
                TypeRef,
                TypeDef,
                FieldPtr,
                Field,
                MethodPtr,
                Method,
                ParamPtr,
                Param,
                InterfaceImpl,
                MemberRef,
                Constant,
                CustomAttribute,
                FieldMarshal,
                DeclSecurity,
                ClassLayout,
                FieldLayout,
                StandAloneSig,
                EventMap,
                EventPtr,
                Event,
                PropertyMap,
                PropertyPtr,
                Property,
                MethodSemantics,
                MethodImpl,
                ModuleRef,
                TypeSpec,
                ImplMap,
                FieldRva,
                EncLog,
                EncMap,
                Assembly,
                AssemblyProcessor,
                AssemblyOs,
                AssemblyRef,
                AssemblyRefProcessor,
                AssemblyRefOs,
                File,
                ExportedType,
                ManifestResource,
                NestedClass,
                GenericParam,
                MethodSpec,
                GenericParamConstraint,
                Document,
                MethodDebugInformation,
                LocalScope,
                LocalVariable,
                LocalConstant,
                ImportScope,
                StateMachineMethod,
                CustomDebugInformation,
            ];
            let mut i = 0;
            while i < list.len() {
                all[list[i] as usize] = Some(list[i]);
                i += 1;
            }
            all
        };
        ALL.get(index).copied().flatten()
    }

    /// The token type for rows of this table, if the table has one.
    pub fn token_type(self) -> Option<TokenType> {
        use Table as T;
        Some(match self {
            T::Module => TokenType::Module,
            T::TypeRef => TokenType::TypeRef,
            T::TypeDef => TokenType::TypeDef,
            T::Field => TokenType::Field,
            T::Method => TokenType::Method,
            T::Param => TokenType::Param,
            T::InterfaceImpl => TokenType::InterfaceImpl,
            T::MemberRef => TokenType::MemberRef,
            T::CustomAttribute => TokenType::CustomAttribute,
            T::DeclSecurity => TokenType::Permission,
            T::StandAloneSig => TokenType::Signature,
            T::Event => TokenType::Event,
            T::Property => TokenType::Property,
            T::ModuleRef => TokenType::ModuleRef,
            T::TypeSpec => TokenType::TypeSpec,
            T::Assembly => TokenType::Assembly,
            T::AssemblyRef => TokenType::AssemblyRef,
            T::File => TokenType::File,
            T::ExportedType => TokenType::ExportedType,
            T::ManifestResource => TokenType::ManifestResource,
            T::GenericParam => TokenType::GenericParam,
            T::MethodSpec => TokenType::MethodSpec,
            T::GenericParamConstraint => TokenType::GenericParamConstraint,
            T::Document => TokenType::Document,
            T::MethodDebugInformation => TokenType::MethodDebugInformation,
            T::LocalScope => TokenType::LocalScope,
            T::LocalVariable => TokenType::LocalVariable,
            T::LocalConstant => TokenType::LocalConstant,
            T::ImportScope => TokenType::ImportScope,
            T::StateMachineMethod => TokenType::StateMachineMethod,
            T::CustomDebugInformation => TokenType::CustomDebugInformation,
            _ => return None,
        })
    }

    pub fn from_token_type(tt: TokenType) -> Option<Table> {
        use Table as T;
        Some(match tt {
            TokenType::Module => T::Module,
            TokenType::TypeRef => T::TypeRef,
            TokenType::TypeDef => T::TypeDef,
            TokenType::Field => T::Field,
            TokenType::Method => T::Method,
            TokenType::Param => T::Param,
            TokenType::InterfaceImpl => T::InterfaceImpl,
            TokenType::MemberRef => T::MemberRef,
            TokenType::CustomAttribute => T::CustomAttribute,
            TokenType::Permission => T::DeclSecurity,
            TokenType::Signature => T::StandAloneSig,
            TokenType::Event => T::Event,
            TokenType::Property => T::Property,
            TokenType::ModuleRef => T::ModuleRef,
            TokenType::TypeSpec => T::TypeSpec,
            TokenType::Assembly => T::Assembly,
            TokenType::AssemblyRef => T::AssemblyRef,
            TokenType::File => T::File,
            TokenType::ExportedType => T::ExportedType,
            TokenType::ManifestResource => T::ManifestResource,
            TokenType::GenericParam => T::GenericParam,
            TokenType::MethodSpec => T::MethodSpec,
            TokenType::GenericParamConstraint => T::GenericParamConstraint,
            TokenType::Document => T::Document,
            TokenType::MethodDebugInformation => T::MethodDebugInformation,
            TokenType::LocalScope => T::LocalScope,
            TokenType::LocalVariable => T::LocalVariable,
            TokenType::LocalConstant => T::LocalConstant,
            TokenType::ImportScope => T::ImportScope,
            TokenType::StateMachineMethod => T::StateMachineMethod,
            TokenType::CustomDebugInformation => T::CustomDebugInformation,
            TokenType::String => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TableInfo {
    /// Offset of the first row, relative to the start of the tables stream.
    pub offset: u32,
    pub rows: u32,
    pub row_size: u32,
}

/// Everything needed to size a column for one image.
#[doc(hidden)]
pub struct SizeCtx {
    pub heap_sizes: u8,
    pub row_counts: [u32; TABLE_COUNT],
    pub coded_sizes: [u8; CODED_INDEX_COUNT],
}

impl SizeCtx {
    pub fn str_size(&self) -> u8 {
        if self.heap_sizes & 0x1 != 0 { 4 } else { 2 }
    }
    pub fn guid_size(&self) -> u8 {
        if self.heap_sizes & 0x2 != 0 { 4 } else { 2 }
    }
    pub fn blob_size(&self) -> u8 {
        if self.heap_sizes & 0x4 != 0 { 4 } else { 2 }
    }
    /// `Image.GetTableIndexSize`: 2 bytes iff row count < 65536.
    pub fn table_index_size(&self, table: Table) -> u8 {
        if self.row_counts[table as usize] < 65536 {
            2
        } else {
            4
        }
    }
    pub fn coded_index_size(&self, kind: CodedIndexKind) -> u8 {
        self.coded_sizes[kind as usize]
    }
}

macro_rules! col_size {
    ($ctx:expr, U16) => {
        2u8
    };
    ($ctx:expr, U32) => {
        4u8
    };
    ($ctx:expr, Str) => {
        $ctx.str_size()
    };
    ($ctx:expr, Guid) => {
        $ctx.guid_size()
    };
    ($ctx:expr, Blob) => {
        $ctx.blob_size()
    };
    ($ctx:expr, (Tab $t:ident)) => {
        $ctx.table_index_size(Table::$t)
    };
    ($ctx:expr, (Coded $c:ident)) => {
        $ctx.coded_index_size(CodedIndexKind::$c)
    };
}

macro_rules! col_type {
    (U16) => {
        u16
    };
    (U32) => {
        u32
    };
    (Str) => {
        StringIndex
    };
    (Guid) => {
        GuidIndex
    };
    (Blob) => {
        BlobIndex
    };
    ((Tab $t:ident)) => {
        u32
    };
    ((Coded $c:ident)) => {
        MetadataToken
    };
}

macro_rules! col_conv {
    ($v:expr, U16) => {
        $v as u16
    };
    ($v:expr, U32) => {
        $v
    };
    ($v:expr, Str) => {
        StringIndex($v)
    };
    ($v:expr, Guid) => {
        GuidIndex($v)
    };
    ($v:expr, Blob) => {
        BlobIndex($v)
    };
    ($v:expr, (Tab $t:ident)) => {
        $v
    };
    ($v:expr, (Coded $c:ident)) => {
        CodedIndexKind::$c.decode($v)
    };
}

/// A typed raw metadata row.
pub trait RawRow: Sized {
    const TABLE: Table;
    #[doc(hidden)]
    fn read(ctx: &SizeCtx, cur: &mut Cur<'_>) -> Result<Self>;
    #[doc(hidden)]
    fn row_size(ctx: &SizeCtx) -> u32;
}

macro_rules! tables {
    ($( $(#[$doc:meta])* $raw:ident = $tag:ident { $( $field:ident : $kind:tt ),* $(,)? } )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $raw {
                $( pub $field: col_type!($kind), )*
            }

            impl RawRow for $raw {
                const TABLE: Table = Table::$tag;

                fn read(#[allow(unused)] ctx: &SizeCtx, cur: &mut Cur<'_>) -> Result<Self> {
                    Ok($raw {
                        $( $field: {
                            let v = cur.col(col_size!(ctx, $kind))?;
                            col_conv!(v, $kind)
                        }, )*
                    })
                }

                fn row_size(#[allow(unused)] ctx: &SizeCtx) -> u32 {
                    0u32 $( + col_size!(ctx, $kind) as u32 )*
                }
            }
        )*

        /// Row size for any present table (Cecil `ComputeTableInformations`).
        pub(crate) fn table_row_size(table: Table, ctx: &SizeCtx) -> u32 {
            match table {
                $( Table::$tag => <$raw as RawRow>::row_size(ctx), )*
            }
        }
    };
}

tables! {
    /// II.22.30
    ModuleRaw = Module {
        generation: U16,
        name: Str,
        mvid: Guid,
        enc_id: Guid,
        enc_base_id: Guid,
    }
    /// II.22.38
    TypeRefRaw = TypeRef {
        scope: (Coded ResolutionScope),
        name: Str,
        namespace: Str,
    }
    /// II.22.37
    TypeDefRaw = TypeDef {
        flags: U32,
        name: Str,
        namespace: Str,
        extends: (Coded TypeDefOrRef),
        field_list: (Tab Field),
        method_list: (Tab Method),
    }
    FieldPtrRaw = FieldPtr {
        field: (Tab Field),
    }
    /// II.22.15
    FieldRaw = Field {
        flags: U16,
        name: Str,
        signature: Blob,
    }
    MethodPtrRaw = MethodPtr {
        method: (Tab Method),
    }
    /// II.22.26
    MethodRaw = Method {
        rva: U32,
        impl_flags: U16,
        flags: U16,
        name: Str,
        signature: Blob,
        param_list: (Tab Param),
    }
    ParamPtrRaw = ParamPtr {
        param: (Tab Param),
    }
    /// II.22.33
    ParamRaw = Param {
        flags: U16,
        sequence: U16,
        name: Str,
    }
    /// II.22.23
    InterfaceImplRaw = InterfaceImpl {
        class: (Tab TypeDef),
        interface: (Coded TypeDefOrRef),
    }
    /// II.22.25
    MemberRefRaw = MemberRef {
        class: (Coded MemberRefParent),
        name: Str,
        signature: Blob,
    }
    /// II.22.9 — `ty` is the ElementType in the low byte, padding in the high.
    ConstantRaw = Constant {
        ty: U16,
        parent: (Coded HasConstant),
        value: Blob,
    }
    /// II.22.10
    CustomAttributeRaw = CustomAttribute {
        parent: (Coded HasCustomAttribute),
        constructor: (Coded CustomAttributeType),
        value: Blob,
    }
    /// II.22.17
    FieldMarshalRaw = FieldMarshal {
        parent: (Coded HasFieldMarshal),
        native_type: Blob,
    }
    /// II.22.11
    DeclSecurityRaw = DeclSecurity {
        action: U16,
        parent: (Coded HasDeclSecurity),
        permission_set: Blob,
    }
    /// II.22.8
    ClassLayoutRaw = ClassLayout {
        packing_size: U16,
        class_size: U32,
        parent: (Tab TypeDef),
    }
    /// II.22.16
    FieldLayoutRaw = FieldLayout {
        offset: U32,
        field: (Tab Field),
    }
    /// II.22.36
    StandAloneSigRaw = StandAloneSig {
        signature: Blob,
    }
    /// II.22.12
    EventMapRaw = EventMap {
        parent: (Tab TypeDef),
        event_list: (Tab Event),
    }
    EventPtrRaw = EventPtr {
        event: (Tab Event),
    }
    /// II.22.13
    EventRaw = Event {
        flags: U16,
        name: Str,
        event_type: (Coded TypeDefOrRef),
    }
    /// II.22.35
    PropertyMapRaw = PropertyMap {
        parent: (Tab TypeDef),
        property_list: (Tab Property),
    }
    PropertyPtrRaw = PropertyPtr {
        property: (Tab Property),
    }
    /// II.22.34
    PropertyRaw = Property {
        flags: U16,
        name: Str,
        ty: Blob,
    }
    /// II.22.28
    MethodSemanticsRaw = MethodSemantics {
        semantics: U16,
        method: (Tab Method),
        association: (Coded HasSemantics),
    }
    /// II.22.27
    MethodImplRaw = MethodImpl {
        class: (Tab TypeDef),
        method_body: (Coded MethodDefOrRef),
        method_declaration: (Coded MethodDefOrRef),
    }
    /// II.22.31
    ModuleRefRaw = ModuleRef {
        name: Str,
    }
    /// II.22.39
    TypeSpecRaw = TypeSpec {
        signature: Blob,
    }
    /// II.22.22
    ImplMapRaw = ImplMap {
        mapping_flags: U16,
        member_forwarded: (Coded MemberForwarded),
        import_name: Str,
        import_scope: (Tab ModuleRef),
    }
    /// II.22.18
    FieldRvaRaw = FieldRva {
        rva: U32,
        field: (Tab Field),
    }
    EncLogRaw = EncLog {
        token: U32,
        func_code: U32,
    }
    EncMapRaw = EncMap {
        token: U32,
    }
    /// II.22.2
    AssemblyRaw = Assembly {
        hash_alg_id: U32,
        major_version: U16,
        minor_version: U16,
        build_number: U16,
        revision_number: U16,
        flags: U32,
        public_key: Blob,
        name: Str,
        culture: Str,
    }
    AssemblyProcessorRaw = AssemblyProcessor {
        processor: U32,
    }
    AssemblyOsRaw = AssemblyOs {
        platform: U32,
        major_version: U32,
        minor_version: U32,
    }
    /// II.22.5
    AssemblyRefRaw = AssemblyRef {
        major_version: U16,
        minor_version: U16,
        build_number: U16,
        revision_number: U16,
        flags: U32,
        public_key_or_token: Blob,
        name: Str,
        culture: Str,
        hash_value: Blob,
    }
    AssemblyRefProcessorRaw = AssemblyRefProcessor {
        processor: U32,
        assembly_ref: (Tab AssemblyRef),
    }
    AssemblyRefOsRaw = AssemblyRefOs {
        platform: U32,
        major_version: U32,
        minor_version: U32,
        assembly_ref: (Tab AssemblyRef),
    }
    /// II.22.19
    FileRaw = File {
        flags: U32,
        name: Str,
        hash_value: Blob,
    }
    /// II.22.14
    ExportedTypeRaw = ExportedType {
        flags: U32,
        type_def_id: U32,
        name: Str,
        namespace: Str,
        implementation: (Coded Implementation),
    }
    /// II.22.24
    ManifestResourceRaw = ManifestResource {
        offset: U32,
        flags: U32,
        name: Str,
        implementation: (Coded Implementation),
    }
    /// II.22.32
    NestedClassRaw = NestedClass {
        nested_class: (Tab TypeDef),
        enclosing_class: (Tab TypeDef),
    }
    /// II.22.20
    GenericParamRaw = GenericParam {
        number: U16,
        flags: U16,
        owner: (Coded TypeOrMethodDef),
        name: Str,
    }
    /// II.22.29
    MethodSpecRaw = MethodSpec {
        method: (Coded MethodDefOrRef),
        instantiation: Blob,
    }
    /// II.22.21
    GenericParamConstraintRaw = GenericParamConstraint {
        owner: (Tab GenericParam),
        constraint: (Coded TypeDefOrRef),
    }

    // Portable PDB tables (used by the optional portable-PDB phase, but their
    // sizes must be known so mixed table streams lay out correctly).
    DocumentRaw = Document {
        name: Blob,
        hash_algorithm: Guid,
        hash: Blob,
        language: Guid,
    }
    MethodDebugInformationRaw = MethodDebugInformation {
        document: (Tab Document),
        sequence_points: Blob,
    }
    LocalScopeRaw = LocalScope {
        method: (Tab Method),
        import_scope: (Tab ImportScope),
        variable_list: (Tab LocalVariable),
        constant_list: (Tab LocalConstant),
        start_offset: U32,
        length: U32,
    }
    LocalVariableRaw = LocalVariable {
        attributes: U16,
        index: U16,
        name: Str,
    }
    LocalConstantRaw = LocalConstant {
        name: Str,
        signature: Blob,
    }
    ImportScopeRaw = ImportScope {
        parent: (Tab ImportScope),
        imports: Blob,
    }
    StateMachineMethodRaw = StateMachineMethod {
        move_next_method: (Tab Method),
        kickoff_method: (Tab Method),
    }
    CustomDebugInformationRaw = CustomDebugInformation {
        parent: (Coded HasCustomDebugInformation),
        kind: Guid,
        value: Blob,
    }
}

/// Typed accessor for one metadata table.
#[derive(Clone, Copy)]
pub struct TableRef<'a, R: RawRow> {
    pub(crate) md: &'a super::metadata::Metadata<'a>,
    pub(crate) info: TableInfo,
    _marker: std::marker::PhantomData<R>,
}

impl<'a, R: RawRow> TableRef<'a, R> {
    pub(crate) fn new(md: &'a super::metadata::Metadata<'a>, info: TableInfo) -> Self {
        TableRef {
            md,
            info,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn len(&self) -> u32 {
        self.info.rows
    }

    pub fn is_empty(&self) -> bool {
        self.info.rows == 0
    }

    /// Reads the row with 1-based id `rid`. O(1): `offset + row_size * (rid-1)`.
    pub fn row(&self, rid: u32) -> Result<R> {
        if rid == 0 || rid > self.info.rows {
            return Err(Error::IndexOutOfRange {
                kind: "table rid",
                index: rid,
                max: self.info.rows,
            });
        }
        let pos = self.info.offset as usize + self.info.row_size as usize * (rid as usize - 1);
        let mut cur = Cur::at(self.md.tables_data, pos);
        R::read(self.md.size_ctx(), &mut cur)
    }

    /// Iterates all rows in rid order, paired with their rid.
    pub fn rows(&self) -> RowIter<'a, R> {
        RowIter {
            table: TableRef {
                md: self.md,
                info: self.info,
                _marker: std::marker::PhantomData,
            },
            next: 1,
        }
    }
}

pub struct RowIter<'a, R: RawRow> {
    table: TableRef<'a, R>,
    next: u32,
}

impl<'a, R: RawRow> Iterator for RowIter<'a, R> {
    type Item = (u32, Result<R>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next > self.table.info.rows {
            return None;
        }
        let rid = self.next;
        self.next += 1;
        Some((rid, self.table.row(rid)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = (self.table.info.rows + 1 - self.next) as usize;
        (n, Some(n))
    }
}

impl<'a, R: RawRow> ExactSizeIterator for RowIter<'a, R> {}
