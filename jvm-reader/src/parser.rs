use crate::error::*;
use crate::parse_utils::*;
use crate::types::*;

pub const CLASS_FILE_MAGIC: u32 = 0xCAFE_BABE;

/// Parse a .class file from raw bytes.
pub fn parse(data: &[u8]) -> ClassFileResult<ClassFile> {
    let mut pos = 0;

    let magic = read_u32_be(data, pos)?;
    pos += 4;
    if magic != CLASS_FILE_MAGIC {
        return Err(ClassFileError::InvalidMagic);
    }

    let minor_version = read_u16_be(data, pos)?;
    pos += 2;
    let major_version = read_u16_be(data, pos)?;
    pos += 2;

    let constant_pool_count = read_u16_be(data, pos)?;
    pos += 2;

    let mut constant_pool = Vec::new();
    let mut slot = 1u16;
    while slot < constant_pool_count {
        let tag = read_u8(data, pos)?;
        pos += 1;
        let entry = parse_cp_entry(data, &mut pos, tag)?;
        constant_pool.push(Some(entry));
        slot += 1;
        if matches!(
            constant_pool.last(),
            Some(Some(CpEntry::Long(_))) | Some(Some(CpEntry::Double(_)))
        ) {
            constant_pool.push(None);
            slot += 1;
        }
    }

    let access_flags = read_u16_be(data, pos)?;
    pos += 2;
    let this_class = read_u16_be(data, pos)?;
    pos += 2;
    let super_class = read_u16_be(data, pos)?;
    pos += 2;

    let interfaces_count = read_u16_be(data, pos)?;
    pos += 2;
    let mut interfaces = Vec::with_capacity(interfaces_count as usize);
    for _ in 0..interfaces_count {
        interfaces.push(read_u16_be(data, pos)?);
        pos += 2;
    }

    let fields_count = read_u16_be(data, pos)?;
    pos += 2;
    let mut fields = Vec::with_capacity(fields_count as usize);
    for _ in 0..fields_count {
        let (f, next) = parse_field_info(data, pos, &constant_pool)?;
        fields.push(f);
        pos = next;
    }

    let methods_count = read_u16_be(data, pos)?;
    pos += 2;
    let mut methods = Vec::with_capacity(methods_count as usize);
    for _ in 0..methods_count {
        let (m, next) = parse_method_info(data, pos, &constant_pool)?;
        methods.push(m);
        pos = next;
    }

    let attributes_count = read_u16_be(data, pos)?;
    pos += 2;
    let mut attributes = Vec::with_capacity(attributes_count as usize);
    for _ in 0..attributes_count {
        let (a, next) = parse_attribute_info(data, pos)?;
        attributes.push(a);
        pos = next;
    }

    let source_file = attributes
        .iter()
        .find(|a| {
            get_utf8_from_pool(&constant_pool, a.name_index)
                .ok()
                .as_deref()
                == Some("SourceFile")
        })
        .and_then(|a| {
            if a.info.len() >= 2 {
                read_u16_be(&a.info, 0).ok()
            } else {
                None
            }
        });

    Ok(ClassFile {
        magic,
        minor_version,
        major_version,
        constant_pool,
        access_flags,
        this_class,
        super_class,
        interfaces,
        fields,
        methods,
        attributes,
        source_file,
    })
}

