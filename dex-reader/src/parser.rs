use std::collections::HashMap;
use std::sync::OnceLock;

use crate::{error::*, instructions::DispArg, parse_utils::*, types::*};

/// Cache for lazily computed parser-derived data.
pub struct Cache<'a> {
    pub pool: OnceLock<DexConstantPool<'a>>,
}

pub const DEX_MAGIC_035: &[u8; 8] = b"dex\n035\0";
pub const DEX_MAGIC_037: &[u8; 8] = b"dex\n037\0";
pub const DEX_MAGIC_038: &[u8; 8] = b"dex\n038\0";
pub const DEX_MAGIC_039: &[u8; 8] = b"dex\n039\0";

pub const DEX_HEADER_SIZE: u32 = 0x70;
pub const ENDIAN_CONSTANT: u32 = 0x12345678;
pub const REVERSE_ENDIAN_CONSTANT: u32 = 0x78563412;

/// High-level parsed view of a DEX file, suitable for library use.
///
/// This holds the primary sections (header, id tables, class defs), the raw map list,
/// and an index of map items by type code for direct access.
pub struct DexParser<'a> {
    pub data: &'a [u8],
    pub header: DexHeader,
    pub map_list: MapList,
    pub map_items_by_type: HashMap<u16, Vec<MapItem>>,

    pub strings: StringTable<'a>,
    pub type_ids: Vec<TypeId>,
    pub proto_ids: Vec<ProtoId>,
    pub field_ids: Vec<FieldId>,
    pub method_ids: Vec<MethodId>,
    pub class_defs: Vec<ClassDef>,

    pub call_site_ids: Option<Vec<CallSiteId>>,
    pub method_handles: Option<Vec<MethodHandle>>,

    pub cache: Cache<'a>,
}

impl<'a> DexParser<'a> {
    /// Parse a DEX file buffer into a high-level `DexParser` view.
    pub fn new(data: &'a [u8]) -> DexResult<Self> {
        let header = parse_dex_header(data)?;
        let map_list = parse_map_list(data, &header)?;
        validate_map_against_header(&map_list, &header)?;

        let strings = parse_string_ids(data, &header)?;
        let type_ids = parse_type_ids(data, &header)?;
        let proto_ids = parse_proto_ids(data, &header)?;
        let field_ids = parse_field_ids(data, header.field_ids_off, header.field_ids_size)?;
        let method_ids = parse_method_ids(data, &header)?;
        let class_defs = parse_class_defs(data, &header)?;

        // Index map items by type code for direct access.
        let mut map_items_by_type: HashMap<u16, Vec<MapItem>> = HashMap::new();
        for item in &map_list.items {
            map_items_by_type
                .entry(item.type_code)
                .or_default()
                .push(*item);
        }

        // Optional sections (present in newer dex versions).
        let call_site_ids = match map_items_by_type.get(&map_item_type::CALL_SITE_ID_ITEM) {
            Some(items) => {
                if items.len() != 1 {
                    return Err(DexError::InvalidDex(
                        "duplicate call_site_id_item map entries",
                    ));
                }
                let m = items[0];
                Some(parse_call_site_ids(data, m.offset, m.size)?)
            }
            None => None,
        };

        let method_handles = match map_items_by_type.get(&map_item_type::METHOD_HANDLE_ITEM) {
            Some(items) => {
                if items.len() != 1 {
                    return Err(DexError::InvalidDex(
                        "duplicate method_handle_item map entries",
                    ));
                }
                let m = items[0];
                Some(parse_method_handles(data, m.offset, m.size)?)
            }
            None => None,
        };

        Ok(Self {
            data,
            header,
            map_list,
            map_items_by_type,
            strings,
            type_ids,
            proto_ids,
            field_ids,
            method_ids,
            class_defs,
            call_site_ids,
            method_handles,
            cache: Cache {
                pool: OnceLock::new(),
            },
        })
    }

