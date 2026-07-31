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

// ---------------------------------------------------------------------------
// Native libraries (`lib/<abi>/*.so`)
// ---------------------------------------------------------------------------
//
// These are free functions over the APK bytes rather than methods on `APKParser`:
// `APKParser::new` keeps only the decompressed DEX buffers and drops the archive, so
// by the time it has returned the `lib/<abi>/` entries are gone.

/// One `lib/<abi>/<file_name>.so` entry in an APK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLibEntry {
    /// The ZIP entry name, e.g. `lib/arm64-v8a/libfoo.so`.
    pub entry_name: String,
    /// The ABI directory, e.g. `arm64-v8a`.
    pub abi: String,
    /// The base file name, e.g. `libfoo.so`. Never contains a path separator, so a
    /// caller that builds an on-disk path from `abi` + `file_name` (rather than from
    /// `entry_name`) cannot be walked out of its target directory by a hostile APK.
    pub file_name: String,
}

/// ABI directories in the order CTADL prefers them.
///
/// An APK usually ships the same library built for several ABIs. They are copies of
/// one program, so importing more than one costs a full disassembly per copy and
/// leaves duplicate `Java_…` symbols that the JNI bridge can only report as
/// ambiguous. Callers pick exactly one; this is the order.
pub const ABI_PREFERENCE: &[&str] = &["arm64-v8a", "armeabi-v7a", "armeabi", "x86_64", "x86"];

/// Splits a `lib/<abi>/<name>.so` entry name into its ABI and base name. Returns
/// `None` for anything else, including a nested path under `lib/<abi>/`.
fn parse_native_lib_entry(name: &str) -> Option<(&str, &str)> {
    let rest = name.trim_start_matches('/').strip_prefix("lib/")?;
    let (abi, file_name) = rest.split_once('/')?;
    if abi.is_empty() || !file_name.ends_with(".so") || file_name.contains('/') {
        return None;
    }
    Some((abi, file_name))
}

/// Every `lib/<abi>/*.so` entry in the APK, sorted by entry name.
///
/// This only reads the ZIP central directory; nothing is decompressed. Returns an
/// empty vector for an APK with no native libraries.
pub fn native_lib_entries(apk_bytes: &[u8]) -> DexResult<Vec<NativeLibEntry>> {
    let archive = ZipArchive::new(std::io::Cursor::new(apk_bytes))
        .map_err(|_| crate::error::DexError::InvalidDex("APK is not a valid ZIP"))?;
    Ok(collect_native_lib_entries(&archive))
}

/// [`native_lib_entries`] against an APK on disk.
///
/// Only the ZIP central directory is read -- the archive is seeked, never loaded into
/// memory and never decompressed -- so a caller that just wants to know *whether* an
/// APK carries native code can ask without paying for a several-hundred-megabyte read.
pub fn native_lib_entries_of_file(path: &std::path::Path) -> DexResult<Vec<NativeLibEntry>> {
    let file = std::fs::File::open(path)
        .map_err(|_| crate::error::DexError::InvalidDex("cannot open APK"))?;
    let archive = ZipArchive::new(file)
        .map_err(|_| crate::error::DexError::InvalidDex("APK is not a valid ZIP"))?;
    Ok(collect_native_lib_entries(&archive))
}

