/*!
The store is relocatable: copy the directory somewhere else, point `--store` at the copy, and
everything in it still resolves. That holds only as long as no config *inside* the store records
where the store lives, which is what these tests check.

Like `cli.rs`, this binary can set the store root only once per process, so both tests share the
one root and use distinct import and project names.
*/
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use ctadl_ascent::project::*;
use tempfile::{TempDir, tempdir};

static INIT: Once = Once::new();

/// Points the store at a temp directory, once, and returns the root as the store itself
/// resolves it.
fn store_root() -> PathBuf {
    INIT.call_once(|| {
        let dir = tempdir().unwrap();
        init_store_path(Some(dir)).unwrap();
    });
    StorePaths::root().to_path_buf()
}

/// A file to stand in for an imported artifact. Its contents never get parsed here: these tests
/// exercise the config, not a language frontend. Returns the [`TempDir`] too, so the caller
/// keeps it alive.
fn fake_artifact(name: &str) -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let path = dir.path().join(name);
    fs::write(&path, b"artifact contents").unwrap();
    (dir, path)
}

/// Copies the directory tree at `src` to `dst`, the way someone relocating a store would.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

#[test]
fn configs_are_store_relative_and_survive_a_copy() {
    let root = store_root();
    let (_artifact_dir, artifact) = fake_artifact("app.jar");

    let import = ArtifactImport::try_create("copy_app", ArtifactLanguage::Jar, &artifact).unwrap();
    fs::write(import.program_path(), b"program").unwrap();
    let project = AnalysisProject::try_create("copy_proj", &["copy_app"]).unwrap();

    // What the config records is relative to the root...
    assert_eq!(import.import_dir, Path::new("imports").join("copy_app"));
    assert_eq!(project.project_dir, Path::new("projects").join("copy_proj"));
    // ...and what callers use is that, resolved against the root in force.
    assert_eq!(import.import_path(), root.join("imports").join("copy_app"));
    assert_eq!(project.dir(), root.join("projects").join("copy_proj"));

    // The artifact stays absolute: it names something outside the store, which does not move
    // when the store does.
    assert_eq!(import.artifact_path, fs::canonicalize(&artifact).unwrap());
    assert!(import.artifact_path.is_absolute());

    // Neither config names the root, which is precisely what makes the copy below work.
    let root_text = root.to_string_lossy().to_string();
    for config in [import.config_path(), project.config_path()] {
        let text = fs::read_to_string(&config).unwrap();
        assert!(
            !text.contains(&root_text),
            "{} records the store root: {}",
            config.display(),
            text
        );
    }

    // Copy the store elsewhere and resolve the copied configs against their new root.
    let elsewhere = tempdir().unwrap();
    let moved = elsewhere.path().join("ctadl");
    copy_dir(&root, &moved);

    let text = fs::read_to_string(moved.join("imports/copy_app").join(IMPORT_CONFIG_FILE)).unwrap();
    let copied: ArtifactImport = serde_json::from_str(&text).unwrap();
    assert!(
        moved
            .join(&copied.import_dir)
            .join(PROGRAM_BITCODE_FILE)
            .is_file()
    );

    let text =
        fs::read_to_string(moved.join("projects/copy_proj").join(PROJECT_CONFIG_FILE)).unwrap();
    let copied: AnalysisProject = serde_json::from_str(&text).unwrap();
    assert!(moved.join(&copied.project_dir).is_dir());
}

/// Configs written before the paths went relative name a directory in the store that wrote them,
/// so a copy of such a store would resolve back to the original location. Reading one rewrites
/// the path from the name, which the fixed layout determines.
#[test]
fn a_legacy_absolute_config_reads_as_store_relative() {
    let root = store_root();
    let (_artifact_dir, artifact) = fake_artifact("legacy.jar");
    let artifact = fs::canonicalize(&artifact).unwrap();

    let import_dir = root.join("imports").join("legacy_app");
    fs::create_dir_all(&import_dir).unwrap();
    fs::write(
        import_dir.join(IMPORT_CONFIG_FILE),
        serde_json::json!({
            "name": "legacy_app",
            "language": "Jar",
            "artifact_path": artifact,
            // The store this was written in, which is not the one reading it.
            "import_path": "/somewhere/else/imports/legacy_app",
            "version": IMPORT_FORMAT_VERSION,
        })
        .to_string(),
    )
    .unwrap();

    let import = ArtifactImport::load_by_name("legacy_app").unwrap();
    assert_eq!(import.import_dir, Path::new("imports").join("legacy_app"));
    assert_eq!(import.import_path(), import_dir);
    // The artifact path is left alone.
    assert_eq!(import.artifact_path, artifact);

    let project_dir = root.join("projects").join("legacy_proj");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join(PROJECT_CONFIG_FILE),
        serde_json::json!({
            "name": "legacy_proj",
            "dir": "/somewhere/else/projects/legacy_proj",
            "imports": ["legacy_app"],
        })
        .to_string(),
    )
    .unwrap();

    let project = AnalysisProject::try_load_name("legacy_proj").unwrap();
    assert_eq!(
        project.project_dir,
        Path::new("projects").join("legacy_proj")
    );
    assert_eq!(project.dir(), project_dir);
}