    /// Returns the constant pool, building and caching it on first access.
    pub fn constant_pool(&self) -> &DexConstantPool<'a> {
        self.cache.pool.get_or_init(|| self.build_constant_pool())
    }

    fn build_constant_pool(&self) -> DexConstantPool<'a> {
        DexConstantPool::new(
            self.data,
            StringTable {
                data: self.data,
                string_ids: self.strings.string_ids.clone(),
            },
            self.type_ids.clone(),
            self.proto_ids.clone(),
            self.field_ids.clone(),
            self.method_ids.clone(),
        )
    }

    pub fn classes(&self) -> impl Iterator<Item = &ClassDef> {
        self.class_defs.iter()
    }

    pub fn methods(&self) -> impl Iterator<Item = &MethodId> {
        self.method_ids.iter()
    }

    pub fn types(&self) -> impl Iterator<Item = &TypeId> {
        self.type_ids.iter()
    }

    pub fn fields(&self) -> impl Iterator<Item = &FieldId> {
        self.field_ids.iter()
    }

    pub fn protos(&self) -> impl Iterator<Item = &ProtoId> {
        self.proto_ids.iter()
    }

    pub fn get_class(&self, idx: u32) -> Option<&ClassDef> {
        self.class_defs.iter().find(|c| c.class_idx == idx)
    }

    pub fn get_method(&self, idx: usize) -> Option<&MethodId> {
        self.method_ids.get(idx)
    }

    pub fn get_type(&self, idx: usize) -> Option<&TypeId> {
        self.type_ids.get(idx)
    }

    pub fn get_field(&self, idx: usize) -> Option<&FieldId> {
        self.field_ids.get(idx)
    }

    pub fn get_proto(&self, idx: usize) -> Option<&ProtoId> {
        self.proto_ids.get(idx)
    }

    pub fn method_name(&self, method: &MethodId) -> DexResult<String> {
        self.strings.get(method.name_idx as usize)
    }

    pub fn proto_signature(&self, proto: &ProtoId) -> DexResult<String> {
        proto.pretty_signature(self.data, &self.strings, &self.type_ids)
    }

    pub fn method_parameters(&self, method: &MethodId) -> DexResult<TypeList> {
        let proto = self
            .proto_ids
            .get(method.proto_idx as usize)
            .ok_or(DexError::InvalidDex("proto_idx out of bounds"))?;

        parse_type_list(self.data, proto.parameters_off)
    }

    pub fn get_string(&self, idx: usize) -> DexResult<String> {
        self.strings.get(idx)
    }

    pub fn get_map_items_by_type(&self, type_code: u16) -> Option<&[MapItem]> {
        self.map_items_by_type.get(&type_code).map(|v| v.as_slice())
    }

    pub fn class_methods(&self, class_def: &ClassDef) -> DexResult<Vec<&MethodId>> {
        let class_idx = class_def.class_idx as u16;
        Ok(self
            .method_ids
            .iter()
            .filter(|m| m.class_idx == class_idx)
            .collect())
    }

    pub fn class_data(&self, class_def: &ClassDef) -> DexResult<ClassData> {
        class_def.parse_class_data(self.data)
    }

    pub fn method_code(&self, method: &EncodedMethod) -> DexResult<Option<CodeItem>> {
        if method.code_off == 0 {
            return Ok(None);
        }
        Ok(Some(parse_code_item(self.data, method.code_off)?))
    }

    pub fn method_instructions(
        &self,
        method: &EncodedMethod,
    ) -> DexResult<Option<Vec<DecodedCodeItem>>> {
        let code = self.method_code(method)?;
        Ok(code.map(|c| decode_code_item(&c)))
    }

    pub fn class_interfaces(&self, class_def: &ClassDef) -> DexResult<TypeList> {
        parse_type_list(self.data, class_def.interfaces_off)
    }

    pub fn class_name(&self, class_def: &ClassDef) -> DexResult<String> {
        class_def.class_name(&self.type_ids, &self.strings)
    }

    pub fn method_signature(&self, method: &MethodId) -> DexResult<String> {
        method.signature(&self.strings, &self.type_ids, &self.proto_ids, self.data)
    }

    pub fn method_triple(&self, method: &MethodId) -> DexResult<(String, String, String)> {
        method.vmt_triple(&self.strings, &self.type_ids, &self.proto_ids, self.data)
    }

    pub fn field_name(&self, field: &FieldId) -> DexResult<String> {
        let class_name = self
            .type_ids
            .get(field.class_idx as usize)
            .ok_or(DexError::InvalidDex("class_idx out of bounds"))?
            .descriptor(&self.strings)?;

        let field_type = self
            .type_ids
            .get(field.type_idx as usize)
            .ok_or(DexError::InvalidDex("type_idx out of bounds"))?
            .descriptor(&self.strings)?;

        let field_name = self.strings.get(field.name_idx as usize)?;
        Ok(format!("{}->{}:{}", class_name, field_name, field_type))
    }

    pub fn type_descriptor(&self, type_id: &TypeId) -> DexResult<String> {
        type_id.descriptor(&self.strings)
    }

    /// Parse the `debug_info_item` for a code item, if present.
    ///
    /// Returns `None` when `code_item.debug_info_off == 0`.
    pub fn debug_info(&self, code_item: &CodeItem) -> DexResult<Option<DebugInfoItem>> {
        if code_item.debug_info_off == 0 {
            return Ok(None);
        }
        Ok(Some(crate::debug_info::parse_debug_info(
            self.data,
            code_item.debug_info_off,
        )?))
    }

    /// Interpret the debug state machine for a code item and return a line-number map.
    ///
    /// `source_file` is the initial source file name (typically from the enclosing
    /// `ClassDef`; pass an empty string when absent).
    ///
    /// Returns an empty `Vec` when there is no debug info.
    pub fn line_map(
        &self,
        code_item: &CodeItem,
        source_file: String,
    ) -> DexResult<Vec<PositionEntry>> {
        match self.debug_info(code_item)? {
            None => Ok(vec![]),
            Some(di) => Ok(crate::debug_info::compute_line_map(
                &di,
                code_item,
                source_file,
                &self.strings,
            )),
        }
    }
}

