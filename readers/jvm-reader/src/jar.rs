// JAR file support. JAR is ZIP with .class entries (and META-INF, etc.).

use std::path::Path;

use zip::ZipArchive;

use crate::error::{ClassFileError, ClassFileResult};
use crate::flow::InstructionFlowIter;
use crate::parser::ClassFileParser;
use crate::types::{ClassFile, CodeAttribute, CpEntry, FieldInfo, MethodInfo};

/// High-level parsed view of a JAR file.
///
/// Mirrors `ClassFileParser` API: provides the same iterating and accessor methods,
/// but over all .class files in the JAR in sequence.
pub struct JarFileParser {
    parsers: Vec<ClassFileParser>,
    /// The archive path of each class file, in the order of `parsers`.
    entry_names: Vec<String>,
}

impl JarFileParser {
    /// Open and parse a JAR file from the given path. Only .class entries are parsed.
    pub fn open(path: &Path) -> ClassFileResult<Self> {
        let data = std::fs::read(path).map_err(ClassFileError::Io)?;
        Self::from_bytes(&data)
    }

    /// Parse a JAR from an already-open reader (e.g. for testing or in-memory ZIPs).
    pub fn from_reader<R: std::io::Read + std::io::Seek>(mut reader: R) -> ClassFileResult<Self> {
        let mut data = Vec::new();
        reader.read_to_end(&mut data).map_err(ClassFileError::Io)?;
        Self::from_bytes(&data)
    }

