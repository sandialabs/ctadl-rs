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

/// Importing a single `.c` file parses it into an IR program and stores it.
#[test]
fn test_cli_import_c_file() {
    run_store_test(|| {
        let dir = tempdir().unwrap();
        let file = dir.path().join("xfer.c");
        std::fs::write(
            &file,
            "int source();\nvoid sink(int);\nint transfer(int a) { return a; }\n",
        )
        .unwrap();

        let import =
            ArtifactImport::try_create("test_import_c_file", ArtifactLanguage::C, &file).unwrap();
        cli::import(&import).unwrap();

        assert!(import.program_path().is_file());
        let data = std::fs::read(import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
    });
}

/// Importing a directory of C sources and headers parses every `.c`/`.h` file
/// underneath it as one translation unit.
#[test]
fn test_cli_import_c_directory() {
    run_store_test(|| {
        let dir = tempdir().unwrap();
        let root = dir.path().join("c_sources");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        // A header (declarations) and two .c files, one nested, that reference it.
        std::fs::write(root.join("util.h"), "int helper(int z);\n").unwrap();
        std::fs::write(
            root.join("main.c"),
            "int helper(int z) { return z; }\nint main() { return helper(1); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("nested").join("more.c"),
            "int other(int a) { return a; }\n",
        )
        .unwrap();
        // A non-C file that must be ignored by the importer.
        std::fs::write(root.join("README.md"), "not C\n").unwrap();

        let import =
            ArtifactImport::try_create("test_import_c_dir", ArtifactLanguage::C, &root).unwrap();
        cli::import(&import).unwrap();

        assert!(import.program_path().is_file());
        let data = std::fs::read(import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
    });
}

/// Absolute path to a checked-in C test fixture under `tests/c/`.
fn c_fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "c", name]
        .iter()
        .collect()
}

/// End-to-end: import `xfer.c`, index it, and run the `xfer.json` taint query. This
/// exercises the C-specific model wiring: `source`/`sink` are only *declared* in the C
/// source (no body), so the importer must register them as external functions for the
/// model's `signature` patterns to match them; the query must then find the
/// source -> sink flow through `transfer`. Also confirms imported C carries source
/// locations: the reported result resolves to a line in `xfer.c`.
#[test]
fn test_cli_query_c_sources_and_sinks() {
    use ctadl_ascent::cli;
    use ctadl_ascent::codegen::CallResolutionStrategy;
    use ctadl_ascent::query_engine::formatter::SarifProfile;

    run_store_test(|| {
        let import = ArtifactImport::try_create(
            "test_xfer_c",
            ArtifactLanguage::C,
            &c_fixture("xfer.c"),
        )
        .unwrap();
        cli::import(&import).unwrap();

        let project = AnalysisProject::try_create("test_xfer_c_proj", &["test_xfer_c"]).unwrap();
        let models = vec![c_fixture("xfer.json")];
        cli::index(
            &project,
            &[],
            &models,
            CallResolutionStrategy::default(),
            true,
            true,
            None,
        )
        .unwrap();

        let out_dir = tempdir().unwrap();
        let sarif = out_dir.path().join("out.sarif");
        cli::query(
            &project,
            &models,
            false,
            &sarif,
            SarifProfile::default(),
            None,
        )
        .unwrap();

        let text = std::fs::read_to_string(&sarif).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let results = doc["runs"][0]["results"].as_array().unwrap();

        // The source (`s = source()`) flows through `transfer` to the sink (`sink(x[2])`),
        // so there is exactly one tainted-path result.
        assert_eq!(
            results.len(),
            1,
            "expected exactly one source->sink flow, got: {text}"
        );
        let result = &results[0];
        assert!(
            result["ruleId"]
                .as_str()
                .is_some_and(|r| r.contains("tainted-path")),
            "unexpected ruleId: {}",
            result["ruleId"]
        );

        // The reported location resolves back to a line in the C source, proving the
        // importer attached source-info spans that survive to SARIF.
        let region = &result["locations"][0]["physicalLocation"]["region"];
        assert!(
            region["startLine"].as_u64().is_some_and(|n| n > 0),
            "result has no source line: {result}"
        );
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