pub fn parse_dex_header(data: &[u8]) -> DexResult<DexHeader> {
    // Header must fit
    check_range(data, 0, DEX_HEADER_SIZE as usize)?;

    let magic = {
        let bytes = read_slice(data, 0x00, 8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        arr
    };

    match &magic {
        DEX_MAGIC_035 | DEX_MAGIC_037 | DEX_MAGIC_038 | DEX_MAGIC_039 => {}
        _ => return Err(DexError::InvalidDex("invalid dex magic")),
    }

    let checksum = read_u32_le(data, 0x08)?;

    let signature = {
        let bytes = read_slice(data, 0x0c, 20)?;
        let mut arr = [0u8; 20];
        arr.copy_from_slice(bytes);
        arr
    };

    let file_size = read_u32_le(data, 0x20)?;
    let header_size = read_u32_le(data, 0x24)?;
    let endian_tag = read_u32_le(data, 0x28)?;

    if header_size != DEX_HEADER_SIZE {
        return Err(DexError::InvalidDex("unexpected header size"));
    }

    match endian_tag {
        ENDIAN_CONSTANT => {}
        REVERSE_ENDIAN_CONSTANT => {
            return Err(DexError::InvalidDex("reverse-endian dex not supported"));
        }
        _ => return Err(DexError::InvalidDex("invalid endian tag")),
    }

    if file_size as usize != data.len() {
        // For analysis, you may want this to be a warning instead
        return Err(DexError::InvalidDex("file size mismatch"));
    }

    let link_size = read_u32_le(data, 0x2c)?;
    let link_off = read_u32_le(data, 0x30)?;

    let map_off = read_u32_le(data, 0x34)?;
    validate_offset(map_off, data.len())?;

    let string_ids_size = read_u32_le(data, 0x38)?;
    let string_ids_off = read_u32_le(data, 0x3c)?;
    validate_offset(string_ids_off, data.len())?;

    let type_ids_size = read_u32_le(data, 0x40)?;
    let type_ids_off = read_u32_le(data, 0x44)?;
    validate_offset(type_ids_off, data.len())?;

    let proto_ids_size = read_u32_le(data, 0x48)?;
    let proto_ids_off = read_u32_le(data, 0x4c)?;
    validate_offset(proto_ids_off, data.len())?;

    let field_ids_size = read_u32_le(data, 0x50)?;
    let field_ids_off = read_u32_le(data, 0x54)?;
    validate_offset(field_ids_off, data.len())?;

    let method_ids_size = read_u32_le(data, 0x58)?;
    let method_ids_off = read_u32_le(data, 0x5c)?;
    validate_offset(method_ids_off, data.len())?;

    let class_defs_size = read_u32_le(data, 0x60)?;
    let class_defs_off = read_u32_le(data, 0x64)?;
    validate_offset(class_defs_off, data.len())?;

    let data_size = read_u32_le(data, 0x68)?;
    let data_off = read_u32_le(data, 0x6c)?;
    validate_offset(data_off, data.len())?;

    Ok(DexHeader {
        magic,
        checksum,
        signature,

        file_size,
        header_size,
        endian_tag,

        link_size,
        link_off,

        map_off,

        string_ids_size,
        string_ids_off,

        type_ids_size,
        type_ids_off,

        proto_ids_size,
        proto_ids_off,

        field_ids_size,
        field_ids_off,

        method_ids_size,
        method_ids_off,

        class_defs_size,
        class_defs_off,

        data_size,
        data_off,
    })
}

pub mod map_item_type {
    pub const HEADER_ITEM: u16 = 0x0000;
    pub const STRING_ID_ITEM: u16 = 0x0001;
    pub const TYPE_ID_ITEM: u16 = 0x0002;
    pub const PROTO_ID_ITEM: u16 = 0x0003;
    pub const FIELD_ID_ITEM: u16 = 0x0004;
    pub const METHOD_ID_ITEM: u16 = 0x0005;
    pub const CLASS_DEF_ITEM: u16 = 0x0006;

    pub const CALL_SITE_ID_ITEM: u16 = 0x0007;
    pub const METHOD_HANDLE_ITEM: u16 = 0x0008;

    pub const MAP_LIST: u16 = 0x1000;
    pub const TYPE_LIST: u16 = 0x1001;
    pub const ANNOTATION_SET_REF_LIST: u16 = 0x1002;
    pub const ANNOTATION_SET_ITEM: u16 = 0x1003;
    pub const CLASS_DATA_ITEM: u16 = 0x2000;
    pub const CODE_ITEM: u16 = 0x2001;
    pub const STRING_DATA_ITEM: u16 = 0x2002;
    pub const DEBUG_INFO_ITEM: u16 = 0x2003;
    pub const ANNOTATION_ITEM: u16 = 0x2004;
    pub const ENCODED_ARRAY_ITEM: u16 = 0x2005;
    pub const ANNOTATIONS_DIRECTORY_ITEM: u16 = 0x2006;
}

pub fn parse_map_list(data: &[u8], header: &DexHeader) -> DexResult<MapList> {
    let map_off = header.map_off as usize;

    // map_off == 0 is invalid in valid dex files
    if map_off == 0 {
        return Err(DexError::InvalidDex("map_off == 0"));
    }

    // Read size
    let size = read_u32_le(data, map_off)? as usize;

    // Each map_item is 12 bytes
    let items_off = map_off + 4;
    let total_size = 4 + size
        .checked_mul(12)
        .ok_or(DexError::InvalidDex("map_list overflow"))?;

    check_range(data, map_off, total_size)?;

    let mut items = Vec::with_capacity(size);

    for i in 0..size {
        let off = items_off + i * 12;

        let type_code = read_u16_le(data, off)?;
        let unused = read_u16_le(data, off + 2)?;
        let item_size = read_u32_le(data, off + 4)?;
        let offset = read_u32_le(data, off + 8)?;

        if unused != 0 {
            return Err(DexError::InvalidDex("map_item.unused != 0"));
        }

        validate_offset(offset, data.len())?;

        items.push(MapItem {
            type_code,
            size: item_size,
            offset,
        });
    }

    Ok(MapList { items })
}

pub fn validate_map_against_header(map: &MapList, header: &DexHeader) -> DexResult<()> {
    use map_item_type::*;

    for item in &map.items {
        match item.type_code {
            HEADER_ITEM => {
                if item.size != 1 || item.offset != 0 {
                    return Err(DexError::InvalidDex("invalid header_item map"));
                }
            }
            STRING_ID_ITEM => {
                if item.size != header.string_ids_size || item.offset != header.string_ids_off {
                    return Err(DexError::InvalidDex("string_ids mismatch"));
                }
            }
            TYPE_ID_ITEM => {
                if item.size != header.type_ids_size || item.offset != header.type_ids_off {
                    return Err(DexError::InvalidDex("type_ids mismatch"));
                }
            }
            PROTO_ID_ITEM => {
                if item.size != header.proto_ids_size || item.offset != header.proto_ids_off {
                    return Err(DexError::InvalidDex("proto_ids mismatch"));
                }
            }
            FIELD_ID_ITEM => {
                if item.size != header.field_ids_size || item.offset != header.field_ids_off {
                    return Err(DexError::InvalidDex("field_ids mismatch"));
                }
            }
            METHOD_ID_ITEM => {
                if item.size != header.method_ids_size || item.offset != header.method_ids_off {
                    return Err(DexError::InvalidDex("method_ids mismatch"));
                }
            }
            CLASS_DEF_ITEM => {
                if item.size != header.class_defs_size || item.offset != header.class_defs_off {
                    return Err(DexError::InvalidDex("class_defs mismatch"));
                }
            }
            // Data section items - these don't have direct header fields to validate against
            MAP_LIST => {
                // Map list is self-referential, basic validation only
                if item.offset == 0 {
                    return Err(DexError::InvalidDex("map_list offset cannot be 0"));
                }
            }
            TYPE_LIST => {
                // Type lists are referenced from proto_ids and class_defs, no header field
            }
            ANNOTATION_SET_REF_LIST => {
                // Referenced from parameter annotations, no header field
            }
            ANNOTATION_SET_ITEM => {
                // Referenced from annotations directory, no header field
            }
            CLASS_DATA_ITEM => {
                // Referenced from class_def_item.class_data_off, no header field
            }
            CODE_ITEM => {
                // Referenced from encoded_method.code_off, no header field
            }
            STRING_DATA_ITEM => {
                // Referenced from string_id_item.string_data_off, no header field
            }
            DEBUG_INFO_ITEM => {
                // Referenced from code_item.debug_info_off, no header field
            }
            ANNOTATION_ITEM => {
                // Referenced from annotation_set_item, no header field
            }
            ENCODED_ARRAY_ITEM => {
                // Referenced from class_def_item.static_values_off, no header field
            }
            ANNOTATIONS_DIRECTORY_ITEM => {
                // Referenced from class_def_item.annotations_off, no header field
            }
            CALL_SITE_ID_ITEM => {
                // Call site IDs (for invoke-custom), no header field
            }
            METHOD_HANDLE_ITEM => {
                // Method handles (for invoke-custom), no header field
            }
            other => {
                println!("Unknown map item {other}");
            }
        }
    }

    Ok(())
}

pub fn parse_string_ids<'a>(data: &'a [u8], header: &DexHeader) -> DexResult<StringTable<'a>> {
    let count = header.string_ids_size as usize;
    let offset = header.string_ids_off as usize;

    check_range(
        data,
        offset,
        count
            .checked_mul(4)
            .ok_or(DexError::InvalidDex("string_ids overflow"))?,
    )?;

    let mut string_ids = Vec::with_capacity(count);

    for i in 0..count {
        let off = offset + i * 4;
        let string_data_off = read_u32_le(data, off)?;
        validate_offset(string_data_off, data.len())?;

        string_ids.push(StringId { string_data_off });
    }

    Ok(StringTable { data, string_ids })
}