    /// Parse a JAR held in memory. The class files are inflated and parsed on the current rayon
    /// pool, each thread reading the archive through a cursor of its own. The classes are in
    /// archive order whatever the thread count, and so is the error: the first failing entry's.
    pub fn from_bytes(data: &[u8]) -> ClassFileResult<Self> {
        use rayon::prelude::*;

        let archive = ZipArchive::new(std::io::Cursor::new(data))
            .map_err(|e| ClassFileError::InvalidZip(e.to_string()))?;
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let name = archive
                .name_for_index(i)
                .expect("an index below the archive's length names an entry")
                .map_err(|e| ClassFileError::InvalidZip(e.to_string()))?;
            if name.ends_with(".class") {
                entries.push((i, name.into_owned()));
            }
        }
        let parsers: Vec<ClassFileResult<ClassFileParser>> = entries
            .par_iter()
            .map_init(
                || archive.clone(),
                |archive, (i, name)| {
                    let mut entry = archive
                        .by_index(*i)
                        .map_err(|e| ClassFileError::InvalidZip(e.to_string()))?;
                    let mut data = Vec::with_capacity(entry.size().try_into().unwrap_or(0));
                    std::io::copy(&mut entry, &mut data).map_err(ClassFileError::Io)?;
                    // Name the entry: without it a whole-JAR failure says only what went
                    // wrong, not which of the thousands of classes it went wrong in.
                    ClassFileParser::parse(&data).map_err(|e| e.in_entry(name))
                },
            )
            .collect();
        let parsers = parsers.into_iter().collect::<ClassFileResult<Vec<_>>>()?;
        let entry_names = entries.into_iter().map(|(_, name)| name).collect();
        Ok(Self {
            parsers,
            entry_names,
        })
    }

    /// The archive path of each class file, in the order of [`Self::class_parsers`].
    pub fn entry_names(&self) -> &[String] {
        &self.entry_names
    }

    /// The class file parsers, one per .class entry in the JAR (in order), for a caller that
    /// frees each once it is done with it.
    pub fn into_class_parsers(self) -> Vec<ClassFileParser> {
        self.parsers
    }

    /// Slice of class file parsers, one per .class entry in the JAR (in order).
    pub fn class_parsers(&self) -> &[ClassFileParser] {
        &self.parsers
    }

    /// Iterate over all classes (one per .class file) in JAR order.
    pub fn classes(&self) -> impl Iterator<Item = &ClassFile> {
        self.parsers.iter().map(ClassFileParser::class_file)
    }

    /// Iterate over all methods from all classes in sequence (class order, then method order).
    pub fn methods(&self) -> impl Iterator<Item = (&ClassFile, &MethodInfo)> {
        self.parsers.iter().flat_map(|p| {
            let cf = p.class_file();
            p.methods().map(move |m| (cf, m))
        })
    }

    /// Iterate over all fields from all classes in sequence.
    pub fn fields(&self) -> impl Iterator<Item = (&ClassFile, &FieldInfo)> {
        self.parsers.iter().flat_map(|p| {
            let cf = p.class_file();
            p.fields().map(move |f| (cf, f))
        })
    }

    /// Iterate over (class, interface_index) for all classes and their direct superinterfaces.
    pub fn interfaces(&self) -> impl Iterator<Item = (&ClassFile, u16)> + '_ {
        self.parsers.iter().flat_map(|p| {
            let cf = p.class_file();
            p.interfaces().map(move |idx| (cf, idx))
        })
    }

    /// Iterate over every instruction in every class (methods with code only), yielding
    /// [InstructionFlowInfo](crate::flow::InstructionFlowInfo) for each.
    pub fn instruction_flow_iter(&self) -> InstructionFlowIter<'_> {
        InstructionFlowIter::new(&self.parsers)
    }

    /// Get the class file parser at index (0-based).
    pub fn get_class_parser(&self, idx: usize) -> Option<&ClassFileParser> {
        self.parsers.get(idx)
    }

    /// Get method by class index and method index (0-based).
    pub fn get_method(&self, class_idx: usize, method_idx: usize) -> Option<&MethodInfo> {
        self.parsers
            .get(class_idx)
            .and_then(|p| p.get_method(method_idx))
    }

    /// Get field by class index and field index (0-based).
    pub fn get_field(&self, class_idx: usize, field_idx: usize) -> Option<&FieldInfo> {
        self.parsers
            .get(class_idx)
            .and_then(|p| p.get_field(field_idx))
    }

    /// Class name for a given class file (binary name in internal form).
    pub fn class_name<'a>(&self, cf: &'a ClassFile) -> ClassFileResult<&'a str> {
        cf.this_class_name()
    }

    /// Human-readable method signature (name + descriptor) using the class file's constant pool.
    pub fn method_signature(&self, cf: &ClassFile, method: &MethodInfo) -> ClassFileResult<String> {
        let name = cf.get_utf8(method.name_index)?;
        let descriptor = cf.get_utf8(method.descriptor_index)?;
        Ok(format!("{} {}", name, descriptor))
    }

    /// Human-readable field signature (name : descriptor).
    pub fn field_signature(&self, cf: &ClassFile, field: &FieldInfo) -> ClassFileResult<String> {
        let name = cf.get_utf8(field.name_index)?;
        let descriptor = cf.get_utf8(field.descriptor_index)?;
        Ok(format!("{}:{}", name, descriptor))
    }

    /// Get string from constant pool by 1-based index (must be CONSTANT_Utf8).
    pub fn get_string<'a>(&self, cf: &'a ClassFile, cp_index: u16) -> ClassFileResult<&'a str> {
        cf.get_utf8(cp_index)
    }

    /// Code attribute for a method, if present.
    pub fn method_code<'a>(&self, method: &'a MethodInfo) -> Option<&'a CodeAttribute> {
        method.code.as_ref()
    }

    /// Constant pool entry by 1-based index.
    pub fn get_cp<'a>(&self, cf: &'a ClassFile, index: u16) -> ClassFileResult<&'a CpEntry> {
        cf.get_cp(index)
    }

    /// Class name for a constant pool CONSTANT_Class index.
    pub fn get_class_name<'a>(&self, cf: &'a ClassFile, cp_index: u16) -> ClassFileResult<&'a str> {
        cf.get_class_name(cp_index)
    }

    /// Field reference string for a CONSTANT_Fieldref index.
    pub fn get_field_ref(&self, cf: &ClassFile, cp_index: u16) -> ClassFileResult<String> {
        cf.get_field_ref(cp_index)
    }

    /// Method reference string for a CONSTANT_Methodref / CONSTANT_InterfaceMethodref index.
    pub fn get_method_ref(&self, cf: &ClassFile, cp_index: u16) -> ClassFileResult<String> {
        cf.get_method_ref(cp_index)
    }
}
