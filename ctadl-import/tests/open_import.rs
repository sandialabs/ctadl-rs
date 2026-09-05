/*! Tests the reading side from end to end, with no front end in the process.

That is the point of these tests, and of the crate. Everything below runs against a store that
`ctadl-import` alone wrote and read, with no parser and no engine. That is all another program
needs to depend on in order to read CTADL imports.
*/

use std::sync::Once;

use ctadl_import::project::{ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_import::{SourceInfoMode, load_import, open_import, save_program_info};
use ctadl_ir::ProgramInfo;
use ctadl_ir::ssa;

/// The store root belongs to the whole process and can be set only once, so every test in this
/// binary shares a single store.
static INIT: Once = Once::new();

fn store() {
    INIT.call_once(|| {
        let dir = tempfile::tempdir().unwrap();
        init_store_path(Some(dir)).unwrap();
    });
}

/// Writes an empty import called `name`. Its artifact is a file in `dir`.
fn write_import(name: &str, dir: &std::path::Path) -> ArtifactImport {
    let artifact = dir.join(format!("{name}.dex"));
    std::fs::write(&artifact, b"not really a dex").unwrap();
    let import = ArtifactImport::try_create(name, ArtifactLanguage::Dex, &artifact).unwrap();
    save_program_info(ProgramInfo::default(), &import).unwrap();
    import
}

/// Calls `save_program_info` and then `open_import`. This is the round trip another program
/// gets without having to know the store layout or the bitcode filenames.
#[test]
fn a_saved_import_opens_by_name() {
    store();
    let dir = tempfile::tempdir().unwrap();
    write_import("round_trip", dir.path());

    let opened = open_import("round_trip", ssa::Pipeline::index_default()).unwrap();
    assert!(opened.program.functions.is_empty());
    // `open_import` does not read source info. A caller who wants it calls `load_import`.
    assert_eq!(opened.source_info.spans.len(), 0);
}

/// Opens the same import by its directory instead of by its name. This is what lets a program
/// read a store it did not create.
#[test]
fn a_saved_import_opens_by_directory() {
    store();
    let dir = tempfile::tempdir().unwrap();
    let import = write_import("by_dir", dir.path());

    let path = import.import_path();
    let opened = open_import(path.to_str().unwrap(), ssa::Pipeline::none()).unwrap();
    assert!(opened.program.functions.is_empty());
}

/// A name that is neither a directory nor an import in this store fails with an error that
/// says where it looked. Naming the store root is usually enough, because the real problem is
/// almost always a wrong `--store`.
#[test]
fn an_unknown_name_names_the_store_it_searched() {
    store();
    let err = open_import("no_such_import", ssa::Pipeline::none()).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("no_such_import"), "{message}");
    assert!(message.contains("imports"), "{message}");
}

/// [`load_import`] returns the IR as the front end wrote it. [`open_import`] does the same and
/// then runs the pipeline. On an empty program the two have to agree, which is a cheap way to
/// check that `open_import` does nothing else along the way.
#[test]
fn load_and_open_agree_before_any_pass_has_work_to_do() {
    store();
    let dir = tempfile::tempdir().unwrap();
    let import = write_import("agree", dir.path());

    let loaded = load_import(&import, SourceInfoMode::Skip).unwrap();
    let opened = open_import("agree", ssa::Pipeline::index_default()).unwrap();
    assert_eq!(
        loaded.program.functions.len(),
        opened.program.functions.len()
    );
}