impl<'a> StringTable<'a> {
    pub fn get(&self, index: usize) -> DexResult<String> {
        let string_id = self
            .string_ids
            .get(index)
            .ok_or(DexError::InvalidDex("string index out of bounds"))?;

        let mut offset = string_id.string_data_off as usize;

        // Read the UTF-16 codepoint count (ULEB128)
        let (_utf16_len, new_offset) = read_uleb128(self.data, offset)?;
        offset = new_offset;

        // Read bytes until null terminator (0x00)
        let mut end = offset;
        while *self.data.get(end).ok_or(DexError::InvalidUtf8)? != 0 {
            end += 1;
        }

        let bytes = &self.data[offset..end];
        let s = decode_mutf8(bytes)?;
        Ok(s)
    }
}

pub fn parse_type_ids(data: &[u8], header: &DexHeader) -> DexResult<Vec<TypeId>> {
    let count = header.type_ids_size as usize;
    let offset = header.type_ids_off as usize;

    check_range(
        data,
        offset,
        count
            .checked_mul(4)
            .ok_or(DexError::InvalidDex("type_ids overflow"))?,
    )?;

    let mut type_ids = Vec::with_capacity(count);

    for i in 0..count {
        let off = offset + i * 4;
        let descriptor_idx = read_u32_le(data, off)?;
        type_ids.push(TypeId { descriptor_idx });
    }

    Ok(type_ids)
}

impl TypeId {
    pub fn descriptor<'a>(&self, strings: &StringTable<'a>) -> DexResult<String> {
        strings.get(self.descriptor_idx as usize)
    }
}

pub fn parse_type_list(data: &[u8], offset: u32) -> DexResult<TypeList> {
    if offset == 0 {
        return Ok(TypeList { types: vec![] });
    }

    let offset = offset as usize;
    let size = read_u32_le(data, offset)? as usize;

    // Each type_idx is u16 + 2 bytes padding to align to 4 bytes
    let list_start = offset + 4;
    check_range(data, list_start, size * 2)?;

    let mut types = Vec::with_capacity(size);
    for i in 0..size {
        let type_idx = read_u16_le(data, list_start + i * 2)?;
        types.push(type_idx);
    }

    Ok(TypeList { types })
}

pub fn parse_proto_ids(data: &[u8], header: &DexHeader) -> DexResult<Vec<ProtoId>> {
    let count = header.proto_ids_size as usize;
    let offset = header.proto_ids_off as usize;

    check_range(
        data,
        offset,
        count
            .checked_mul(12)
            .ok_or(DexError::InvalidDex("proto_ids overflow"))?,
    )?;

    let mut protos = Vec::with_capacity(count);

    for i in 0..count {
        let off = offset + i * 12;
        let shorty_idx = read_u32_le(data, off)?;
        let return_type_idx = read_u32_le(data, off + 4)?;
        let parameters_off = read_u32_le(data, off + 8)?;

        protos.push(ProtoId {
            shorty_idx,
            return_type_idx,
            parameters_off,
        });
    }

    Ok(protos)
}

