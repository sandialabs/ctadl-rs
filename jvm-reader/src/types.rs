// JVM .class file types per JVMS §4.

use crate::error::*;

// --- Constant pool (JVMS §4.4) ---

/// Constant pool entry. 1-based indexing; Long/Double consume two slots.
#[derive(Debug, Clone)]
pub enum CpEntry {
    Utf8(String),
    Integer(i32),
    Float(u32),
    Long(i64),
    Double(u64),
    Class {
        name_index: u16,
    },
    String {
        string_index: u16,
    },
    Fieldref {
        class_index: u16,
        name_and_type_index: u16,
    },
    Methodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    InterfaceMethodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    MethodType {
        descriptor_index: u16,
    },
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    Module {
        name_index: u16,
    },
    Package {
        name_index: u16,
    },
}

// --- ClassFile (JVMS §4.1) ---

#[derive(Debug, Clone)]
pub struct ClassFile {
    pub magic: u32,
    pub minor_version: u16,
    pub major_version: u16,
    /// Constant pool; 1-based index i -> pool[(i-1)]. None = unusable slot after Long/Double.
    pub constant_pool: Vec<Option<CpEntry>>,
    pub access_flags: u16,
    pub this_class: u16,
    pub super_class: u16,
    pub interfaces: Vec<u16>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub attributes: Vec<AttributeInfo>,
    /// SourceFile attribute: 1-based constant pool index of the source file name (Utf8), if present.
    pub source_file: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub access_flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub attributes: Vec<AttributeInfo>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub access_flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub attributes: Vec<AttributeInfo>,
    pub code: Option<CodeAttribute>,
}

#[derive(Debug, Clone)]
pub struct AttributeInfo {
    pub name_index: u16,
    pub info: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CodeAttribute {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    pub exception_table: Vec<ExceptionEntry>,
    pub attributes: Vec<AttributeInfo>,
    /// Byte offset in the raw `.class` file where the method `code` array starts (first opcode byte).
    ///
    /// For a class loaded from a JAR, this is relative to the start of that entry's decompressed bytes,
    /// not an offset inside the ZIP archive.
    pub code_byte_offset_in_classfile: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptionEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    pub catch_type: u16,
}

// --- Constant pool accessors ---

impl ClassFile {
    /// Get constant pool entry by 1-based index.
    pub fn get_cp(&self, index: u16) -> ClassFileResult<&CpEntry> {
        let i = (index as usize)
            .checked_sub(1)
            .ok_or(ClassFileError::InvalidClassFile("cp index 0"))?;
        self.constant_pool
            .get(i)
            .and_then(Option::as_ref)
            .ok_or(ClassFileError::InvalidClassFile("invalid cp index"))
    }

    /// Get UTF-8 string by constant pool index (must be CONSTANT_Utf8).
    pub fn get_utf8(&self, index: u16) -> ClassFileResult<&str> {
        match self.get_cp(index)? {
            CpEntry::Utf8(s) => Ok(s.as_str()),
            _ => Err(ClassFileError::InvalidClassFile("expected Utf8")),
        }
    }

    /// Get class name (binary name in internal form, e.g. "java/lang/Object") by CONSTANT_Class index.
    pub fn get_class_name(&self, class_cp_index: u16) -> ClassFileResult<&str> {
        match self.get_cp(class_cp_index)? {
            CpEntry::Class { name_index } => self.get_utf8(*name_index),
            _ => Err(ClassFileError::InvalidClassFile("expected Class")),
        }
    }

    /// Get field ref as "ClassName.name:descriptor" (javap style).
    pub fn get_field_ref(&self, cp_index: u16) -> ClassFileResult<String> {
        match self.get_cp(cp_index)? {
            CpEntry::Fieldref {
                class_index,
                name_and_type_index,
            } => {
                let class_name = self.get_class_name(*class_index)?;
                let (name, descriptor) = self.get_name_and_type(*name_and_type_index)?;
                Ok(format!("{}.{}:{}", class_name, name, descriptor))
            }
            _ => Err(ClassFileError::InvalidClassFile("expected Fieldref")),
        }
    }

    /// Get method/interface method ref for javap comment style: ClassName."name":descriptor (quotes for &lt;init&gt;/&lt;clinit&gt;).
    pub fn get_method_ref(&self, cp_index: u16) -> ClassFileResult<String> {
        match self.get_cp(cp_index)? {
            CpEntry::Methodref {
                class_index,
                name_and_type_index,
            }
            | CpEntry::InterfaceMethodref {
                class_index,
                name_and_type_index,
            } => {
                let class_name = self.get_class_name(*class_index)?;
                let (name, descriptor) = self.get_name_and_type(*name_and_type_index)?;
                let name_part = if name == "<init>" || name == "<clinit>" {
                    format!("\"{}\"", name)
                } else {
                    name.to_string()
                };
                Ok(format!("{}.{}:{}", class_name, name_part, descriptor))
            }
            _ => Err(ClassFileError::InvalidClassFile(
                "expected Methodref or InterfaceMethodref",
            )),
        }
    }

    /// Get NameAndType as (name, descriptor).
    pub fn get_name_and_type(&self, cp_index: u16) -> ClassFileResult<(&str, &str)> {
        match self.get_cp(cp_index)? {
            CpEntry::NameAndType {
                name_index,
                descriptor_index,
            } => {
                let name = self.get_utf8(*name_index)?;
                let descriptor = self.get_utf8(*descriptor_index)?;
                Ok((name, descriptor))
            }
            _ => Err(ClassFileError::InvalidClassFile("expected NameAndType")),
        }
    }

    /// This class binary name (internal form).
    pub fn this_class_name(&self) -> ClassFileResult<&str> {
        self.get_class_name(self.this_class)
    }
}
