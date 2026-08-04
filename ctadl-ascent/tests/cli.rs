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
        let result = cli::import(&import, cli::ImportOptions::default());
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

        cli::import(&import, cli::ImportOptions::default()).unwrap();
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

/// The fixture APK ships no `lib/<abi>` entries, so the native-library pass is a no-op
/// and the import records no sub-imports. This is the path every APK without native
/// code takes, and the one that must not need Ghidra.
#[test]
fn test_cli_import_apk_without_native_libs() {
    run_store_test(|| {
        let name = "test_import_no_native";
        let import = ArtifactImport::try_create(name, ArtifactLanguage::Apk, &test_file()).unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();

        let reloaded = ArtifactImport::load_by_name(name).unwrap();
        assert!(
            reloaded.sub_imports.is_empty(),
            "an APK with no native libraries records no sub-imports, got {:?}",
            reloaded.sub_imports
        );
        // Nothing was extracted, so the staging directory was never created.
        assert!(!import.import_path().join("native").exists());
    });
}

/// Writes an APK built from `(entry name, contents)` pairs into `dir`, and returns its
/// path. Enough of an APK for the import path: a ZIP whose entry names are what the Dex
/// and native-library passes look for.
fn write_apk(dir: &std::path::Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    use std::io::Write;
    let path = dir.join(name);
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (entry, contents) in entries {
        writer.start_file(*entry, options).unwrap();
        writer.write_all(contents).unwrap();
    }
    writer.finish().unwrap();
    path
}