impl ProtoId {
    // Smali/baksmali method descriptors use the "dex descriptor" form:
    //   (paramTypes...)returnType
    //
    // Where each type is a descriptor like:
    //   I, J, D, Ljava/lang/String;, [I, etc.
    pub fn pretty_signature<'a>(
        &self,
        data: &[u8],
        strings: &StringTable<'a>,
        type_ids: &[TypeId],
    ) -> DexResult<String> {
        let ret_type = type_ids
            .get(self.return_type_idx as usize)
            .ok_or(DexError::InvalidDex("return_type_idx out of bounds"))?
            .descriptor(strings)?;

        let type_list = parse_type_list(data, self.parameters_off)?;
        let mut out = String::from("(");
        for &idx in &type_list.types {
            let ty = type_ids
                .get(idx as usize)
                .ok_or(DexError::InvalidDex("parameter type_idx out of bounds"))?
                .descriptor(strings)?;
            out.push_str(&ty);
        }
        out.push(')');
        out.push_str(&ret_type);
        Ok(out)
    }

    pub fn shorty<'a>(&self, strings: &StringTable<'a>) -> DexResult<String> {
        strings.get(self.shorty_idx as usize)
    }
}

pub fn parse_method_ids(data: &[u8], header: &DexHeader) -> DexResult<Vec<MethodId>> {
    let count = header.method_ids_size as usize;
    let offset = header.method_ids_off as usize;

    check_range(
        data,
        offset,
        count
            .checked_mul(8)
            .ok_or(DexError::InvalidDex("method_ids overflow"))?,
    )?;

    let mut methods = Vec::with_capacity(count);

    for i in 0..count {
        let off = offset + i * 8;
        let class_idx = read_u16_le(data, off)?;
        let proto_idx = read_u16_le(data, off + 2)?;
        let name_idx = read_u32_le(data, off + 4)?;

        methods.push(MethodId {
            class_idx,
            proto_idx,
            name_idx,
        });
    }

    Ok(methods)
}

impl MethodId {
    pub fn signature<'a>(
        &self,
        strings: &StringTable<'a>,
        type_ids: &[TypeId],
        proto_ids: &[ProtoId],
        data: &[u8],
    ) -> DexResult<String> {
        // 1 Method name
        let name = strings.get(self.name_idx as usize)?;

        // 2 Proto
        let proto: &ProtoId = proto_ids
            .get(self.proto_idx as usize)
            .ok_or(DexError::InvalidDex("proto_idx out of bounds"))?;

        // 3 Class
        let class_name = type_ids
            .get(self.class_idx as usize)
            .ok_or(DexError::InvalidDex("class_idx out of bounds"))?
            .descriptor(strings)?;

        // 4 Pretty-print full signature
        let params = proto.pretty_signature(data, strings, type_ids)?; // returns (arg_types)->ret_type
        Ok(format!("{}->{}{}", class_name, name, params))
    }

    /// Returns (class name, simple name, descriptor)
    pub fn vmt_triple<'a>(
        &self,
        strings: &StringTable<'a>,
        type_ids: &[TypeId],
        proto_ids: &[ProtoId],
        data: &[u8],
    ) -> DexResult<(String, String, String)> {
        // 1 Method name
        let name = strings.get(self.name_idx as usize)?;

        // 2 Proto
        let proto: &ProtoId = proto_ids
            .get(self.proto_idx as usize)
            .ok_or(DexError::InvalidDex("proto_idx out of bounds"))?;

        // 3 Class
        let class_name = type_ids
            .get(self.class_idx as usize)
            .ok_or(DexError::InvalidDex("class_idx out of bounds"))?
            .descriptor(strings)?;

        // 4 Pretty-print full signature
        let descriptor = proto.pretty_signature(data, strings, type_ids)?; // returns (arg_types)->ret_type
        Ok((class_name, name, descriptor))
    }
}

pub fn parse_class_defs(data: &[u8], header: &DexHeader) -> DexResult<Vec<ClassDef>> {
    let count = header.class_defs_size as usize;
    let offset = header.class_defs_off as usize;

    check_range(
        data,
        offset,
        count
            .checked_mul(32)
            .ok_or(DexError::InvalidDex("class_defs overflow"))?,
    )?;

    let mut class_defs = Vec::with_capacity(count);

    for i in 0..count {
        let off = offset + i * 32;
        let class_idx = read_u32_le(data, off)?;
        let access_flags = read_u32_le(data, off + 4)?;
        let superclass_idx = read_u32_le(data, off + 8)?;
        let interfaces_off = read_u32_le(data, off + 12)?;
        let source_file_idx = read_u32_le(data, off + 16)?;
        let annotations_off = read_u32_le(data, off + 20)?;
        let class_data_off = read_u32_le(data, off + 24)?;
        let static_values_off = read_u32_le(data, off + 28)?;

        class_defs.push(ClassDef {
            class_idx,
            access_flags,
            superclass_idx,
            interfaces_off,
            source_file_idx,
            annotations_off,
            class_data_off,
            static_values_off,
        });
    }

    Ok(class_defs)
}

impl ClassDef {
    pub fn class_name<'a>(
        &self,
        type_ids: &[TypeId],
        strings: &StringTable<'a>,
    ) -> DexResult<String> {
        type_ids
            .get(self.class_idx as usize)
            .ok_or(DexError::InvalidDex("class_idx out of bounds"))?
            .descriptor(strings)
    }

    pub fn source_file<'a>(&self, strings: &StringTable<'a>) -> Option<DexResult<String>> {
        if self.source_file_idx == 0xFFFFFFFF || self.source_file_idx == 0 {
            return None;
        }
        Some(strings.get(self.source_file_idx as usize))
    }
}

