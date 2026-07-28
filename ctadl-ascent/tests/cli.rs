/*!
NOTE: Tests in this file have a special structure.

We have to be careful to set up a temporary store path for the tests. This should be done only once
per process, so we do it in `initialize`. This sets up the store to point to a temp directory. To
ensure this happens for your store tests, wrap the test body in [`run_store_test`].

Also, tests need to be sure their artifact import and project names are distinct. This needs to be
done manually.

*/
use std::path::PathBuf;
use std::sync::Once;
use tempfile::tempdir;

use ctadl_ascent::cli;
use ctadl_ascent::project::*;

static INIT: Once = Once::new();

pub fn initialize() {
    INIT.call_once(|| {
        let dir = tempdir().unwrap();
        init_store_path(Some(dir)).unwrap();
    });
}

fn test_file() -> PathBuf {
    // The real-world APK fixture is owned by the xtask regression harness (see
    // xtask/tests/dex/). Not sure if .. is allowed but seems to work.
    [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "xtask",
        "tests",
        "dex",
        "com.noto_54.apk",
    ]
    .iter()
    .collect()
}

/// Wrap the body of your store tests in this. See the note at the top of the file.
fn run_store_test<F>(test: F)
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    initialize();
    let result = std::panic::catch_unwind(test);
    assert!(result.is_ok())
}

// TODO fix these by finding some small dex files to test on

#[test]
fn test_cli_import() {
    run_store_test(|| {
        let result = ArtifactImport::try_create("test_import", ArtifactLanguage::Apk, &test_file());
        assert!(result.is_ok());
        let import = result.unwrap();
        let result = cli::import(&import);
        assert!(result.is_ok());

        assert!(import.name == "test_import");
        assert!(import.program_path().is_file());
        assert!(import.config_path().is_file());
        let data = std::fs::read(import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
        assert!(ArtifactImport::load_by_name("test_import").is_ok());
    });
}

#[test]
fn test_cli_import_skip_existing() {
    run_store_test(|| {
        let name = "test_import_skip";
        // Before any import exists, nothing is up to date.
        assert!(!ArtifactImport::is_up_to_date(name, &test_file()).unwrap());

        let import = ArtifactImport::try_create(name, ArtifactLanguage::Apk, &test_file()).unwrap();
        // Destination not yet written and no hash recorded: still not up to date.
        assert!(!ArtifactImport::is_up_to_date(name, &test_file()).unwrap());

        cli::import(&import).unwrap();
        // Destination exists, but the hash has not been recorded yet.
        assert!(!ArtifactImport::is_up_to_date(name, &test_file()).unwrap());

        // Recording the hash (as the import command does on success) makes the
        // import up to date so a `--skip-existing` re-import is skipped.
        let mut import = ArtifactImport::load_by_name(name).unwrap();
        import.record_artifact_hash().unwrap();
        assert!(import.hash.is_some());
        assert!(ArtifactImport::is_up_to_date(name, &test_file()).unwrap());

        // A reloaded config still reflects the recorded hash and path.
        let reloaded = ArtifactImport::load_by_name(name).unwrap();
        assert_eq!(reloaded.hash, import.hash);
        assert!(ArtifactImport::is_up_to_date(name, &test_file()).unwrap());
    });
}

#[test]
fn test_hash_artifact_file_and_dir() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // A single file hashes deterministically and is sensitive to content.
    let file = root.join("a.bin");
    std::fs::write(&file, b"hello").unwrap();
    let h1 = hash_artifact(&file).unwrap();
    assert_eq!(h1, hash_artifact(&file).unwrap());
    std::fs::write(&file, b"hello!").unwrap();
    assert_ne!(h1, hash_artifact(&file).unwrap());

    // A directory hashes over its files deterministically, independent of
    // creation order, and changes when a file changes.
    let sub = root.join("tree");
    std::fs::create_dir_all(sub.join("nested")).unwrap();
    std::fs::write(sub.join("nested").join("y.txt"), b"world").unwrap();
    std::fs::write(sub.join("x.txt"), b"foo").unwrap();
    let d1 = hash_artifact(&sub).unwrap();
    assert_eq!(d1, hash_artifact(&sub).unwrap());
    std::fs::write(sub.join("x.txt"), b"bar").unwrap();
    assert_ne!(d1, hash_artifact(&sub).unwrap());
}

