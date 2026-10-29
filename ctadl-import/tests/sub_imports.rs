/*! Tests which sub-imports a project expands to under [`SubImports`].

`ctadl index --no-native-libs` drops the native libraries an APK or XAPK import expanded to, and
keeps everything else: the parent, an XAPK's split APKs, and any import named explicitly.
*/

use std::sync::Once;

use ctadl_import::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};

/// The store root belongs to the whole process and can be set only once, so every test in this
/// binary shares a single store.
static INIT: Once = Once::new();

fn store() {
    INIT.call_once(|| {
        let dir = tempfile::tempdir().unwrap();
        init_store_path(Some(Box::leak(Box::new(dir)).path())).unwrap();
    });
}

/// Writes an import config called `name` with the given sub-imports. No program is written:
/// expansion reads configs only.
fn import(name: &str, language: ArtifactLanguage, subs: &[&str]) {
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("artifact");
    std::fs::write(&artifact, b"contents").unwrap();
    let mut import = ArtifactImport::try_create(name, language, &artifact).unwrap();
    import.sub_imports = subs.iter().map(|s| s.to_string()).collect();
    import.save().unwrap();
}

#[test]
fn no_native_libs_leaves_only_the_apk() {
    store();
    import("apk__arm64-v8a__libx", ArtifactLanguage::Pcode, &[]);
    import("apk__arm64-v8a__liby", ArtifactLanguage::Pcode, &[]);
    import(
        "apk",
        ArtifactLanguage::Apk,
        &["apk__arm64-v8a__libx", "apk__arm64-v8a__liby"],
    );

    assert_eq!(
        AnalysisProject::ephemeral("p", &["apk"], SubImports::All).imports,
        ["apk", "apk__arm64-v8a__libx", "apk__arm64-v8a__liby"]
    );
    assert_eq!(
        AnalysisProject::ephemeral("p", &["apk"], SubImports::NoNativeLibs).imports,
        ["apk"]
    );
}

#[test]
fn no_native_libs_keeps_an_xapks_splits() {
    store();
    import("xapk__base", ArtifactLanguage::Apk, &[]);
    import(
        "xapk__config.arm64_v8a",
        ArtifactLanguage::Apk,
        &["xapk__config.arm64_v8a__libx"],
    );
    import("xapk__config.arm64_v8a__libx", ArtifactLanguage::Pcode, &[]);
    // A bundle's list is flat: the splits *and* what they produced.
    import(
        "xapk",
        ArtifactLanguage::Xapk,
        &[
            "xapk__base",
            "xapk__config.arm64_v8a",
            "xapk__config.arm64_v8a__libx",
        ],
    );

    assert_eq!(
        AnalysisProject::ephemeral("p", &["xapk"], SubImports::NoNativeLibs).imports,
        ["xapk", "xapk__base", "xapk__config.arm64_v8a"]
    );
}

/// Naming a library on the command line is a request for it, whatever it is a sub-import of.
#[test]
fn no_native_libs_keeps_a_named_library() {
    store();
    import("named__x86__liba", ArtifactLanguage::Pcode, &[]);
    import("named__x86__libb", ArtifactLanguage::Pcode, &[]);
    import(
        "named",
        ArtifactLanguage::Apk,
        &["named__x86__liba", "named__x86__libb"],
    );

    assert_eq!(
        AnalysisProject::ephemeral(
            "p",
            &["named", "named__x86__libb"],
            SubImports::NoNativeLibs
        )
        .imports,
        ["named", "named__x86__libb"]
    );
    // And on its own, too.
    assert_eq!(
        AnalysisProject::ephemeral("p", &["named__x86__liba"], SubImports::NoNativeLibs).imports,
        ["named__x86__liba"]
    );
}

/// A sub-import with no loadable config passes through, as it does without the filter:
/// `cli::index` reports a missing import properly.
#[test]
fn no_native_libs_passes_through_an_unloadable_sub_import() {
    store();
    import("dangling", ArtifactLanguage::Apk, &["dangling__missing"]);
    assert_eq!(
        AnalysisProject::ephemeral("p", &["dangling"], SubImports::NoNativeLibs).imports,
        ["dangling", "dangling__missing"]
    );
}
