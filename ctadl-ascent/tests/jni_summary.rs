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
