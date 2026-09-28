/*! Telling Ghidra the exact prototype of each JNI native implementation.

The decompiler recovers only the parameters a function body uses, misses arguments passed on the
stack, and often misses the return value. A JNI native's prototype is not a guess, though: the
Java declaration fixes it -- `JNIEnv *`, `jobject` or `jclass`, one C type per Java parameter, and
the C type of the Java return. So before Ghidra decompiles a library for export, `import_pcode`
writes what it knows about the library's natives to a hints file ([`HINTS_FILE`], in the import
directory), and `ExportPcode.java` applies each prototype. See `SignatureRecovery` there.

A native implementation is found in one of two ways, and each gives a hint:

* **By symbol.** The `Java_<class>_<method>` convention, from the `native` methods a Dex
  declares. Only an APK import has the Dex at hand when it imports the libraries, so the caller
  passes these in ([`from_java_natives`]).
* **By address.** A `RegisterNatives` table entry names its descriptor and its function pointer,
  so the library alone is enough ([`from_registry`]). This works for a library imported on its
  own, too.

The symbol mangling here is the JNI spec's ("Resolving Native Method Names"), shared with the
bridge in `ctadl_ascent::languages::jni`, which resolves the same names at index time.
*/

use std::collections::BTreeMap;
use std::path::Path;

use ctadl_import::error::{Error, ErrorContext};

/// File name of the hints, in the import directory. Kept after the import, for inspection.
pub const HINTS_FILE: &str = "jni-signatures.tsv";

/// Mangles one component of a JNI symbol name.
///
/// Per the JNI spec: `/` becomes `_`, `_` becomes `_1`, `;` becomes `_2`, `[` becomes `_3`, ASCII
/// alphanumerics pass through, and anything else becomes `_0` followed by four lowercase hex digits
/// of its UTF-16 code unit (two escapes for a character outside the BMP, which is one surrogate
/// pair).
pub fn mangle_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("_0{:04x}", unit));
                }
            }
        }
    }
    out
}

/// The *short* JNI symbol name: `Java_<class>_<method>`. `class_internal` is the internal form
/// (`com/example/Crypto`), not the type descriptor.
pub fn short_name(class_internal: &str, method: &str) -> String {
    format!(
        "Java_{}_{}",
        mangle_component(class_internal),
        mangle_component(method)
    )
}

/// The *long* JNI symbol name: the short name, `__`, then the mangled parameter descriptor
/// (parentheses and return type stripped, e.g. `Ljava/lang/String;` for
/// `(Ljava/lang/String;)Ljava/lang/String;`).
pub fn long_name(class_internal: &str, method: &str, param_descriptor: &str) -> String {
    format!(
        "{}__{}",
        short_name(class_internal, method),
        mangle_component(param_descriptor)
    )
}

/// Where a hinted native implementation is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HintTarget {
    /// A symbol, e.g. `Java_com_example_Crypto_encrypt`.
    Symbol(String),
    /// An offset from the start of the library's first loadable segment, i.e. from the address
    /// Ghidra calls the image base.
    Offset(u64),
}

/// One native implementation and the Java method descriptor that fixes its prototype.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SignatureHint {
    pub target: HintTarget,
    /// E.g. `(JI)[B`.
    pub descriptor: String,
}

/// Hints for a Dex's `native` methods, given as `(class, simple name, descriptor)`. The class may
/// be a type descriptor (`Lcom/example/Crypto;`) or internal (`com/example/Crypto`).
///
/// Every method gets a hint under its long symbol name. It also gets one under its short name,
/// unless another method in `natives` shares that short name with a different descriptor: an
/// overloaded native is implemented under long names, and a short-name hint would give one
/// overload's prototype to another's function.
pub fn from_java_natives<'a>(
    natives: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<SignatureHint> {
    let mut by_short: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut hints = Vec::new();
    for (class, name, descriptor) in natives {
        let (Some(open), Some(close)) = (descriptor.find('('), descriptor.find(')')) else {
            continue;
        };
        let class = class
            .strip_prefix('L')
            .and_then(|c| c.strip_suffix(';'))
            .unwrap_or(class);
        hints.push(SignatureHint {
            target: HintTarget::Symbol(long_name(class, name, &descriptor[open + 1..close])),
            descriptor: descriptor.to_string(),
        });
        let descriptors = by_short.entry(short_name(class, name)).or_default();
        if !descriptors.iter().any(|d| d == descriptor) {
            descriptors.push(descriptor.to_string());
        }
    }
    for (short, descriptors) in by_short {
        if let [descriptor] = descriptors.as_slice() {
            hints.push(SignatureHint {
                target: HintTarget::Symbol(short),
                descriptor: descriptor.clone(),
            });
        }
    }
    hints.sort();
    hints.dedup();
    hints
}