pub fn parse_class_data(data: &[u8], offset: u32) -> DexResult<ClassData> {
    if offset == 0 {
        return Ok(ClassData {
            static_fields: vec![],
            instance_fields: vec![],
            direct_methods: vec![],
            virtual_methods: vec![],
        });
    }

    let off = offset as usize;

    let (static_fields_size, off) = read_uleb128(data, off)?;
    let (instance_fields_size, off) = read_uleb128(data, off)?;
    let (direct_methods_size, off) = read_uleb128(data, off)?;
    let (virtual_methods_size, mut off) = read_uleb128(data, off)?;

    // Parse fields
    let mut static_fields = Vec::with_capacity(static_fields_size as usize);
    let mut prev_idx = 0;
    for _ in 0..static_fields_size {
        let (field_idx_diff, n) = read_uleb128(data, off)?;
        off = n;
        let (access_flags, n) = read_uleb128(data, off)?;
        off = n;
        prev_idx += field_idx_diff;
        static_fields.push(EncodedField {
            field_idx: prev_idx,
            access_flags,
        });
    }

    let mut instance_fields = Vec::with_capacity(instance_fields_size as usize);
    prev_idx = 0;
    for _ in 0..instance_fields_size {
        let (field_idx_diff, n) = read_uleb128(data, off)?;
        off = n;
        let (access_flags, n) = read_uleb128(data, off)?;
        off = n;
        prev_idx += field_idx_diff;
        instance_fields.push(EncodedField {
            field_idx: prev_idx,
            access_flags,
        });
    }

    // Parse methods
    let mut direct_methods = Vec::with_capacity(direct_methods_size as usize);
    prev_idx = 0;
    for _ in 0..direct_methods_size {
        let (method_idx_diff, n) = read_uleb128(data, off)?;
        off = n;
        let (access_flags, n) = read_uleb128(data, off)?;
        off = n;
        let (code_off, n) = read_uleb128(data, off)?;
        assert!(code_off == 0 || (code_off as usize) < data.len());
        assert!(code_off % 4 == 0); // code_item is 4-byte aligned
        off = n;
        prev_idx += method_idx_diff;
        direct_methods.push(EncodedMethod {
            method_idx: prev_idx,
            access_flags,
            code_off,
        });
    }

    let mut virtual_methods = Vec::with_capacity(virtual_methods_size as usize);
    prev_idx = 0;
    for _ in 0..virtual_methods_size {
        let (method_idx_diff, n) = read_uleb128(data, off)?;
        off = n;
        let (access_flags, n) = read_uleb128(data, off)?;
        off = n;
        let (code_off, n) = read_uleb128(data, off)?;
        assert!(code_off == 0 || (code_off as usize) < data.len());
        assert!(code_off % 4 == 0); // code_item is 4-byte aligned
        off = n;
        prev_idx += method_idx_diff;
        virtual_methods.push(EncodedMethod {
            method_idx: prev_idx,
            access_flags,
            code_off,
        });
    }

    Ok(ClassData {
        static_fields,
        instance_fields,
        direct_methods,
        virtual_methods,
    })
}

impl ClassDef {
    pub fn parse_class_data(&self, data: &[u8]) -> DexResult<ClassData> {
        parse_class_data(data, self.class_data_off)
    }
}

/// Parse the encoded_catch_handler_list that immediately follows the try_item
/// array inside a code_item.
///
/// See: encoded_catch_handler_list and encoded_catch_handler formats in the
/// DEX spec.
fn parse_catch_handler_list(data: &[u8], list_off: usize) -> DexResult<CatchHandlerList> {
    // Validate that the starting offset is within the buffer; the LEB128
    // helpers will enforce bounds for the rest.
    check_range(data, list_off, 1)?;

    let (size, mut pos) = read_uleb128(data, list_off)?;
    let size_u32 = size;

    let mut handlers = Vec::with_capacity(size_u32 as usize);

    for _ in 0..size_u32 {
        // Relative offset from the start of the list to this handler entry.
        let handler_start = (pos - list_off) as u32;

        let (raw_size, p) = read_sleb128(data, pos)?;
        pos = p;

        let count = raw_size.abs() as u32;
        let mut pairs = Vec::with_capacity(count as usize);

        for _ in 0..count {
            let (type_idx, np) = read_uleb128(data, pos)?;
            pos = np;
            let (addr, np2) = read_uleb128(data, pos)?;
            pos = np2;

            pairs.push(TypeAddrPair { type_idx, addr });
        }

        let catch_all_addr = if raw_size <= 0 {
            let (addr, np) = read_uleb128(data, pos)?;
            pos = np;
            Some(addr)
        } else {
            None
        };

        handlers.push(EncodedCatchHandler {
            raw_size,
            pairs,
            catch_all_addr,
            start_off: handler_start,
        });
    }

    Ok(CatchHandlerList {
        size: size_u32,
        handlers,
    })
}

pub fn parse_code_item(data: &[u8], offset: u32) -> DexResult<CodeItem> {
    let code_off = offset;
    let offset = offset as usize;
    check_range(data, offset, 16)?;

    let registers_size = read_u16_le(data, offset)?;
    let ins_size = read_u16_le(data, offset + 2)?;
    let outs_size = read_u16_le(data, offset + 4)?;
    let tries_size = read_u16_le(data, offset + 6)?;
    let debug_info_off = read_u32_le(data, offset + 8)?;
    let insns_size = read_u32_le(data, offset + 12)? as usize;
    //println!("insns_size: {insns_size}");

    // Read instructions
    let insns_offset = offset + 16;
    check_range(data, insns_offset, insns_size * 2)?;
    let mut insns = Vec::with_capacity(insns_size);
    for i in 0..insns_size {
        insns.push(read_u16_le(data, insns_offset + i * 2)?);
    }

    let mut tries = Vec::new();
    let mut handlers: Option<CatchHandlerList> = None;

    // 4-byte alignment for tries
    let mut next_offset = insns_offset + insns_size * 2;
    if tries_size > 0 && next_offset % 4 != 0 {
        next_offset += 2; // padding
    }

    if tries_size > 0 {
        // Parse try_item array.
        check_range(data, next_offset, tries_size as usize * 8)?; // each try_item = 8 bytes
        for i in 0..tries_size {
            let off = next_offset + i as usize * 8;
            let start_addr = read_u32_le(data, off)?;
            let insn_count = read_u16_le(data, off + 4)?;
            let handler_off = read_u16_le(data, off + 6)?;
            tries.push(TryItem {
                start_addr,
                insn_count,
                handler_off,
            });
        }

        // The encoded_catch_handler_list immediately follows the try_item array.
        let handlers_offset = next_offset + tries_size as usize * 8;
        handlers = Some(parse_catch_handler_list(data, handlers_offset)?);
    }

    Ok(CodeItem {
        registers_size,
        ins_size,
        outs_size,
        tries_size,
        debug_info_off,
        insns,
        tries,
        handlers,
        code_off,
    })
}

