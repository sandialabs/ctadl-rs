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
    // Not sure if .. is allowed but seems to work
    [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "dex-reader",
        "dex-files",
        "com.noto_54.apk",
    ]
    .iter()
    .collect()
}

/// Wrap the body of your store tests in this. See the note at the top of the file.
fn run_store_test<F>(test: F) -> ()
where
    F: FnOnce() -> () + std::panic::UnwindSafe,
{
    initialize();
    let result = std::panic::catch_unwind(|| test());
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
        let data = std::fs::read(&import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
        assert!(ArtifactImport::load_by_name("test_import").is_ok());
    });
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