/// Hints for the `RegisterNatives` tables in an ELF image. Empty for anything that is not an ELF
/// the scan understands, or has no tables.
pub fn from_registry(data: &[u8]) -> Vec<SignatureHint> {
    let mut hints = Vec::new();
    for (offsets, descriptor) in crate::jni_registry::registered_functions(data) {
        for offset in offsets {
            hints.push(SignatureHint {
                target: HintTarget::Offset(offset),
                descriptor: descriptor.clone(),
            });
        }
    }
    hints.sort();
    hints.dedup();
    hints
}

/// Writes `hints` in the format `SignatureRecovery.readHints` in `ExportPcode.java` reads: one
/// tab-separated row each, `sym<TAB><symbol><TAB><descriptor>` or
/// `off<TAB><hex offset><TAB><descriptor>`.
///
/// # Errors
///
/// If the file cannot be written.
pub fn write(path: &Path, hints: &[SignatureHint]) -> Result<(), Error> {
    let mut out =
        String::from("# JNI native prototypes for ExportPcode.java's SignatureRecovery\n");
    for hint in hints {
        match &hint.target {
            HintTarget::Symbol(s) => out.push_str(&format!("sym\t{s}\t{}\n", hint.descriptor)),
            HintTarget::Offset(o) => out.push_str(&format!("off\t{o:x}\t{}\n", hint.descriptor)),
        }
    }
    std::fs::write(path, out)
        .map_err(Error::Io)
        .err_context(|| format!("writing JNI signature hints: '{}'", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(hints: &[SignatureHint]) -> Vec<(&str, &str)> {
        hints
            .iter()
            .filter_map(|h| match &h.target {
                HintTarget::Symbol(s) => Some((s.as_str(), h.descriptor.as_str())),
                HintTarget::Offset(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_native_is_hinted_under_its_long_and_short_names() {
        let hints = from_java_natives([("Lcom/example/Crypto;", "encrypt", "([BI)[B")]);
        assert_eq!(
            symbols(&hints),
            [
                ("Java_com_example_Crypto_encrypt", "([BI)[B"),
                ("Java_com_example_Crypto_encrypt___3BI", "([BI)[B"),
            ]
        );
    }

    /// An overloaded native's short name would pick one overload's prototype for another's body.
    #[test]
    fn an_overloaded_native_is_hinted_under_its_long_names_only() {
        let hints = from_java_natives([("LFoo;", "f", "(I)V"), ("LFoo;", "f", "(J)J")]);
        assert_eq!(
            symbols(&hints),
            [("Java_Foo_f__I", "(I)V"), ("Java_Foo_f__J", "(J)J")]
        );
    }

    /// The same method seen twice (two Dex files, say) is not an overload.
    #[test]
    fn a_native_seen_twice_keeps_its_short_name() {
        let hints = from_java_natives([("LFoo;", "f", "(I)V"), ("LFoo;", "f", "(I)V")]);
        assert_eq!(
            symbols(&hints),
            [("Java_Foo_f", "(I)V"), ("Java_Foo_f__I", "(I)V")]
        );
    }

    #[test]
    fn a_malformed_descriptor_is_skipped() {
        assert!(from_java_natives([("LFoo;", "f", "I")]).is_empty());
    }

    #[test]
    fn a_file_that_is_not_elf_has_no_registry_hints() {
        assert!(from_registry(b"PK\x03\x04 not an ELF").is_empty());
    }

    #[test]
    fn hints_are_written_one_row_each() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(HINTS_FILE);
        let hints = vec![
            SignatureHint {
                target: HintTarget::Symbol("Java_Foo_f".into()),
                descriptor: "(I)V".into(),
            },
            SignatureHint {
                target: HintTarget::Offset(0x1f00),
                descriptor: "(J)J".into(),
            },
        ];
        write(&path, &hints).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(rows, ["sym\tJava_Foo_f\t(I)V", "off\t1f00\t(J)J"]);
    }
}