/// The `lib/<abi>/*.so` entries of an already-opened archive, sorted by entry name.
fn collect_native_lib_entries<R: std::io::Read + std::io::Seek>(
    archive: &ZipArchive<R>,
) -> Vec<NativeLibEntry> {
    let mut entries: Vec<NativeLibEntry> = archive
        .file_names()
        .filter_map(|name| {
            parse_native_lib_entry(name).map(|(abi, file_name)| NativeLibEntry {
                entry_name: name.to_string(),
                abi: abi.to_string(),
                file_name: file_name.to_string(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.entry_name.cmp(&b.entry_name));
    entries
}

/// The single ABI to import out of `entries`: the first [`ABI_PREFERENCE`] hit
/// present, else the lexicographically first ABI present. `None` when `entries` is
/// empty.
pub fn preferred_abi(entries: &[NativeLibEntry]) -> Option<&str> {
    for preferred in ABI_PREFERENCE {
        if let Some(entry) = entries.iter().find(|e| e.abi == *preferred) {
            return Some(&entry.abi);
        }
    }
    // An ABI directory we have never heard of (a new Android ABI, or a repackaged
    // app) is still worth importing; take the first by name so the choice is stable.
    entries.iter().map(|e| e.abi.as_str()).min()
}

/// Decompresses every `lib/<abi>/*.so` entry for one ABI, in one pass over the
/// archive. Entries are returned sorted by entry name, as in [`native_lib_entries`].
pub fn read_native_libs(apk_bytes: &[u8], abi: &str) -> DexResult<Vec<(NativeLibEntry, Vec<u8>)>> {
    let mut archive = ZipArchive::new(std::io::Cursor::new(apk_bytes))
        .map_err(|_| crate::error::DexError::InvalidDex("APK is not a valid ZIP"))?;

    let mut out = Vec::new();
    for index in 0..archive.len() {
        let mut zip_file = archive
            .by_index(index)
            .map_err(|_| crate::error::DexError::InvalidDex("failed to read entry from APK"))?;
        let Some((entry_abi, file_name)) = parse_native_lib_entry(zip_file.name()) else {
            continue;
        };
        if entry_abi != abi {
            continue;
        }
        let entry = NativeLibEntry {
            entry_name: zip_file.name().to_string(),
            abi: entry_abi.to_string(),
            file_name: file_name.to_string(),
        };
        let mut buf = Vec::new();
        zip_file.read_to_end(&mut buf).map_err(|_| {
            crate::error::DexError::InvalidDex("failed to decompress native library entry")
        })?;
        out.push((entry, buf));
    }
    out.sort_by(|(a, _), (b, _)| a.entry_name.cmp(&b.entry_name));
    Ok(out)
}

/// True when `bytes` opens with an object-file magic a disassembler can load.
///
/// Used to skip a mis-named `lib/<abi>/*.so` entry (an app's data file, a stub, a
/// text placeholder) instead of spending a full disassembly run discovering it is
/// not code.
///
/// Mach-O is accepted alongside ELF even though a real APK only ever contains ELF:
/// the end-to-end regression case builds its fixture library with the host
/// compiler, which emits Mach-O on a macOS worker. Note that the fat-binary magic
/// `0xcafebabe` is also the Java class-file magic — harmless here, since this is
/// only ever asked about an entry already known to sit at `lib/<abi>/*.so`.
pub fn looks_like_object_file(bytes: &[u8]) -> bool {
    let Some(magic) = bytes.get(..4) else {
        return false;
    };
    matches!(
        magic,
        // ELF
        b"\x7fELF"
        // Mach-O, 32- and 64-bit, both byte orders
        | [0xfe, 0xed, 0xfa, 0xce] | [0xce, 0xfa, 0xed, 0xfe]
        | [0xfe, 0xed, 0xfa, 0xcf] | [0xcf, 0xfa, 0xed, 0xfe]
        // Mach-O universal ("fat") binaries, 32- and 64-bit, both byte orders
        | [0xca, 0xfe, 0xba, 0xbe] | [0xbe, 0xba, 0xfe, 0xca]
        | [0xca, 0xfe, 0xba, 0xbf] | [0xbf, 0xba, 0xfe, 0xca]
    )
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
    /// decompress and validate each as DEX. Returns error if the buffer is not a ZIP or any
    /// DEX entry fails to parse.
    ///
    /// An APK with *no* `classes*.dex` is not an error here: a split APK out of an Android
    /// App Bundle (the `config.<abi>.apk` in an XAPK, say) is a real, valid APK that carries
    /// only native libraries or only resources. Every class-level accessor below is then
    /// simply empty. A caller that has no use for such an APK checks [`Self::dex_count`].
    pub fn new(apk_bytes: &[u8]) -> DexResult<Self> {
        let mut archive = ZipArchive::new(std::io::Cursor::new(apk_bytes))
            .map_err(|_| crate::error::DexError::InvalidDex("APK is not a valid ZIP"))?;

        let mut names: Vec<String> = archive
            .file_names()
            .filter(|n| is_dex_entry_name(n))
            .map(String::from)
            .collect();
        names.sort_by_cached_key(|n| dex_entry_sort_key(n));

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

#[cfg(test)]
mod native_lib_tests {
    use super::*;
    use std::io::Write;

    /// Builds an in-memory ZIP from `(name, contents)` pairs.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const ELF: &[u8] = b"\x7fELFsome code";

    fn sample_apk() -> Vec<u8> {
        zip_of(&[
            ("classes.dex", b"dex\n035\0"),
            ("lib/arm64-v8a/libfoo.so", ELF),
            ("lib/arm64-v8a/libbar.so", ELF),
            ("lib/arm64-v8a/notes.txt", b"not a library"),
            ("lib/x86/libfoo.so", ELF),
            ("assets/data.bin", ELF),
        ])
    }

    #[test]
    fn lists_only_native_libraries_under_lib_abi() {
        let entries = native_lib_entries(&sample_apk()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.entry_name.as_str()).collect();
        // `notes.txt` has no `.so` suffix and `assets/data.bin` is not under `lib/`,
        // so neither is a candidate. Sorted by entry name.
        assert_eq!(
            names,
            [
                "lib/arm64-v8a/libbar.so",
                "lib/arm64-v8a/libfoo.so",
                "lib/x86/libfoo.so",
            ]
        );
        assert_eq!(entries[0].abi, "arm64-v8a");
        assert_eq!(entries[0].file_name, "libbar.so");
    }

    #[test]
    fn an_apk_with_no_native_libraries_lists_nothing() {
        let apk = zip_of(&[("classes.dex", b"dex\n035\0"), ("res/x.xml", b"<x/>")]);
        assert!(native_lib_entries(&apk).unwrap().is_empty());
    }

    /// A split APK out of an app bundle carries native libraries and no Dex at all.
    #[test]
    fn native_libraries_are_found_without_any_dex() {
        let apk = zip_of(&[("lib/armeabi-v7a/libfoo.so", ELF)]);
        let entries = native_lib_entries(&apk).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].abi, "armeabi-v7a");
    }

    /// Listing from disk sees exactly what listing from bytes sees. Written by hand
    /// rather than with `tempfile`: this crate is workspace-excluded and built offline
    /// from its own lockfile, so one test is not worth a dependency.
    #[test]
    fn listing_from_a_file_matches_listing_from_bytes() {
        let path = std::env::temp_dir().join("dex_reader_native_lib_listing_test.apk");
        std::fs::write(&path, sample_apk()).unwrap();
        let from_file = native_lib_entries_of_file(&path);
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            from_file.unwrap(),
            native_lib_entries(&sample_apk()).unwrap()
        );
    }

    /// An APK carrying only native libraries parses, with no classes in it. This is the
    /// `config.<abi>.apk` of an XAPK; rejecting it would take its libraries with it.
    #[test]
    fn an_apk_with_no_dex_entries_parses_as_empty() {
        let apk = zip_of(&[
            ("AndroidManifest.xml", b"\x03\x00\x08\x00"),
            ("lib/arm64-v8a/libfoo.so", ELF),
        ]);
        let parser = APKParser::new(&apk).unwrap();
        assert_eq!(parser.dex_count(), 0);
        assert_eq!(parser.dex_parsers_with_filenames().len(), 0);
        assert_eq!(parser.classes().count(), 0);
    }

    /// Whatever it holds, it still has to be a ZIP.
    #[test]
    fn a_non_zip_is_still_rejected() {
        assert!(APKParser::new(b"not a zip at all").is_err());
    }

    #[test]
    fn preferred_abi_follows_the_preference_order() {
        let entries = native_lib_entries(&sample_apk()).unwrap();
        assert_eq!(preferred_abi(&entries), Some("arm64-v8a"));

        // x86 beats x86_64? No -- 64-bit first.
        let apk = zip_of(&[("lib/x86/a.so", ELF), ("lib/x86_64/a.so", ELF)]);
        let entries = native_lib_entries(&apk).unwrap();
        assert_eq!(preferred_abi(&entries), Some("x86_64"));
    }

    #[test]
    fn an_unknown_abi_is_still_selected_deterministically() {
        let apk = zip_of(&[("lib/riscv64/a.so", ELF), ("lib/loongarch64/a.so", ELF)]);
        let entries = native_lib_entries(&apk).unwrap();
        assert_eq!(preferred_abi(&entries), Some("loongarch64"));
        assert_eq!(preferred_abi(&[]), None);
    }

    #[test]
    fn reads_only_the_requested_abis_libraries() {
        let libs = read_native_libs(&sample_apk(), "arm64-v8a").unwrap();
        let names: Vec<&str> = libs.iter().map(|(e, _)| e.file_name.as_str()).collect();
        assert_eq!(names, ["libbar.so", "libfoo.so"]);
        assert!(libs.iter().all(|(_, bytes)| bytes == ELF));

        assert_eq!(read_native_libs(&sample_apk(), "mips").unwrap().len(), 0);
    }

    /// A hostile entry name cannot escape the ABI directory, and is not even
    /// recognized as a library: `file_name` is a single path component by construction.
    #[test]
    fn a_nested_or_traversing_entry_name_is_not_a_library() {
        assert_eq!(parse_native_lib_entry("lib/arm64-v8a/sub/evil.so"), None);
        assert_eq!(parse_native_lib_entry("lib/../../evil.so"), None);
        assert_eq!(parse_native_lib_entry("lib//evil.so"), None);
        assert_eq!(parse_native_lib_entry("lib/arm64-v8a/"), None);
        assert_eq!(
            parse_native_lib_entry("lib/arm64-v8a/ok.so"),
            Some(("arm64-v8a", "ok.so"))
        );
    }

    #[test]
    fn object_file_magic_accepts_elf_and_mach_o_and_rejects_junk() {
        assert!(looks_like_object_file(b"\x7fELF\x02\x01\x01"));
        assert!(looks_like_object_file(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c]));
        assert!(!looks_like_object_file(b"not a library"));
        assert!(!looks_like_object_file(b"PK\x03\x04"));
        assert!(!looks_like_object_file(b"\x7fEL"));
        assert!(!looks_like_object_file(b""));
    }
}
