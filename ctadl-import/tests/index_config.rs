/*! Tests what `write_index_config` records about the imports an index was built from. */

use ctadl_import::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};

/// One test in this binary, so the store is set once.
#[test]
fn write_index_config_records_each_imports_hash() {
    let store = tempfile::tempdir().unwrap();
    init_store_path(Some(store.path())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("libx.so");
    std::fs::write(&artifact, b"\x7fELF").unwrap();

    let mut hashed =
        ArtifactImport::try_create("hashed", ArtifactLanguage::Pcode, &artifact).unwrap();
    hashed.record_artifact_hash().unwrap();
    // Never finished, so it never recorded a hash.
    ArtifactImport::try_create("unhashed", ArtifactLanguage::Pcode, &artifact).unwrap();

    let project =
        AnalysisProject::try_create("hashes", &["hashed", "unhashed"], SubImports::All).unwrap();
    project.write_index_config(None).unwrap();

    let config = project.index_config().expect("index config");
    assert_eq!(
        config.import_hashes.into_iter().collect::<Vec<_>>(),
        [("hashed".to_string(), hashed.hash.unwrap())]
    );
}
