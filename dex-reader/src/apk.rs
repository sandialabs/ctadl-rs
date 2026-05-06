//! APK parsing: read APK (ZIP) files, extract DEX entries, and expose a unified API over all DEXes.

use std::io::Read;

use crate::error::DexResult;
use crate::parser::DecodedCodeItem;
use crate::parser::{DexParser, parse_dex_header};
use crate::types::{ClassData, ClassDef, CodeItem, EncodedMethod, MethodId, TypeList};
use zip::ZipArchive;

/// DEX file entry name pattern: top-level `classes.dex`, `classes2.dex`, etc.
fn is_dex_entry_name(name: &str) -> bool {
    let name = name.trim_start_matches('/');
    if let Some(base) = name.strip_prefix("classes") {
        if base == ".dex" {
            return true;
        }
        // classes2.dex -> base = "2.dex"
        if let Some(num) = base.strip_suffix(".dex") {
            return !num.is_empty() && num.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// Sort key for DEX entry names: "classes.dex" < "classes2.dex" < "classes3.dex" ...
fn dex_entry_sort_key(name: &str) -> (bool, u32) {
    let name = name.trim_start_matches('/');
    if name == "classes.dex" {
        return (true, 0);
    }
    if let Some(rest) = name.strip_prefix("classes") {
        // "2.dex" or ".dex" (already handled above)
        if let Some(num) = rest.strip_suffix(".dex") {
            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                let n: u32 = num.parse().unwrap_or(0);
                return (true, n);
            }
        }
    }
    (false, 0)
}

/// High-level parser for an APK file. Extracts all `classes*.dex` entries and exposes
/// a DexParser-like API by aggregating over all DEX files.
pub struct APKParser {
    dex_files: Vec<ApkDexFile>,
}

struct ApkDexFile {
    name: String,
    buffer: Vec<u8>,
}

impl APKParser {
    /// Parse an APK buffer: open as ZIP, find all `classes*.dex` entries (in canonical order),
    /// decompress and validate each as DEX. Returns error if no DEX entries or any entry fails to parse.
    pub fn new(apk_bytes: &[u8]) -> DexResult<Self> {
        let mut archive = ZipArchive::new(std::io::Cursor::new(apk_bytes))
            .map_err(|_| crate::error::DexError::InvalidDex("APK is not a valid ZIP"))?;

        let mut names: Vec<String> = archive
            .file_names()
            .filter(|n| is_dex_entry_name(n))
            .map(String::from)
            .collect();
        names.sort_by_cached_key(|n| dex_entry_sort_key(n));

        if names.is_empty() {
            return Err(crate::error::DexError::InvalidDex(
                "APK contains no classes*.dex entries",
            ));
        }

        let mut dex_files = Vec::with_capacity(names.len());
        for name in &names {
            let mut zip_file = archive.by_name(name).map_err(|_| {
                crate::error::DexError::InvalidDex("failed to read DEX entry from APK")
            })?;
            let mut buf = Vec::new();
            zip_file.read_to_end(&mut buf).map_err(|_| {
                crate::error::DexError::InvalidDex("failed to decompress DEX entry")
            })?;
            parse_dex_header(&buf)?;
            dex_files.push(ApkDexFile {
                name: name.clone(),
                buffer: buf,
            });
        }

        Ok(Self { dex_files })
    }

    /// Number of DEX files in this APK.
    pub fn dex_count(&self) -> usize {
        self.dex_files.len()
    }

    /// Return one `DexParser` per DEX buffer. Parsers borrow from this APKParser.
    pub fn dex_parsers(&self) -> Vec<DexParser<'_>> {
        self.dex_files
            .iter()
            .map(|dex| DexParser::new(&dex.buffer).expect("already validated in new()"))
            .collect()
    }

    /// Return one `(filename, DexParser)` pair per DEX buffer. Parsers borrow from this APKParser.
    pub fn dex_parsers_with_filenames(&self) -> Vec<(&str, DexParser<'_>)> {
        self.dex_files
            .iter()
            .map(|dex| {
                (
                    dex.name.as_str(),
                    DexParser::new(&dex.buffer).expect("already validated in new()"),
                )
            })
            .collect()
    }

    /// Get a parser for the given DEX index. Panics if index is out of bounds.
    fn parser(&self, dex_index: usize) -> DexParser<'_> {
        DexParser::new(&self.dex_files[dex_index].buffer).expect("already validated in new()")
    }

    /// Iterator over all classes from all DEXes.
    pub fn classes(&self) -> impl Iterator<Item = ApkClass> + '_ {
        (0..self.dex_files.len()).flat_map(move |dex_index| {
            let p = self.parser(dex_index);
            p.classes()
                .map(move |class_def| ApkClass {
                    dex_index,
                    class_def: *class_def,
                })
                .collect::<Vec<_>>()
        })
    }

    /// Class name (descriptor) for an APK-level class.
    pub fn class_name(&self, apk_class: &ApkClass) -> DexResult<String> {
        self.parser(apk_class.dex_index)
            .class_name(&apk_class.class_def)
    }

    /// Class data for an APK-level class.
    pub fn class_data(&self, apk_class: &ApkClass) -> DexResult<ClassData> {
        self.parser(apk_class.dex_index)
            .class_data(&apk_class.class_def)
    }

    /// Methods defined on an APK-level class. Returns APK-level method refs (dex_index + MethodId).
    pub fn class_methods(&self, apk_class: &ApkClass) -> DexResult<Vec<ApkMethod>> {
        let p = self.parser(apk_class.dex_index);
        let method_ids = p.class_methods(&apk_class.class_def)?;
        Ok(method_ids
            .into_iter()
            .map(|m| ApkMethod {
                dex_index: apk_class.dex_index,
                method_id: *m,
            })
            .collect())
    }

    /// Interfaces implemented by an APK-level class.
    pub fn class_interfaces(&self, apk_class: &ApkClass) -> DexResult<TypeList> {
        self.parser(apk_class.dex_index)
            .class_interfaces(&apk_class.class_def)
    }

    /// Human-readable method signature for an APK-level method.
    pub fn method_signature(&self, apk_method: &ApkMethod) -> DexResult<String> {
        self.parser(apk_method.dex_index)
            .method_signature(&apk_method.method_id)
    }

    /// Code item for an encoded method (from class_data). Returns None if abstract/native.
    pub fn method_code(
        &self,
        dex_index: usize,
        method: &EncodedMethod,
    ) -> DexResult<Option<CodeItem>> {
        self.parser(dex_index).method_code(method)
    }

    /// Decoded instructions for an encoded method. Returns None if no code.
    pub fn method_instructions(
        &self,
        dex_index: usize,
        method: &EncodedMethod,
    ) -> DexResult<Option<Vec<DecodedCodeItem>>> {
        self.parser(dex_index).method_instructions(method)
    }
}

/// Reference to a class within an APK (which DEX and which class def).
#[derive(Debug, Clone, Copy)]
pub struct ApkClass {
    pub dex_index: usize,
    pub class_def: ClassDef,
}

/// Reference to a method within an APK (which DEX and which method id).
#[derive(Debug, Clone, Copy)]
pub struct ApkMethod {
    pub dex_index: usize,
    pub method_id: MethodId,
}