impl EncodedMethod {
    pub fn code<'a>(&self, data: &'a [u8]) -> DexResult<Option<CodeItem>> {
        if self.code_off == 0 {
            return Ok(None); // abstract or native
        }
        Ok(Some(parse_code_item(data, self.code_off)?))
    }
}

/// A decoded item in a code_item - can be either a regular instruction or a payload
pub enum DecodedCodeItem {
    Instruction {
        offset: usize,
        inst: crate::instructions::Instruction,
    },
    Payload {
        offset: usize,
        payload: crate::instructions::PayloadInstruction,
    },
}

impl CodeItem {
    /// Returns the absolute byte offset of a `DecodedCodeItem` from the
    /// beginning of the DEX file.
    ///
    /// `item` must have been decoded from this `CodeItem`.
    pub fn absolute_offset(&self, item: &DecodedCodeItem) -> usize {
        // code_item layout: 16-byte fixed header, then insns[]
        self.code_off as usize + 16 + item.offset() * 2
    }

    /// Index of the first parameter register (== registers_size - ins_size).
    pub fn first_param_reg(&self) -> u16 {
        self.registers_size - self.ins_size
    }

    /// True if `reg` is a parameter register.
    pub fn is_param_reg(&self, reg: u16) -> bool {
        reg >= self.first_param_reg() && reg < self.registers_size
    }

    /// True if `reg` is a local (non-parameter) register.
    pub fn is_local_reg(&self, reg: u16) -> bool {
        reg < self.first_param_reg()
    }

    /// Convert a register number to a 0-based parameter index (pN).
    /// Returns `None` if `reg` is not a parameter register.
    pub fn reg_to_p(&self, reg: u16) -> Option<u16> {
        if self.is_param_reg(reg) {
            Some(reg - self.first_param_reg())
        } else {
            None
        }
    }
}

impl DecodedCodeItem {
    /// Offset of this item in 16-bit code units.
    pub fn offset(&self) -> usize {
        match self {
            DecodedCodeItem::Instruction { offset, .. } => *offset,
            DecodedCodeItem::Payload { offset, .. } => *offset,
        }
    }
}

/// Returns indices of call (invoke) instructions that are immediately followed by a move-result.
/// Callers can use this to know which invokes have return values to track.
pub fn call_indices_before_move_result(items: &[DecodedCodeItem]) -> Vec<usize> {
    let mut out = Vec::new();
    let end = items.len().saturating_sub(1);
    for i in 0..end {
        if let DecodedCodeItem::Instruction { inst, .. } = &items[i] {
            if inst.is_invoke() {
                if let DecodedCodeItem::Instruction { inst: next, .. } = &items[i + 1] {
                    if next.is_move_result() {
                        out.push(i);
                    }
                }
            }
        }
    }
    out
}

pub fn decode_code_item(code: &CodeItem) -> Vec<DecodedCodeItem> {
    let mut pc = 0usize;
    let mut out = Vec::new();
    let insns = &code.insns;

    while pc < insns.len() {
        let word = insns[pc];

        // Detect payloads by full 16-bit opcode
        match word {
            0x0100 => {
                // packed-switch-payload
                // Format: size (ushort), first_key (sint), targets[] (sint array)
                let size = insns[pc + 1] as u16;
                let first_key = {
                    let low = insns[pc + 2] as i32;
                    let high = insns[pc + 3] as i32;
                    low | (high << 16)
                };

                let mut targets = Vec::with_capacity(size as usize);
                for i in 0..size as usize {
                    let idx = pc + 4 + i * 2;
                    let low = insns[idx] as i32;
                    let high = insns[idx + 1] as i32;
                    targets.push(low | (high << 16));
                }

                let width = 4 + (size as usize) * 2;
                out.push(DecodedCodeItem::Payload {
                    offset: pc,
                    payload: crate::instructions::PayloadInstruction::PackedSwitch(
                        crate::types::PackedSwitchPayload {
                            size,
                            first_key,
                            targets,
                        },
                    ),
                });

                pc += width;
            }

            0x0200 => {
                // sparse-switch-payload
                // Format: size (ushort), keys[] (sint array), targets[] (sint array)
                let size = insns[pc + 1] as u16;

                let mut keys = Vec::with_capacity(size as usize);
                for i in 0..size as usize {
                    let idx = pc + 2 + i * 2;
                    let low = insns[idx] as u32;
                    let high = insns[idx + 1] as u32;
                    let combined = low | (high << 16);
                    keys.push(combined as i32); // Sign-extend
                }

                let mut targets = Vec::with_capacity(size as usize);
                let targets_start = pc + 2 + (size as usize) * 2;
                for i in 0..size as usize {
                    let idx = targets_start + i * 2;
                    let low = insns[idx] as u32;
                    let high = insns[idx + 1] as u32;
                    let combined = low | (high << 16);
                    targets.push(combined as i32); // Sign-extend
                }

                let width = 2 + (size as usize) * 4;
                out.push(DecodedCodeItem::Payload {
                    offset: pc,
                    payload: crate::instructions::PayloadInstruction::SparseSwitch(
                        crate::types::SparseSwitchPayload {
                            size,
                            keys,
                            targets,
                        },
                    ),
                });

                pc += width;
            }

            0x0300 => {
                // fill-array-data-payload
                // Format: element_width (ushort), size (uint), data[] (ubyte array)
                let element_width = insns[pc + 1] as u16;
                let size = {
                    let low = insns[pc + 2] as u32;
                    let high = insns[pc + 3] as u32;
                    low | (high << 16)
                };

                // Data starts at pc + 4, and spans size * element_width bytes
                // Since insns is u16 array, we need to extract bytes carefully
                let data_size = (size as usize) * (element_width as usize);
                let data_words = (data_size + 1) / 2; // Round up to u16 boundary
                let mut data = Vec::with_capacity(data_size);

                for i in 0..data_words {
                    let word_idx = pc + 4 + i;
                    if word_idx < insns.len() {
                        let word = insns[word_idx];
                        data.push((word & 0xFF) as u8);
                        if data.len() < data_size {
                            data.push((word >> 8) as u8);
                        }
                    }
                }

                // Truncate to exact size
                data.truncate(data_size);

                let width = (data_size + 1) / 2 + 4; // Round up to u16 boundary + header
                out.push(DecodedCodeItem::Payload {
                    offset: pc,
                    payload: crate::instructions::PayloadInstruction::FillArrayData(
                        crate::types::FillArrayDataPayload {
                            element_width,
                            size,
                            data,
                        },
                    ),
                });

                pc += width;
            }

            _ => {
                // Normal instruction
                let (width, inst) = crate::instructions::Instruction::new(&insns[pc..]);
                out.push(DecodedCodeItem::Instruction { offset: pc, inst });
                pc += width;
            }
        }
    }

    out
}

