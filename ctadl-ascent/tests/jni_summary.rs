//! How `ctadl index --summary` feeds the JNI bridge, at the store level.
//!
//! The end-to-end JNI case, with real Dex and ELF inputs, is the `+summary` packaging in
//! `cargo xtask regression --frontend jni`. This file checks what can be checked with Lua alone,
//! which needs no toolchain.

use ctadl_ascent::cli;
use ctadl_ascent::project::{
    AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports, init_store_path,
};
use std::path::Path;
use std::sync::Once;

static INIT: Once = Once::new();

fn store_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp store"));
    INIT.call_once(|| {
        init_store_path(Some(dir.path())).expect("init store");
    });
    dir.path()
}

fn import_lua(name: &str, text: &str) -> ArtifactImport {
    let src = store_dir().join(format!("{name}.lua"));
    std::fs::write(&src, text).expect("writing lua source");
    let import =
        ArtifactImport::try_create(name, ArtifactLanguage::Lua, &src).expect("import args");
    cli::import(&import, cli::ImportOptions::default()).expect("importing lua");
    import
}

fn full_message(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

/// The bridge reads a summary project's imports -- their symbol tables only -- and
/// `--no-jni-bridge` turns that off along with the rest of the bridge. With the summary
/// project's import made unreadable after it was indexed, the first index fails reading its VMT
/// and the second never looks.
#[test]
fn no_jni_bridge_skips_reading_summary_imports() {
    let lib = import_lua(
        "nobridge_lib",
        "local function f(x)\n  return x\nend\nreturn f\n",
    );
    let summary = AnalysisProject::try_create("nobridge_sp", &["nobridge_lib"], SubImports::All)
        .expect("summary project");
    cli::index(&summary, &[], &[], false, cli::IndexOptions::default()).expect("indexing sp");
    std::fs::remove_file(lib.vmt_path()).expect("removing the library's vmt");

    import_lua("nobridge_app", "print(io.read())\n");
    let app = AnalysisProject::try_create("nobridge_app", &["nobridge_app"], SubImports::All)
        .expect("app project");
    let summaries = ["nobridge_sp".to_string()];

    let err = cli::index(&app, &summaries, &[], false, cli::IndexOptions::default())
        .expect_err("the bridge reads the summary project's imports");
    let message = full_message(&err);
    assert!(message.contains("vmt"), "{message}");

    cli::index(
        &app,
        &summaries,
        &[],
        false,
        cli::IndexOptions {
            no_jni_bridge: true,
            ..Default::default()
        },
    )
    .expect("--no-jni-bridge does not read the summary project's imports");
}

/// An import config named `name` with no program behind it, which is all the provenance check
/// reads. Its artifact holds `contents`, and its hash is recorded, as a finished import's is.
fn fake_import(name: &str, language: ArtifactLanguage, subs: &[&str], contents: &str) {
    let artifact = store_dir().join(format!("{name}.artifact"));
    std::fs::write(&artifact, contents).expect("writing artifact");
    let mut import =
        ArtifactImport::try_create(name, language, &artifact).expect("creating import");
    import.sub_imports = subs.iter().map(|s| s.to_string()).collect();
    import.record_artifact_hash().expect("recording hash");
}

/// A summary project over `imports`, stamped as indexed now.
fn fake_summary_project(name: &str, imports: &[&str]) -> AnalysisProject {
    let project = AnalysisProject::try_create(name, imports, SubImports::All).expect("project");
    project.write_index_config(None).expect("index config");
    project
}

#[test]
fn provenance_is_silent_for_this_apps_library_and_flags_the_rest() {
    use cli::SummaryProvenance::*;
    fake_import("prov_app__x86__libx", ArtifactLanguage::Pcode, &[], "x v1");
    fake_import("prov_app__x86__liby", ArtifactLanguage::Pcode, &[], "y");
    fake_import(
        "prov_app",
        ArtifactLanguage::Apk,
        &["prov_app__x86__libx", "prov_app__x86__liby"],
        "apk",
    );
    fake_import(
        "prov_other__x86__libx",
        ArtifactLanguage::Pcode,
        &[],
        "other x",
    );
    // The app project the way the summary workflow builds it: its libraries filtered out.
    let app = AnalysisProject::try_create("prov_app", &["prov_app"], SubImports::NoNativeLibs)
        .expect("app project");
    assert_eq!(app.imports, ["prov_app"]);

    // This app's own library, unchanged since it was indexed: nothing to say.
    let xproj = fake_summary_project("prov_xproj", &["prov_app__x86__libx"]);
    assert_eq!(cli::check_summary_provenance(&app, &xproj), []);

    // Another app's library.
    let other = fake_summary_project("prov_otherproj", &["prov_other__x86__libx"]);
    assert_eq!(
        cli::check_summary_provenance(&app, &other),
        [NotThisApp {
            project: "prov_otherproj".into(),
            import: "prov_other__x86__libx".into()
        }]
    );

    // This app's library, re-imported from a different build after the project was indexed.
    fake_import("prov_app__x86__libx", ArtifactLanguage::Pcode, &[], "x v2");
    assert_eq!(
        cli::check_summary_provenance(&app, &xproj),
        [Stale {
            project: "prov_xproj".into(),
            import: "prov_app__x86__libx".into()
        }]
    );
}

/// An index written before import hashes were recorded cannot be checked for staleness, which
/// is said once rather than warned about.
#[test]
fn provenance_says_when_it_cannot_check_staleness() {
    fake_import("old_app__x86__libx", ArtifactLanguage::Pcode, &[], "x");
    fake_import(
        "old_app",
        ArtifactLanguage::Apk,
        &["old_app__x86__libx"],
        "apk",
    );
    let app = AnalysisProject::try_create("old_app", &["old_app"], SubImports::NoNativeLibs)
        .expect("app project");
    let xproj = AnalysisProject::try_create("old_xproj", &["old_app__x86__libx"], SubImports::All)
        .expect("summary project");
    std::fs::write(
        xproj.index_path().unwrap().join("index_config.json"),
        serde_json::json!({"version": ctadl_ascent::project::INDEX_FORMAT_VERSION}).to_string(),
    )
    .unwrap();
    assert_eq!(
        cli::check_summary_provenance(&app, &xproj),
        [cli::SummaryProvenance::NoHashes {
            project: "old_xproj".into()
        }]
    );
}