fn parse_cp_entry(data: &[u8], pos: &mut usize, tag: u8) -> ClassFileResult<CpEntry> {
    match tag {
        1 => {
            let length = read_u16_be(data, *pos)?;
            *pos += 2;
            let bytes = read_slice(data, *pos, length as usize)?;
            *pos += length as usize;
            let s = decode_modified_utf8(bytes)?;
            Ok(CpEntry::Utf8(s))
        }
        3 => {
            let bytes = read_u32_be(data, *pos)?;
            *pos += 4;
            Ok(CpEntry::Integer(bytes as i32))
        }
        4 => {
            let bytes = read_u32_be(data, *pos)?;
            *pos += 4;
            Ok(CpEntry::Float(bytes))
        }
        5 => {
            let high = read_u32_be(data, *pos)?;
            *pos += 4;
            let low = read_u32_be(data, *pos)?;
            *pos += 4;
            let v = ((high as i64) << 32) | (low as i64);
            Ok(CpEntry::Long(v))
        }
        6 => {
            let high = read_u32_be(data, *pos)?;
            *pos += 4;
            let low = read_u32_be(data, *pos)?;
            *pos += 4;
            Ok(CpEntry::Double((high as u64) << 32 | low as u64))
        }
        7 => {
            let name_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Class { name_index })
        }
        8 => {
            let string_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::String { string_index })
        }
        9 => {
            let class_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let name_and_type_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Fieldref {
                class_index,
                name_and_type_index,
            })
        }
        10 => {
            let class_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let name_and_type_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Methodref {
                class_index,
                name_and_type_index,
            })
        }
        11 => {
            let class_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let name_and_type_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::InterfaceMethodref {
                class_index,
                name_and_type_index,
            })
        }
        12 => {
            let name_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let descriptor_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::NameAndType {
                name_index,
                descriptor_index,
            })
        }
        15 => {
            let reference_kind = read_u8(data, *pos)?;
            *pos += 1;
            let reference_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::MethodHandle {
                reference_kind,
                reference_index,
            })
        }
        16 => {
            let descriptor_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::MethodType { descriptor_index })
        }
        17 => {
            let bootstrap_method_attr_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let name_and_type_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Dynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            })
        }
        18 => {
            let bootstrap_method_attr_index = read_u16_be(data, *pos)?;
            *pos += 2;
            let name_and_type_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::InvokeDynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            })
        }
        19 => {
            let name_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Module { name_index })
        }
        20 => {
            let name_index = read_u16_be(data, *pos)?;
            *pos += 2;
            Ok(CpEntry::Package { name_index })
        }
        _ => Err(ClassFileError::InvalidClassFile(
            "unknown constant pool tag",
        )),
    }
}

fn parse_attribute_info(data: &[u8], pos: usize) -> ClassFileResult<(AttributeInfo, usize)> {
    let name_index = read_u16_be(data, pos)?;
    let length = read_u32_be(data, pos + 2)?;
    let info = read_slice(data, pos + 6, length as usize)?.to_vec();
    Ok((
        AttributeInfo { name_index, info },
        pos + 6 + length as usize,
    ))
}

fn get_utf8_from_pool(pool: &[Option<CpEntry>], name_index: u16) -> ClassFileResult<&str> {
    let i = (name_index as usize)
        .checked_sub(1)
        .ok_or(ClassFileError::InvalidClassFile("cp index 0"))?;
    match pool.get(i).and_then(Option::as_ref) {
        Some(CpEntry::Utf8(s)) => Ok(s.as_str()),
        _ => Err(ClassFileError::InvalidClassFile(
            "expected Utf8 for attribute name",
        )),
    }
}

fn parse_field_info(
    data: &[u8],
    pos: usize,
    _constant_pool: &[Option<CpEntry>],
) -> ClassFileResult<(FieldInfo, usize)> {
    let mut p = pos;
    let access_flags = read_u16_be(data, p)?;
    p += 2;
    let name_index = read_u16_be(data, p)?;
    p += 2;
    let descriptor_index = read_u16_be(data, p)?;
    p += 2;
    let attributes_count = read_u16_be(data, p)?;
    p += 2;
    let mut attributes = Vec::new();
    for _ in 0..attributes_count {
        let (a, next) = parse_attribute_info(data, p)?;
        attributes.push(a);
        p = next;
    }
    Ok((
        FieldInfo {
            access_flags,
            name_index,
            descriptor_index,
            attributes,
        },
        p,
    ))
}

