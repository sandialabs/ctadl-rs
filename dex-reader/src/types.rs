// Header (header_item)
#[derive(Debug, Clone, Copy, Default)]
pub struct DexHeader {
    pub magic: [u8; 8],
    pub checksum: u32,
    pub signature: [u8; 20],

    pub file_size: u32,
    pub header_size: u32,
    pub endian_tag: u32,

    pub link_size: u32,
    pub link_off: u32,

    pub map_off: u32,

    pub string_ids_size: u32,
    pub string_ids_off: u32,

    pub type_ids_size: u32,
    pub type_ids_off: u32,

    pub proto_ids_size: u32,
    pub proto_ids_off: u32,

    pub field_ids_size: u32,
    pub field_ids_off: u32,

    pub method_ids_size: u32,
    pub method_ids_off: u32,

    pub class_defs_size: u32,
    pub class_defs_off: u32,

    pub data_size: u32,
    pub data_off: u32,
}

#[derive(Debug, Clone, Default)]
pub struct MapList {
    pub items: Vec<MapItem>,
}

#[derive(Debug, Clone, Copy)]
pub struct MapItem {
    pub type_code: u16,
    pub size: u32,
    pub offset: u32,
}

// ID sections
// String IDs (string_id_item)
#[derive(Debug, Clone, Copy)]
pub struct StringId {
    pub string_data_off: u32,
}

#[derive(Debug)]
pub struct StringTable<'a> {
    pub data: &'a [u8],
    pub string_ids: Vec<StringId>,
}

impl<'a> StringTable<'a> {
    pub fn len(&self) -> usize {
        self.string_ids.len()
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct AccessFlag(u32);

impl AccessFlag {
    /// Returns true if this access flag is set in the given parsed set of flags
    #[inline]
    pub fn is_set_in(&self, parsed: u32) -> bool {
        let AccessFlag(mask) = self;
        (mask & parsed) != 0
    }
}

pub const ACC_PUBLIC: AccessFlag = AccessFlag(0x1);
pub const ACC_PRIVATE: AccessFlag = AccessFlag(0x2);
pub const ACC_PROTECTED: AccessFlag = AccessFlag(0x4);
pub const ACC_STATIC: AccessFlag = AccessFlag(0x8);
pub const ACC_FINAL: AccessFlag = AccessFlag(0x10);
pub const ACC_SYNCHRONIZED: AccessFlag = AccessFlag(0x20);
pub const ACC_VOLATILE: AccessFlag = AccessFlag(0x40);
pub const ACC_BRIDGE: AccessFlag = AccessFlag(0x40);
pub const ACC_TRANSIENT: AccessFlag = AccessFlag(0x80);
pub const ACC_VARARGS: AccessFlag = AccessFlag(0x80);
pub const ACC_NATIVE: AccessFlag = AccessFlag(0x100);
pub const ACC_INTERFACE: AccessFlag = AccessFlag(0x200);
pub const ACC_ABSTRACT: AccessFlag = AccessFlag(0x400);
pub const ACC_STRICT: AccessFlag = AccessFlag(0x800);
pub const ACC_SYNTHETIC: AccessFlag = AccessFlag(0x1000);
pub const ACC_ANNOTATION: AccessFlag = AccessFlag(0x2000);
pub const ACC_ENUM: AccessFlag = AccessFlag(0x4000);
pub const ACC_CONSTRUCTOR: AccessFlag = AccessFlag(0x10000);
pub const ACC_DECLARED_SYNCHRONIZED: AccessFlag = AccessFlag(0x20000);

// Type IDs (type_id_item)
#[derive(Debug, Clone, Copy)]
pub struct TypeId {
    pub descriptor_idx: u32,
}

// Proto IDs (proto_id_item)
#[derive(Debug, Clone, Copy)]
pub struct ProtoId {
    /// index into the string table
    pub shorty_idx: u32,
    /// type table index
    pub return_type_idx: u32,
    /// 0 or absolute offset from the start of the file that contains the data that is a size
    /// followed by array of type indices
    pub parameters_off: u32,
}

// Field IDs (field_id_item)
#[derive(Debug, Clone, Copy)]
pub struct FieldId {
    pub class_idx: u16,
    pub type_idx: u16,
    pub name_idx: u32,
}

// Method IDs (method_id_item)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MethodId {
    pub class_idx: u16,
    pub proto_idx: u16,
    pub name_idx: u32,
}

// Call site IDs (call_site_id_item)
#[derive(Debug, Clone, Copy)]
pub struct CallSiteId {
    /// Offset from start of file to call site definition (call_site_item)
    pub call_site_off: u32,
}

// Method handles (method_handle_item)
#[derive(Debug, Clone, Copy)]
pub struct MethodHandle {
    pub method_handle_type: u16,
    /// Field or method id, depending on method_handle_type.
    pub field_or_method_id: u16,
}

#[derive(Debug, Clone)]
pub struct TypeList {
    pub types: Vec<u16>, // indices into type_ids
}

// Class definitions (class_def_item)
#[derive(Debug, Clone, Copy)]
pub struct ClassDef {
    pub class_idx: u32,
    pub access_flags: u32,
    pub superclass_idx: u32,