pub fn parse_field_ids(
    data: &[u8],
    field_ids_off: u32,
    field_ids_size: u32,
) -> DexResult<Vec<FieldId>> {
    let mut field_ids = Vec::with_capacity(field_ids_size as usize);
    let mut offset = field_ids_off as usize;

    for _ in 0..field_ids_size {
        let class_idx = read_u16_le(data, offset)?;
        let type_idx = read_u16_le(data, offset + 2)?;
        let name_idx = read_u32_le(data, offset + 4)?;
        offset += 8;

        field_ids.push(FieldId {
            class_idx,
            type_idx,
            name_idx,
        });
    }

    Ok(field_ids)
}

pub fn parse_call_site_ids(
    data: &[u8],
    call_site_ids_off: u32,
    call_site_ids_size: u32,
) -> DexResult<Vec<CallSiteId>> {
    if call_site_ids_size == 0 || call_site_ids_off == 0 {
        return Ok(Vec::new());
    }

    let count = call_site_ids_size as usize;
    let offset = call_site_ids_off as usize;
    check_range(
        data,
        offset,
        count
            .checked_mul(4)
            .ok_or(DexError::InvalidDex("call_site_ids overflow"))?,
    )?;

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = offset + i * 4;
        let call_site_off = read_u32_le(data, off)?;
        validate_offset(call_site_off, data.len())?;
        out.push(CallSiteId { call_site_off });
    }

    Ok(out)
}

pub fn parse_method_handles(
    data: &[u8],
    method_handles_off: u32,
    method_handles_size: u32,
) -> DexResult<Vec<MethodHandle>> {
    if method_handles_size == 0 || method_handles_off == 0 {
        return Ok(Vec::new());
    }

    let count = method_handles_size as usize;
    let offset = method_handles_off as usize;
    check_range(
        data,
        offset,
        count
            .checked_mul(8)
            .ok_or(DexError::InvalidDex("method_handles overflow"))?,
    )?;

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = offset + i * 8;
        let method_handle_type = read_u16_le(data, off)?;
        let unused1 = read_u16_le(data, off + 2)?;
        let field_or_method_id = read_u16_le(data, off + 4)?;
        let unused2 = read_u16_le(data, off + 6)?;

        if unused1 != 0 || unused2 != 0 {
            return Err(DexError::InvalidDex("method_handle_item unused != 0"));
        }

        out.push(MethodHandle {
            method_handle_type,
            field_or_method_id,
        });
    }

    Ok(out)
}

impl FieldId {
    pub fn pretty_name<'a>(&self, cp: &DexConstantPool<'a>) -> DexResult<String> {
        let class_name = cp
            .type_ids
            .get(self.class_idx as usize)
            .ok_or(DexError::InvalidDex("class_idx out of bounds"))?
            .descriptor(&cp.strings)?;

        let field_type = cp
            .type_ids
            .get(self.type_idx as usize)
            .ok_or(DexError::InvalidDex("type_idx out of bounds"))?
            .descriptor(&cp.strings)?;

        let field_name = cp.strings.get(self.name_idx as usize)?;

        Ok(format!("{}->{}:{}", class_name, field_name, field_type))
    }
}

pub fn disassemble_code_item_with_constants(code: &CodeItem, cp: &DexConstantPool) -> Vec<String> {
    let instructions = decode_code_item(code);
    let mut result = Vec::with_capacity(instructions.len());

    for item in instructions {
        let offset = match &item {
            DecodedCodeItem::Instruction { offset, .. } => *offset,
            DecodedCodeItem::Payload { offset, .. } => *offset,
        };

        let display_str = match &item {
            DecodedCodeItem::Instruction { inst, .. } => {
                format!("{}", DispArg(inst, cp))
            }
            DecodedCodeItem::Payload { payload, .. } => {
                format!("{}", DispArg(payload, cp))
            }
        };

        result.push(format!("{:04X}: {}", offset * 2, display_str));
    }

    result
}