fn parse_method_info(
    data: &[u8],
    pos: usize,
    constant_pool: &[Option<CpEntry>],
) -> ClassFileResult<(MethodInfo, usize)> {
    let mut p = pos;
    let access_flags = read_u16_be(data, p)?;
    p += 2;
    let name_index = read_u16_be(data, p)?;
    p += 2;
    let descriptor_index = read_u16_be(data, p)?;
    p += 2;
    let attributes_count = read_u16_be(data, p)?;
    p += 2;
    let mut attributes = Vec::new();
    let mut code = None;
    for _ in 0..attributes_count {
        let attr_start = p;
        let (a, next) = parse_attribute_info(data, p)?;
        let name = get_utf8_from_pool(constant_pool, a.name_index)?;
        if name == "Code" {
            // `info` begins at attr_start + 6 (name_index u2 + attribute_length u4).
            // First bytecode byte follows max_stack(2), max_locals(2), code_length(4).
            let bytecode_offset = attr_start
                .checked_add(6 + 8)
                .filter(|&o| o <= u32::MAX as usize)
                .ok_or(ClassFileError::InvalidClassFile(
                    "Code attribute offset overflow",
                ))? as u32;
            code = Some(parse_code_attribute(&a.info, bytecode_offset)?);
        }
        attributes.push(a);
        p = next;
    }
    Ok((
        MethodInfo {
            access_flags,
            name_index,
            descriptor_index,
            attributes,
            code,
        },
        p,
    ))
}

fn parse_code_attribute(
    info: &[u8],
    code_byte_offset_in_classfile: u32,
) -> ClassFileResult<CodeAttribute> {
    if info.len() < 8 {
        return Err(ClassFileError::InvalidClassFile("Code attribute too short"));
    }
    let mut pos = 0;
    let max_stack = read_u16_be(info, pos)?;
    pos += 2;
    let max_locals = read_u16_be(info, pos)?;
    pos += 2;
    let code_length = read_u32_be(info, pos)?;
    pos += 4;
    let code = read_slice(info, pos, code_length as usize)?.to_vec();
    pos += code_length as usize;
    let exception_table_length = read_u16_be(info, pos)?;
    pos += 2;
    let mut exception_table = Vec::with_capacity(exception_table_length as usize);
    for _ in 0..exception_table_length {
        let start_pc = read_u16_be(info, pos)?;
        pos += 2;
        let end_pc = read_u16_be(info, pos)?;
        pos += 2;
        let handler_pc = read_u16_be(info, pos)?;
        pos += 2;
        let catch_type = read_u16_be(info, pos)?;
        pos += 2;
        exception_table.push(ExceptionEntry {
            start_pc,
            end_pc,
            handler_pc,
            catch_type,
        });
    }
    let attributes_count = read_u16_be(info, pos)?;
    pos += 2;
    let mut attributes = Vec::new();
    for _ in 0..attributes_count {
        let name_index = read_u16_be(info, pos)?;
        pos += 2;
        let length = read_u32_be(info, pos)?;
        pos += 4;
        let attr_info = read_slice(info, pos, length as usize)?.to_vec();
        pos += length as usize;
        attributes.push(AttributeInfo {
            name_index,
            info: attr_info,
        });
    }
    Ok(CodeAttribute {
        max_stack,
        max_locals,
        code,
        exception_table,
        attributes,
        code_byte_offset_in_classfile,
    })
}

/// High-level parsed view of a .class file, analogous to dex-reader's `DexParser`.
///
/// A .class file defines a single class, so `classes()` yields one item.
pub struct ClassFileParser {
    class_file: ClassFile,
}

impl ClassFileParser {
    /// Parse a .class file buffer into a high-level `ClassFileParser` view.
    pub fn parse(data: &[u8]) -> ClassFileResult<Self> {
        parse(data).map(|class_file| Self { class_file })
    }

    /// Reference to the underlying parsed class file.
    pub fn class_file(&self) -> &ClassFile {
        &self.class_file
    }

    /// Iterate over "classes". A .class file has exactly one class, so this yields one element.
    pub fn classes(&self) -> impl Iterator<Item = &ClassFile> {
        std::iter::once(&self.class_file)
    }

    /// Iterate over all methods declared in this class.
    pub fn methods(&self) -> impl Iterator<Item = &MethodInfo> {
        self.class_file.methods.iter()
    }

    /// Iterate over all fields declared in this class.
    pub fn fields(&self) -> impl Iterator<Item = &FieldInfo> {
        self.class_file.fields.iter()
    }