    pub interfaces_off: u32,
    pub source_file_idx: u32,

    pub annotations_off: u32,
    pub class_data_off: u32,
    pub static_values_off: u32,
}

#[derive(Debug, Clone)]
pub struct ClassData {
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
}

// Encoded fields & methods
#[derive(Debug, Clone)]
pub struct EncodedField {
    pub field_idx: u32,
    pub access_flags: u32,
}

#[derive(Debug, Clone)]
pub struct EncodedMethod {
    pub method_idx: u32,
    pub access_flags: u32,
    pub code_off: u32,
}

// (All fields are ULEB128-encoded when stored.)

#[derive(Debug, Clone)]
pub struct CodeItem {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub tries_size: u16,
    pub debug_info_off: u32,
    pub insns: Vec<u16>, // 16-bit code units
    pub tries: Vec<TryItem>,
    pub handlers: Option<CatchHandlerList>,
    pub code_off: u32, // absolute byte offset of this code_item in the DEX file
}

// Try / catch (try_item)
#[derive(Debug, Clone, Copy)]
pub struct TryItem {
    pub start_addr: u32,
    pub insn_count: u16,
    /// Byte offset (ULEB128) from the start of the encoded_catch_handler_list
    /// to the corresponding encoded_catch_handler.
    pub handler_off: u16,
}

/// Single (type_idx, addr) pair inside an encoded_catch_handler.
#[derive(Debug, Clone)]
pub struct TypeAddrPair {
    /// Index into the type_ids list for the exception type.
    pub type_idx: u32,
    /// Bytecode address of the associated exception handler.
    pub addr: u32,
}

/// One encoded_catch_handler entry inside the encoded_catch_handler_list.
///
/// See: encoded_catch_handler format in the DEX spec.
#[derive(Debug, Clone)]
pub struct EncodedCatchHandler {
    /// Signed size from sleb128. Negative values indicate the
    /// presence of a catch-all handler.
    pub raw_size: i32,
    /// Pairs of (type_idx, addr) encoded as uleb128.
    pub pairs: Vec<TypeAddrPair>,
    /// Optional catch-all handler address (uleb128) when raw_size <= 0.
    pub catch_all_addr: Option<u32>,
    /// Relative byte offset from the start of the encoded_catch_handler_list
    /// to this handler entry. This is what TryItem::handler_off refers to.
    pub start_off: u32,
}

/// Top-level container for the encoded_catch_handler_list that follows
/// the try_item array inside a code_item.
///
/// See: encoded_catch_handler_list format in the DEX spec.
#[derive(Debug, Clone)]
pub struct CatchHandlerList {
    /// Number of handler lists as read from the uleb128 size.
    pub size: u32,
    /// All encoded_catch_handler entries, in order of appearance.
    pub handlers: Vec<EncodedCatchHandler>,
}

impl CatchHandlerList {
    /// Look up a handler by its relative offset from the start of the
    /// encoded_catch_handler_list, as stored in TryItem::handler_off.
    pub fn get_by_off(&self, handler_off: u16) -> Option<&EncodedCatchHandler> {
        let off = handler_off as u32;
        self.handlers.iter().find(|h| h.start_off == off)
    }
}

// Debug info (debug_info_item)

/// Sentinel value meaning "no string index" in debug_info_item fields.
pub const NO_INDEX: u32 = 0xFFFFFFFF;
/// Line number delta base for special opcodes.
pub const DBG_LINE_BASE: i32 = -4;
/// Line number delta range for special opcodes.
pub const DBG_LINE_RANGE: u32 = 15;
/// First special opcode value (0x0a).
pub const DBG_FIRST_SPECIAL: u8 = 0x0a;

/// One decoded opcode from the `debug_info_item` state machine.
#[derive(Debug, Clone)]
pub enum DebugInfoOpcode {
    EndSequence,
    AdvancePc {
        addr_diff: u32,
    },
    AdvanceLine {
        line_diff: i32,
    },
    StartLocal {
        register_num: u32,
        name_idx: u32,
        type_idx: u32,
    },
    StartLocalExtended {
        register_num: u32,
        name_idx: u32,
        type_idx: u32,
        sig_idx: u32,
    },
    EndLocal {
        register_num: u32,
    },
    RestartLocal {
        register_num: u32,
    },
    SetPrologueEnd,
    SetEpilogueBegin,
    /// `name_idx` is `NO_INDEX` when the raw encoded value was 0.
    SetFile {
        name_idx: u32,
    },
    /// A special opcode (0x0a–0xff); address and line deltas are pre-computed.
    Special {
        addr_delta: u32,
        line_delta: i32,
    },
}

/// Fully parsed `debug_info_item`.
#[derive(Debug, Clone)]
pub struct DebugInfoItem {
    /// Absolute byte offset of this item in the DEX file.
    pub offset: u32,
    /// Initial source line number for the state machine.
    pub line_start: u32,
    /// Number of parameter name entries.
    pub parameters_size: u32,
    /// Parameter name string indices. `NO_INDEX` means the parameter is unnamed.
    pub parameter_names: Vec<u32>,
    /// Decoded opcodes in order, ending with `EndSequence`.
    pub opcodes: Vec<DebugInfoOpcode>,
}

/// One entry produced by interpreting a `debug_info_item` state machine:
/// maps a bytecode address to a source location.
#[derive(Debug, Clone)]
pub struct PositionEntry {
    /// Bytecode address in 16-bit code units (same space as `TryItem::start_addr`).
    pub address: u32,
    /// Absolute byte offset of the instruction from the start of the DEX file.
    pub absolute_offset: u64,
    /// Source file name at this point (may change within a method via `SetFile`).
    pub source_file: String,
    /// Source line number.
    pub line: u32,
}

/// Flat record used for JSON line-map serialization.
#[derive(Debug, Clone)]
pub struct LineMapEntry {
    /// Fully-qualified method reference, e.g. `"Lcom/example/Foo;->bar(I)V"`.
    pub method: String,
    /// Absolute byte offset of the instruction from the start of the DEX file.
    pub dex_offset: u64,
    /// Source file name (empty string if not available).
    pub source_file: String,
    /// Source line number.
    pub line: u32,
}

// Annotations (simplified)
#[derive(Debug)]
pub struct AnnotationSet {
    pub size: u32,
    // u32 offsets
}

#[derive(Debug)]
pub struct AnnotationItem {
    pub visibility: u8,
    // encoded_annotation follows
}

#[derive(Debug, Clone, Copy)]
pub enum Operand {
    /// string constant pool index
    CS(u32),
    /// type constant pool index
    CT(u32),
    /// proto constant pool index
    CP(u32),
    /// field constant pool index
    CF(u32),
    /// method constant pool index
    CM(u32),
    /// call site table index
    CC(u32),
    /// immediate signed hat
    H(i16),
    /// immediate signed long
    L(i64),
    /// branch target
    T(i32),
    /// immediate unsigned int
    U(u32),
    /// register
    V(u32),
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub opcode: u8,
    pub operands: Vec<Operand>, // decoded operands
    pub offset: usize,          // offset in code_item
}

/// Packed switch payload (dense jump table)
/// Format: size (ushort), first_key (sint), targets[] (sint array)
#[derive(Debug, Clone)]
pub struct PackedSwitchPayload {
    pub size: u16,
    pub first_key: i32,
    pub targets: Vec<i32>, // relative branch targets (signed offsets)
}

/// Sparse switch payload (sparse jump table)
/// Format: size (ushort), keys[] (sint array), targets[] (sint array)
#[derive(Debug, Clone)]
pub struct SparseSwitchPayload {
    pub size: u16,
    pub keys: Vec<i32>,    // case values
    pub targets: Vec<i32>, // relative branch targets (signed offsets)
}

/// Fill array data payload
/// Format: element_width (ushort), size (uint), data[] (ubyte array)
#[derive(Debug, Clone)]
pub struct FillArrayDataPayload {
    pub element_width: u16, // width of each element (1, 2, 4, or 8 bytes)
    pub size: u32,          // number of elements
    pub data: Vec<u8>,      // raw data bytes
}

#[derive(Debug)]
pub struct DexConstantPool<'a> {
    /// Reference to the entire Dex file buffer
    pub data: &'a [u8],
    pub strings: StringTable<'a>,
    pub type_ids: Vec<TypeId>,
    pub proto_ids: Vec<ProtoId>,
    pub field_ids: Vec<FieldId>,
    pub method_ids: Vec<MethodId>,
}

impl<'a> DexConstantPool<'a> {
    /// `file_buf` is the entire dex file buffer
    pub fn new(
        file_buf: &'a [u8],
        strings: StringTable<'a>,
        type_ids: Vec<TypeId>,
        proto_ids: Vec<ProtoId>,
        field_ids: Vec<FieldId>,
        method_ids: Vec<MethodId>,
    ) -> Self {
        Self {
            data: file_buf,
            strings,
            type_ids,
            proto_ids,
            field_ids,
            method_ids,
        }
    }
}

// Decode operands lazily by opcode.

// Top-level container
pub struct Dex<'a> {
    pub data: &'a [u8],
    pub header: DexHeader,
}
