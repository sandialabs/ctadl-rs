/*! The reader half, end to end, with no front end in the process.

That is the point of the test as much as of the crate: everything below runs against a store
written and read by `ctadl-import` alone -- no parser, no engine -- which is what a downstream
consumer's dependency on CTADL is now allowed to be.
*/

use std::sync::Once;

use ctadl_import::project::{ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_import::{SourceInfoMode, load_import, open_import, save_program_info};
use ctadl_ir::ProgramInfo;
use ctadl_ir::ssa;

/// The store root is process-wide and settable once, so every test in this binary shares one.
static INIT: Once = Once::new();

fn store() {
    INIT.call_once(|| {
        let dir = tempfile::tempdir().unwrap();
        init_store_path(Some(dir)).unwrap();
    });
}

/// Writes an empty import named `name`, whose "artifact" is a file in `dir`.
fn write_import(name: &str, dir: &std::path::Path) -> ArtifactImport {
    let artifact = dir.join(format!("{name}.dex"));
    std::fs::write(&artifact, b"not really a dex").unwrap();
    let import = ArtifactImport::try_create(name, ArtifactLanguage::Dex, &artifact).unwrap();
    save_program_info(ProgramInfo::default(), &import).unwrap();
    import
}

/// `save_program_info` then `open_import`: the round trip that every consumer used to hand-roll
/// out of the store layout and the bitcode filenames.
#[test]
fn a_saved_import_opens_by_name() {
    store();
    let dir = tempfile::tempdir().unwrap();
    write_import("round_trip", dir.path());

    let opened = open_import("round_trip", ssa::Pipeline::index_default()).unwrap();
    assert!(opened.program.functions.is_empty());
    // Source info is skipped by `open_import`; `load_import` is where a caller asks for it.
    assert_eq!(opened.source_info.spans.len(), 0);
}

/// The same import, addressed by its directory rather than its name. This is what lets a
/// consumer read a store it did not create.
#[test]
fn a_saved_import_opens_by_directory() {
    store();
    let dir = tempfile::tempdir().unwrap();
    let import = write_import("by_dir", dir.path());

    let path = import.import_path();
    let opened = open_import(path.to_str().unwrap(), ssa::Pipeline::none()).unwrap();
    assert!(opened.program.functions.is_empty());
}

/// A name that is neither a directory nor an import in this store fails saying where it looked.
/// The store root is nearly always the answer -- it is the wrong `--store`.
#[test]
fn an_unknown_name_names_the_store_it_searched() {
    store();
    let err = open_import("no_such_import", ssa::Pipeline::none()).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("no_such_import"), "{message}");
    assert!(message.contains("imports"), "{message}");
}

/// [`load_import`] hands back the IR as the front end wrote it; [`open_import`] is that plus the
/// pipeline. Both must agree on an empty program, which is the cheapest way to say that
/// `open_import` is not doing anything else on the way through.
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