//#[test]
//fn test_cli_index() {
//    env_logger::init();
//    run_store_test(|| {
//        let result =
//            ArtifactImport::try_create("test_index_artifact", ArtifactLanguage::Dex, &test_file());
//        assert!(result.is_ok());
//        let import = result.unwrap();
//        let result = cli::import(&import);
//        assert!(result.is_ok());
//        //let import = result.unwrap();

//        let result = AnalysisProject::try_create("test_index_project", &["test_index_artifact"]);
//        assert!(result.is_ok());
//        let project = result.unwrap();
//        let result = cli::index(&project);
//        assert!(result.is_ok());

//        assert!(project.name == "test_index_project");
//        assert_eq!(project.imports, &["test_index_artifact"]);
//        assert!(project.dir.is_dir());
//        assert!(project.index_path().is_ok());
//        assert!(project.index_path().unwrap().is_dir());
//        assert!(project.config_path().is_file());

//        // Check that there are some files in the index dir
//        let result = std::fs::read_dir(&project.index_path().unwrap());
//        assert!(result.is_ok());
//        let contents: Vec<_> = result.unwrap().into_iter().collect();
//        assert!(contents.len() > 1);
//    });
//}

// ---------------------------------------------------------------------------
// The index format-version gate.
//
// `index` and `query` are separate processes and every access path crosses the
// parquet boundary between them. The decoders are infallible-by-construction for
// anything this build wrote and panic on anything else, so this gate is what turns
// a stale `index/` into an actionable "re-run `ctadl index`" instead of a panic --
// or, before the encoding was fixed, into silently-wrong analysis results.
// ---------------------------------------------------------------------------

#[test]
fn index_version_gate_accepts_what_this_build_wrote() {
    run_store_test(|| {
        let project = AnalysisProject::try_create("gate_ok", &["nonexistent_import"]).unwrap();
        project.write_index_config().unwrap();
        assert!(
            project.check_index_config().is_ok(),
            "an index this build just stamped must be readable"
        );
    });
}

#[test]
fn index_version_gate_rejects_an_index_from_before_the_gate() {
    run_store_test(|| {
        let project = AnalysisProject::try_create("gate_missing", &["nonexistent_import"]).unwrap();
        // An `index/` with no config is one written before the gate existed -- exactly the
        // stale-encoding case, since those builds wrote unescaped `.[]` / `.[_elem_]`.
        std::fs::create_dir_all(project.index_path().unwrap()).unwrap();
        match project.check_index_config() {
            Err(ctadl_ascent::error::Error::IncompatibleIndex {
                project: p,
                expected,
                ..
            }) => {
                assert_eq!(p, "gate_missing");
                assert_eq!(expected, INDEX_FORMAT_VERSION);
            }
            other => panic!("expected IncompatibleIndex, got: {other:?}"),
        }
    });
}

#[test]
fn index_version_gate_rejects_a_different_version() {
    run_store_test(|| {
        let project = AnalysisProject::try_create("gate_stale", &["nonexistent_import"]).unwrap();
        let path = project.index_path().unwrap().join(INDEX_CONFIG_FILE);
        std::fs::write(&path, r#"{"version":"1"}"#).unwrap();
        match project.check_index_config() {
            Err(ctadl_ascent::error::Error::IncompatibleIndex { found, .. }) => {
                assert_eq!(found, "1");
            }
            other => panic!("expected IncompatibleIndex, got: {other:?}"),
        }
        // The message must name the fix -- it is the whole point of the variant.
        let msg = project.check_index_config().unwrap_err().to_string();
        assert!(
            msg.contains("ctadl index gate_stale"),
            "message must name the command to run: {msg}"
        );
    });
}