    /// Iterate over direct superinterface indices (constant pool indices for CONSTANT_Class).
    pub fn interfaces(&self) -> impl Iterator<Item = u16> + '_ {
        self.class_file.interfaces.iter().copied()
    }

    /// Get method by index (0-based into the methods table).
    pub fn get_method(&self, idx: usize) -> Option<&MethodInfo> {
        self.class_file.methods.get(idx)
    }

    /// Get field by index (0-based into the fields table).
    pub fn get_field(&self, idx: usize) -> Option<&FieldInfo> {
        self.class_file.fields.get(idx)
    }

    /// Name of this class (binary name in internal form, e.g. `java/lang/Object`).
    pub fn class_name(&self) -> ClassFileResult<&str> {
        self.class_file.this_class_name()
    }

    /// Human-readable method signature (name + descriptor).
    pub fn method_signature(&self, method: &MethodInfo) -> ClassFileResult<String> {
        let name = self.class_file.get_utf8(method.name_index)?;
        let descriptor = self.class_file.get_utf8(method.descriptor_index)?;
        Ok(format!("{}{}", name, descriptor))
    }

    /// Human-readable method name.
    pub fn method_name(&self, method: &MethodInfo) -> ClassFileResult<String> {
        let name = self.class_file.get_utf8(method.name_index)?;
        Ok(format!("{}", name))
    }

    /// Human-readable method descriptor.
    pub fn method_proto(&self, method: &MethodInfo) -> ClassFileResult<String> {
        let descriptor = self.class_file.get_utf8(method.descriptor_index)?;
        Ok(format!("{}", descriptor))
    }

    /// Human-readable field signature (name : descriptor).
    pub fn field_signature(&self, field: &FieldInfo) -> ClassFileResult<String> {
        let name = self.class_file.get_utf8(field.name_index)?;
        let descriptor = self.class_file.get_utf8(field.descriptor_index)?;
        Ok(format!("{}:{}", name, descriptor))
    }

    /// Get string from constant pool by 1-based index (must be CONSTANT_Utf8).
    pub fn get_string(&self, cp_index: u16) -> ClassFileResult<&str> {
        self.class_file.get_utf8(cp_index)
    }

    /// Code attribute for a method, if present (absent for abstract/native).
    pub fn method_code<'a>(&self, method: &'a MethodInfo) -> Option<&'a CodeAttribute> {
        method.code.as_ref()
    }

    /// Constant pool entry by 1-based index.
    pub fn get_cp(&self, index: u16) -> ClassFileResult<&CpEntry> {
        self.class_file.get_cp(index)
    }

    /// Class name for a constant pool CONSTANT_Class index.
    pub fn get_class_name(&self, cp_index: u16) -> ClassFileResult<&str> {
        self.class_file.get_class_name(cp_index)
    }

    /// Field reference string for a CONSTANT_Fieldref index.
    pub fn get_field_ref(&self, cp_index: u16) -> ClassFileResult<String> {
        self.class_file.get_field_ref(cp_index)
    }

    /// Method reference string for a CONSTANT_Methodref / CONSTANT_InterfaceMethodref index.
    pub fn get_method_ref(&self, cp_index: u16) -> ClassFileResult<String> {
        self.class_file.get_method_ref(cp_index)
    }

    /// Compute basic blocks for a method, if it has code.
    pub fn basic_blocks<'a>(
        &'a self,
        method: &'a MethodInfo,
    ) -> ClassFileResult<Option<crate::flow::MethodBasicBlocks<'a>>> {
        if method.code.is_none() {
            return Ok(None);
        }
        crate::flow::compute_basic_blocks_for_method(self.class_file(), method).map(Some)
    }

    /// Compute basic blocks for a method and normalize stack locations to StackSlot ids.
    pub fn basic_blocks_with_stack_slots<'a>(
        &'a self,
        method: &'a MethodInfo,
    ) -> ClassFileResult<Option<crate::flow::MethodBasicBlocks<'a>>> {
        if method.code.is_none() {
            return Ok(None);
        }
        let mut cfg = crate::flow::compute_basic_blocks_for_method(self.class_file(), method)?;
        crate::flow::normalize_stack_slots_for_method(&mut cfg)?;
        Ok(Some(cfg))
    }
}
