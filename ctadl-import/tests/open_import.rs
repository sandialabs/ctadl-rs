/*! Tests the reading side from end to end, with no front end in the process.

That is the point of these tests, and of the crate. Everything below runs against a store that
`ctadl-import` alone wrote and read, with no parser and no engine. That is all another program
needs to depend on in order to read CTADL imports.
*/

use std::sync::Once;

use ctadl_import::project::{
    ArtifactImport, ArtifactLanguage, IMPORT_FORMAT_VERSION, init_store_path,
};
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

/// Rewrites the `version` field of `import`'s config, leaving everything else alone. This is
/// what a store written by an older build looks like from here: the files are all present and
/// the bitcode is readable, only the format they were written in is not the one this build
/// expects.
fn set_config_version(import: &ArtifactImport, version: &str) {
    let path = import.config_path();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&text).unwrap();
    config["version"] = serde_json::Value::String(version.to_string());
    std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
}

/// The whole message a user sees, which is the error joined to its causes. `Error::Context`
/// prints only its own context line, so the diagnostic that matters is one link down; `main`
/// returns `anyhow::Result`, which walks the chain the same way this does.
fn full_message(err: &ctadl_import::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

/// Walks the cause chain for the [`Error::IncompatibleImport`] the version check raises. It is
/// not the outermost error: `resolve_import` wraps it in an `Error::Context` saying which import
/// it was reading.
fn incompatible_cause(err: &ctadl_import::Error) -> (String, String, String) {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(ctadl_import::Error::IncompatibleImport {
            name,
            found,
            expected,
            ..
        }) = e.downcast_ref::<ctadl_import::Error>()
        {
            return (name.clone(), found.clone(), expected.clone());
        }
        current = e.source();
    }
    panic!("no IncompatibleImport in the cause chain of: {err:?}");
}

/// A store written by an older build is refused, and the error says which artifact to import
/// again.
///
/// This is the regression the reader exists for. Reading `ir-program.bitcode` directly -- which
/// is what a downstream crate had to do before [`open_import`] was public -- cannot make this
/// check, so a stale store either decodes into something wrong or fails with a `bitcode::Error`
/// that names no cause. Here the bitcode is perfectly readable and the version is the only thing
/// wrong, which is the point: the check fires on the config, before the decode.
#[test]
fn an_import_from_an_older_build_is_refused_by_name() {
    store();
    let dir = tempfile::tempdir().unwrap();
    let import = write_import("stale_by_name", dir.path());
    set_config_version(&import, "5");

    let err = open_import("stale_by_name", ssa::Pipeline::index_default()).unwrap_err();
    let (name, found, expected) = incompatible_cause(&err);
    assert_eq!(name, "stale_by_name");
    assert_eq!(found, "5");
    assert_eq!(expected, IMPORT_FORMAT_VERSION);

    // The user's next move is to import that artifact again, so the message has to name it.
    let message = full_message(&err);
    assert!(
        message.contains(&import.artifact_path.display().to_string()),
        "{message}"
    );
    assert!(message.contains("re-import it"), "{message}");
}

/// The same, opened by directory rather than by name. Both spellings resolve through
/// [`ArtifactImport::load`], and neither goes around it -- which is what makes the check
/// unavoidable rather than merely available.
#[test]
fn an_import_from_an_older_build_is_refused_by_directory() {
    store();
    let dir = tempfile::tempdir().unwrap();
    let import = write_import("stale_by_dir", dir.path());
    set_config_version(&import, "5");

    let path = import.import_path();
    let err = open_import(path.to_str().unwrap(), ssa::Pipeline::none()).unwrap_err();
    let (_, found, _) = incompatible_cause(&err);
    assert_eq!(found, "5");
    assert!(full_message(&err).contains("import format 5"), "{err:?}");
}
