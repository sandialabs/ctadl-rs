// JVM .class file types per JVMS §4.

use std::borrow::Cow;

use crate::error::*;

// --- CONSTANT_Utf8 contents (JVMS §4.4.7) ---

/// The contents of a `CONSTANT_Utf8_info` entry.
///
/// A class file stores strings as UTF-16 code units, so an entry may legally
/// hold surrogates that no Rust `str` can represent: generated lexers abuse
/// string constants as packed UTF-16 tables, and `smaliFlexLexer` has 25
/// deliberately unpaired ones. Well-formed *pairs* are the far more common
/// case -- every emoji, CJK extension and supplementary symbol in a literal is
/// one -- and those recombine into ordinary scalar values.
///
/// So the two cases get two representations: the overwhelmingly common one
/// stays a plain `String`, and only an entry that actually needs code units
/// pays to keep them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JvmString {
    /// Every code unit is a Unicode scalar value (surrogates, if any, paired).
    Utf8(String),
    /// Holds unpaired surrogates; kept as raw UTF-16 code units.
    Utf16(Box<[u16]>),
}

impl JvmString {
    /// Build from the UTF-16 code units of a `CONSTANT_Utf8` entry.
    pub fn from_code_units(units: Vec<u16>) -> Self {
        match String::from_utf16(&units) {
            Ok(s) => JvmString::Utf8(s),
            Err(_) => JvmString::Utf16(units.into_boxed_slice()),
        }
    }

    /// The string, when it is a Unicode scalar sequence; `None` when it holds
    /// unpaired surrogates.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JvmString::Utf8(s) => Some(s.as_str()),
            JvmString::Utf16(_) => None,
        }
    }

    /// The string with each unpaired surrogate replaced by `U+FFFD`.
    ///
    /// For names, display and diagnostics. Borrows in the common case.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        self.to_string_replacing('\u{FFFD}')
    }

    /// The string with each unpaired surrogate replaced by `replacement`.
    ///
    /// The disassembler wants `?` here rather than `U+FFFD`: no charset can
    /// encode a lone surrogate, so `javap`'s own output stream substitutes one,
    /// and matching it keeps the two listings comparable.
    pub fn to_string_replacing(&self, replacement: char) -> Cow<'_, str> {
        match self {
            JvmString::Utf8(s) => Cow::Borrowed(s.as_str()),
            JvmString::Utf16(units) => Cow::Owned(
                char::decode_utf16(units.iter().copied())
                    .map(|r| r.unwrap_or(replacement))
                    .collect(),
            ),
        }
    }

    /// The exact UTF-16 code units, for callers that need the data rather than
    /// text.
    pub fn code_units(&self) -> Box<dyn Iterator<Item = u16> + '_> {
        match self {
            JvmString::Utf8(s) => Box::new(s.encode_utf16()),
            JvmString::Utf16(units) => Box::new(units.iter().copied()),
        }
    }

    /// Number of UTF-16 code units, i.e. what `String.length()` reports in Java.
    pub fn len_utf16(&self) -> usize {
        match self {
            JvmString::Utf8(s) => s.encode_utf16().count(),
            JvmString::Utf16(units) => units.len(),
        }
    }

    /// Position and value of the first unpaired surrogate, if any.
    fn first_unpaired_surrogate(&self) -> Option<(usize, u16)> {
        let JvmString::Utf16(units) = self else {
            return None;
        };
        let mut i = 0;
        while i < units.len() {
            let u = units[i];
            if (0xD800..0xDC00).contains(&u) {
                match units.get(i + 1) {
                    Some(low) if (0xDC00..0xE000).contains(low) => i += 2,
                    _ => return Some((i, u)),
                }
            } else if (0xDC00..0xE000).contains(&u) {
                return Some((i, u));
            } else {
                i += 1;
            }
        }
        None
    }

    /// The string for a call site that requires a `str` (a name or a
    /// descriptor, none of which may legally contain an unpaired surrogate).
    pub fn as_str_or_err(&self) -> ClassFileResult<&str> {
        match self.as_str() {
            Some(s) => Ok(s),
            None => {
                let (index, code_unit) = self.first_unpaired_surrogate().unwrap_or((0, 0));
                Err(ClassFileError::UnpairedSurrogate {
                    cp_index: None,
                    index,
                    code_unit,
                })
            }
        }
    }
}

impl From<&str> for JvmString {
    fn from(s: &str) -> Self {
        JvmString::Utf8(s.to_string())
    }
}

impl From<String> for JvmString {
    fn from(s: String) -> Self {
        JvmString::Utf8(s)
    }
}

// --- Constant pool (JVMS §4.4) ---

/// Constant pool entry. 1-based indexing; Long/Double consume two slots.
#[derive(Debug, Clone)]
pub enum CpEntry {
    Utf8(JvmString),
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

    /// Get the raw contents of a CONSTANT_Utf8 entry by constant pool index.
    pub fn get_jvm_string(&self, index: u16) -> ClassFileResult<&JvmString> {
        match self.get_cp(index)? {
            CpEntry::Utf8(s) => Ok(s),
            _ => Err(ClassFileError::InvalidClassFile("expected Utf8")),
        }
    }

    /// Get UTF-8 string by constant pool index (must be CONSTANT_Utf8).
    ///
    /// Errors when the entry holds unpaired surrogates. That is the right
    /// answer for every caller here -- names and descriptors -- because none of
    /// them may legally contain one; string *constants* should use
    /// [`ClassFile::get_utf8_lossy`] or [`JvmString::code_units`] instead.
    pub fn get_utf8(&self, index: u16) -> ClassFileResult<&str> {
        self.get_jvm_string(index)?
            .as_str_or_err()
            .map_err(|e| e.with_cp_index(index))
    }

    /// Get a CONSTANT_Utf8 entry with unpaired surrogates replaced by `U+FFFD`.
    ///
    /// For string constants and diagnostics, which must not fail just because
    /// the class file carries UTF-16 data a `str` cannot hold.
    pub fn get_utf8_lossy(&self, index: u16) -> ClassFileResult<Cow<'_, str>> {
        Ok(self.get_jvm_string(index)?.to_string_lossy())
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
