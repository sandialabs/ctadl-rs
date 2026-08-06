/*!
Importing an app bundle (`.xapk`).

A bundle is a ZIP of split APKs: the Dex in the base, the native libraries in `config.<abi>.apk`,
and -- usually the majority -- one resource-only split per language and screen density. Two things
about that shape are easy to get wrong and expensive to get wrong quietly:

* a resource-only split holds no code in either language, and must be skipped rather than fail the
  whole import;
* the sub-import list must be **flat**, because [`AnalysisProject::ephemeral`] expands exactly one
  level. Nesting bundle → split → library drops every `.so` at index time, which looks exactly
  like the JNI bug the registry work exists to fix.

Ghidra is not required: with none installed the native split imports as an APK with no native
sub-imports, and everything asserted here still holds.
*/

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Once;

use ctadl_ascent::cli;
use ctadl_ascent::project::*;
use tempfile::tempdir;

static INIT: Once = Once::new();

/// Points the store at a temp directory, once per process.
fn store_root() -> PathBuf {
    INIT.call_once(|| {
        let dir = tempdir().unwrap();
        // Leaked on purpose: the store has to outlive every test in this binary.
        init_store_path(Some(Box::leak(Box::new(dir)).path())).unwrap();
    });
    StorePaths::root().to_path_buf()
}

/// A ZIP of `(entry name, contents)`, stored uncompressed so the fixtures stay readable.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }
    out
}

/// A DEX with a header, a map list naming only the header, and no classes. Valid enough to
/// parse; the bundle test is about packaging, not about what the Dex frontend recovers.
fn empty_dex() -> Vec<u8> {
    const HEADER: u32 = 112;
    const SIZE: u32 = 128;
    let mut dex = vec![0u8; SIZE as usize];
    dex[..8].copy_from_slice(b"dex\n035\0");
    dex[0x20..0x24].copy_from_slice(&SIZE.to_le_bytes()); // file_size
    dex[0x24..0x28].copy_from_slice(&HEADER.to_le_bytes()); // header_size
    dex[0x28..0x2c].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // endian tag
    dex[0x34..0x38].copy_from_slice(&HEADER.to_le_bytes()); // map_off
    // map_list: one entry, `header_item` x1 at offset 0.
    dex[112..116].copy_from_slice(&1u32.to_le_bytes());
    dex[116..118].copy_from_slice(&0u16.to_le_bytes()); // TYPE_HEADER_ITEM
    dex[120..124].copy_from_slice(&1u32.to_le_bytes()); // size
    dex[124..128].copy_from_slice(&0u32.to_le_bytes()); // offset
    dex
}

/// Enough of an ELF that `looks_like_object_file` accepts it. Whether Ghidra can do anything
/// with it is beside the point and is why the native sub-import is not asserted on.
const FAKE_ELF: &[u8] = b"\x7fELF\x02\x01\x01\x00 not really a library";

/// A three-split bundle. The base APK is named so that it sorts *after* the config split, which
/// is what makes the Dex-first ordering below an assertion rather than a coincidence.
fn bundle() -> Vec<u8> {
    zip_of(&[
        (
            "config.arm64_v8a.apk",
            &zip_of(&[("lib/arm64-v8a/libfoo.so", FAKE_ELF)]),
        ),
        (
            "config.en.apk",
            &zip_of(&[("res/values.xml", b"<resources/>")]),
        ),
        ("myapp.apk", &zip_of(&[("classes.dex", &empty_dex())])),
    ])
}

#[test]
fn a_bundle_imports_its_code_splits_and_skips_its_resource_split() {
    let root = store_root();
    let dir = tempdir().unwrap();
    let path = dir.path().join("myapp.xapk");
    fs::write(&path, bundle()).unwrap();

    let import = ArtifactImport::try_create("bundle", ArtifactLanguage::Xapk, &path).unwrap();
    cli::import(&import, cli::ImportOptions::default()).expect("importing the bundle");
    let import = ArtifactImport::load_by_name("bundle").unwrap();

    // Every split was extracted, including the one that is not imported: the skip is a decision
    // about the split's contents, taken after it is on disk.
    let splits: Vec<String> = fs::read_dir(root.join("imports").join("bundle").join("splits"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(splits.len(), 3, "{splits:?}");

    // The resource-only split holds no code in either language and contributes no import.
    assert_eq!(
        import.sub_imports,
        ["bundle__myapp", "bundle__config.arm64_v8a"],
        "the Dex-bearing split comes first, and the resource-only split is skipped"
    );
    assert!(ArtifactImport::load_by_name("bundle__config.en").is_err());

    // The bundle itself contributes no program of its own; the code is all in the splits.
    assert!(import.program_path().is_file());
}

/// The flattening regression, stated as what [`AnalysisProject::ephemeral`] does: it chains a
/// name with its *own* `sub_imports` and does not recurse, so a native library two levels down
/// is reachable only if the bundle's list already names it.
#[test]
fn ephemeral_expands_one_level_so_the_bundle_list_must_be_flat() {
    store_root();
    let dir = tempdir().unwrap();
    let artifact = dir.path().join("stand-in");
    fs::write(&artifact, b"contents").unwrap();

    // A split that produced one native library, as an APK import does.
    let mut split =
        ArtifactImport::try_create("flat__split", ArtifactLanguage::Apk, &artifact).unwrap();
    split.sub_imports = vec!["flat__split__lib".to_string()];
    split.save().unwrap();
    ArtifactImport::try_create("flat__split__lib", ArtifactLanguage::Pcode, &artifact).unwrap();

    // What a bundle import must record: the split *and* what the split produced.
    let mut flat = ArtifactImport::try_create("flat", ArtifactLanguage::Xapk, &artifact).unwrap();
    flat.sub_imports = vec!["flat__split".to_string(), "flat__split__lib".to_string()];
    flat.save().unwrap();
    assert_eq!(
        AnalysisProject::ephemeral("p", &["flat"]).imports,
        ["flat", "flat__split", "flat__split__lib"]
    );

    // And what it must not: nesting alone loses the library, silently.
    let mut nested =
        ArtifactImport::try_create("nested", ArtifactLanguage::Xapk, &artifact).unwrap();
    nested.sub_imports = vec!["flat__split".to_string()];
    nested.save().unwrap();
    assert_eq!(
        AnalysisProject::ephemeral("p", &["nested"]).imports,
        ["nested", "flat__split"],
        "expansion is one level deep, so a nested list drops the library"
    );
}