/// A split APK out of an app bundle -- `config.arm64_v8a.apk` inside an XAPK -- carries
/// native libraries and no `classes*.dex` at all. It imports: the Java half is simply
/// empty, and the libraries are what the import is for.
///
/// `native_libs: false` keeps this test off Ghidra, which the native half needs and
/// which no unit-test worker is guaranteed to have. What is under test here is that a
/// Dex-less APK is accepted at all -- before this, it failed outright on "APK contains
/// no classes*.dex entries" and the libraries went with it.
#[test]
fn test_cli_import_native_only_split_apk() {
    run_store_test(|| {
        let dir = tempdir().unwrap();
        let apk = write_apk(
            dir.path(),
            "config.arm64_v8a.apk",
            &[
                ("AndroidManifest.xml", b"\x03\x00\x08\x00"),
                ("lib/arm64-v8a/libfoo.so", b"\x7fELFstub"),
            ],
        );

        let name = "test_import_native_only";
        let import = ArtifactImport::try_create(name, ArtifactLanguage::Apk, &apk).unwrap();
        cli::import(
            &import,
            cli::ImportOptions {
                native_libs: false,
                ..Default::default()
            },
        )
        .unwrap();

        // The parent import is real and its (empty) Java program round-trips.
        let data = std::fs::read(import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
        assert!(ArtifactImport::load_by_name(name).is_ok());
    });
}

/// The other splits of the same bundle hold only resources -- no Dex, no `lib/<abi>/`.
/// Importing one can only produce an empty program that indexes to nothing, so it is
/// rejected with a message that says where the code actually is.
#[test]
fn test_cli_import_resource_only_split_apk_is_rejected() {
    run_store_test(|| {
        let dir = tempdir().unwrap();
        let apk = write_apk(
            dir.path(),
            "config.en.apk",
            &[
                ("AndroidManifest.xml", b"\x03\x00\x08\x00"),
                ("res/values/strings.xml", b"<resources/>"),
            ],
        );

        let import =
            ArtifactImport::try_create("test_import_res_only", ArtifactLanguage::Apk, &apk)
                .unwrap();
        let err = cli::import(&import, cli::ImportOptions::default()).unwrap_err();
        assert!(
            matches!(err, ctadl_ascent::error::Error::NothingToImport { .. }),
            "expected NothingToImport, got {err:?}"
        );
        // The message has to name both halves it looked for; that is what tells the user
        // this APK is a split rather than a broken one.
        let message = err.to_string();
        assert!(message.contains("classes*.dex"), "{message}");
        assert!(message.contains("lib/<abi>/"), "{message}");
    });
}

/// Naming an import in a project also co-indexes whatever was imported out of it --
/// this is what makes `ctadl import app.apk && ctadl index p app` see the APK's native
/// libraries without the user naming them.
#[test]
fn test_project_expands_sub_imports() {
    run_store_test(|| {
        let dir = tempdir().unwrap();
        let artifact = dir.path().join("libfoo.so");
        std::fs::write(&artifact, b"\x7fELF").unwrap();

        for name in ["expand_child_a", "expand_child_b"] {
            ArtifactImport::try_create(name, ArtifactLanguage::Pcode, &artifact).unwrap();
        }
        let mut parent =
            ArtifactImport::try_create("expand_parent", ArtifactLanguage::Apk, &artifact).unwrap();
        parent.sub_imports = vec!["expand_child_a".into(), "expand_child_b".into()];
        parent.save().unwrap();

        let project = AnalysisProject::try_create("expand_proj", &["expand_parent"]).unwrap();
        // Parent first, then its sub-imports in order.
        assert_eq!(
            project.imports,
            ["expand_parent", "expand_child_a", "expand_child_b"]
        );

        // Naming a sub-import explicitly alongside its parent does not index it twice.
        let project =
            AnalysisProject::try_create("expand_proj_dedup", &["expand_parent", "expand_child_b"])
                .unwrap();
        assert_eq!(
            project.imports,
            ["expand_parent", "expand_child_a", "expand_child_b"]
        );
    });
}

/// A project may name an import that does not exist yet; `index` has its own preflight
/// gates that report that properly, so expansion must not turn it into an error here.
#[test]
fn test_project_expansion_tolerates_a_missing_import() {
    run_store_test(|| {
        let project = AnalysisProject::try_create("expand_missing", &["no_such_import"]).unwrap();
        assert_eq!(project.imports, ["no_such_import"]);
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
//        let result = cli::import(&import, cli::ImportOptions::default());
//        assert!(result.is_ok());
//        //let import = result.unwrap();

//        let result = AnalysisProject::try_create("test_index_project", &["test_index_artifact"]);
//        assert!(result.is_ok());
//        let project = result.unwrap();
//        let result = cli::index(&project);
//        assert!(result.is_ok());

//        assert!(project.name == "test_index_project");
//        assert_eq!(project.imports, &["test_index_artifact"]);
//        assert!(project.dir().is_dir());
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

// ---------------------------------------------------------------------------
// `query` with no index. The rest of the check is covered by `tests/model_check.rs`, which
// drives `check_programs` with no store at all; what is store-specific is name resolution --
// and the promise that a query that cannot run writes nothing into the store.
// ---------------------------------------------------------------------------

#[test]
fn query_without_an_index_checks_the_models_and_writes_nothing() {
    run_store_test(|| {
        let name = "test_model_check";
        let import = ArtifactImport::try_create(name, ArtifactLanguage::Apk, &test_file()).unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();
        // Reloaded: `cli::import` records the APK's native sub-imports into the config.
        let import = ArtifactImport::load_by_name(name).unwrap();

        let mut models = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        {
            use std::io::Write as _;
            write!(
                models,
                r#"{{"model_generators": [
                    {{"find": "methods",
                      "where": [{{"constraint": "signature_match", "name": "toString"}}],
                      "model": {{"sources": [{{"kind": "k", "port": "Return"}}]}}}}
                ]}}"#
            )
            .unwrap();
            models.flush().unwrap();
        }

        // What `ctadl query <an-import-that-was-never-indexed>` builds: the import list, with
        // no project written to the store.
        let project = AnalysisProject::ephemeral(name, &[name]);
        let outcome = cli::check_models(&project, &[models.path().to_path_buf()]).unwrap();

        // Naming the import names everything imported out of it: the APK plus its native
        // libraries, the same expansion `AnalysisProject::try_create` does.
        let checked: Vec<&str> = outcome
            .check
            .imports
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let mut expected = vec![name.to_string()];
        expected.extend(import.sub_imports.iter().cloned());
        assert_eq!(
            checked,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(!outcome.has_file_errors());
        assert!(outcome.check.matched[0].total.unwrap() > 0);

        // A query that only checked model files must leave the store as it found it.
        let project_dir = StorePaths::projects_path().join(name);
        assert!(
            !project_dir.exists(),
            "the model check wrote a project config: {}",
            project_dir.display()
        );
    });
}
