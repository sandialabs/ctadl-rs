/*!
NOTE: Tests in this file have a special structure.

We have to be careful to set up a temporary store path for the tests. This should be done only once
per process, so we do it in `initialize`. This sets up the store to point to a temp directory. To
ensure this happens for your store tests, wrap the test body in [`run_store_test`].

Also, tests need to be sure their artifact import and project names are distinct. This needs to be
done manually.

Every test kept here runs in milliseconds. Keep it that way: a case that needs a real artifact
belongs in `xtask`, and one that needs only a synthetic one belongs here.

*/
use std::path::PathBuf;
use std::process::Command;
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

/// Wrap the body of your store tests in this. See the note at the top of the file.
fn run_store_test<F>(test: F)
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    initialize();
    let result = std::panic::catch_unwind(test);
    assert!(result.is_ok())
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
        cli::import(&import, cli::ImportOptions::default()).unwrap();

        assert!(import.program_path().is_file());
        let data = std::fs::read(import.program_path()).unwrap();
        assert!(ctadl_ir::encode::decode_program(&data).is_ok());
    });
}

/// Importing a directory of C sources and headers parses every `.c`/`.h` file
/// underneath it as a translation unit of its own and lowers them into one program.
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
        cli::import(&import, cli::ImportOptions::default()).unwrap();

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
///
/// This is also the end-to-end check on element-address composition: `transfer(&x[1], s)`
/// passes the *address* `x.[1]`, `transfer` writes its parameter at `@p0.[1].deref`, and
/// `sink(x[2])` reads `x.[2].deref`. The flow exists only if those two paths compose --
/// offsets are summed where they meet -- which is why the fixture indexes two different
/// slots rather than one. The unit-test version of the same shape is
/// `address_of_element_composes_with_callee_index` in the tree-sitter frontend's `tests.rs`.
#[test]
fn test_cli_query_c_sources_and_sinks() {
    use ctadl_ascent::cli;
    use ctadl_ascent::codegen::CallResolutionStrategy;
    use ctadl_ascent::query_engine::formatter::SarifProfile;

    run_store_test(|| {
        let import =
            ArtifactImport::try_create("test_xfer_c", ArtifactLanguage::C, &c_fixture("xfer.c"))
                .unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();

        let project = AnalysisProject::try_create("test_xfer_c_proj", &["test_xfer_c"]).unwrap();
        let models = vec![c_fixture("xfer.json")];
        cli::index(
            &project,
            &[],
            &models,
            false,
            cli::IndexOptions {
                strategy: CallResolutionStrategy::default(),
                ..Default::default()
            },
        )
        .unwrap();

        let out_dir = tempdir().unwrap();
        let sarif = out_dir.path().join("out.sarif");
        cli::query(&project, &models, &sarif, SarifProfile::default(), None).unwrap();

        let text = std::fs::read_to_string(&sarif).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let results = doc["runs"][0]["results"].as_array().unwrap();

        // Every profile also emits informational `taint-source`/`taint-sink` results
        // describing which endpoints matched, and `tainted-path` itself reports a non-`fail`
        // result when the query ran but found nothing. Those are context, not findings, so
        // the flow assertions look only at the `fail` `tainted-path` results.
        let paths: Vec<_> = results
            .iter()
            .filter(|r| {
                r["ruleId"]
                    .as_str()
                    .is_some_and(|id| id.contains("tainted-path"))
                    && r["kind"].as_str() == Some("fail")
            })
            .collect();

        // The source (`s = source()`) flows through `transfer` to the sink (`sink(x[2])`),
        // so there is exactly one tainted-path result.
        assert_eq!(
            paths.len(),
            1,
            "expected exactly one source->sink flow, got: {text}"
        );
        let result = paths[0];

        // The reported location resolves back to a line in the C source, proving the
        // importer attached source-info spans that survive to SARIF.
        let region = &result["locations"][0]["physicalLocation"]["region"];
        assert!(
            region["startLine"].as_u64().is_some_and(|n| n > 0),
            "result has no source line: {result}"
        );

        // The code flow must visit the summarized interprocedural call itself, not
        // just its source and sink endpoints. `transfer` is analyzed by summary (the
        // flow links its actual-arg vertices by an intra edge rather than descending
        // into it), so its call on line 12 -- between `s = source()` on 11 and
        // `sink(x[2])` on 13 -- is on no Call/Return path edge and would be elided
        // unless the formatter surfaces the interior call-arg vertex. Assert all
        // three lines appear as code-flow steps.
        let mut step_lines = std::collections::BTreeSet::new();
        for flow in result["codeFlows"].as_array().into_iter().flatten() {
            for thread in flow["threadFlows"].as_array().into_iter().flatten() {
                for loc in thread["locations"].as_array().into_iter().flatten() {
                    if let Some(line) =
                        loc["location"]["physicalLocation"]["region"]["startLine"].as_u64()
                    {
                        step_lines.insert(line);
                    }
                }
            }
        }
        for line in [11, 12, 13] {
            assert!(
                step_lines.contains(&line),
                "code flow is missing line {line} (steps at lines {step_lines:?}); \
                 line 12 is the summarized `transfer(&x[1], s)` call: {text}"
            );
        }
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

#[test]
fn test_android_phase4_noto_manifest_and_intent_counts() {
    run_store_test(|| {
        let import_name = "phase4_noto_import";
        let project_name = "phase4_noto_project";
        let import =
            ArtifactImport::try_create(import_name, ArtifactLanguage::Apk, &test_file()).unwrap();
        cli::import(
            &import,
            cli::ImportOptions {
                native_libs: false,
                ..Default::default()
            },
        )
        .unwrap();

        let manifest =
            ctadl_ascent::languages::android_manifest::AndroidManifest::load(import.import_path())
                .unwrap();
        assert_eq!(manifest.nodes.len(), 180);
        assert_eq!(manifest.attrs.len(), 302);
        let components = manifest.components();
        assert_eq!(components.len(), 35);
        assert_eq!(
            components
                .iter()
                .filter(|component| component.tag == "activity-alias")
                .count(),
            10
        );
        assert!(components.iter().any(|component| {
            component.descriptor.as_deref() == Some("Lcom/noto/app/AppActivity;")
                && component.exported == Some(true)
                && component.has_intent_filter
        }));
        assert!(components.iter().any(|component| {
            component.descriptor.as_deref() == Some("Lcom/noto/app/note/NoteReminderReceiver;")
                && component.exported == Some(false)
        }));

        let project = AnalysisProject::try_create(project_name, &[import_name]).unwrap();
        cli::index(&project, &[], &[], false, cli::IndexOptions::default()).unwrap();

        let index_path = project.index_path().unwrap();
        let pairs = ctadl_ascent::facts::schema::intent_pair::try_load(&index_path).unwrap();
        let explicit = pairs
            .iter()
            .filter(|(_, _, _, kind)| *kind == ctadl_ascent::facts::IntentPairKind::Explicit)
            .count();
        let implicit = pairs.len() - explicit;
        assert_eq!(explicit, 5, "intent pairs: {pairs:?}");
        assert_eq!(implicit, 1, "intent pairs: {pairs:?}");

        let calls = ctadl_ascent::facts::schema::call::try_load(&index_path).unwrap();
        assert!(
            calls.len() >= pairs.len(),
            "final call graph should include derived intent calls"
        );
    });
}

#[test]
fn test_android_phase4_explicit_activity_extra_flow() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <activity android:name=".AppActivity" android:exported="false" />
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Activity;
import android.os.Bundle;
import android.content.Intent;

public class Sender extends Activity {
    public void go(String tainted) {
        Intent i = new Intent(this, AppActivity.class);
        i.putExtra("k", tainted);
        startActivity(i);
    }
}

class AppActivity extends Activity {
    public void onCreate(Bundle b) {
        sink(getIntent().getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_link_case("phase4_explicit_activity", manifest, app, 1);
    });
}

#[test]
fn test_android_phase4_implicit_activity_extra_flow() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <activity android:name=".AppActivity" android:exported="true">
                    <intent-filter>
                        <action android:name="com.example.SEND" />
                        <category android:name="android.intent.category.DEFAULT" />
                        <data android:mimeType="text/plain" />
                    </intent-filter>
                </activity>
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Activity;
import android.os.Bundle;
import android.content.Intent;

public class Sender extends Activity {
    public void go(String tainted) {
        Intent i = new Intent("com.example.SEND");
        i.setType("text/plain");
        i.putExtra("k", tainted);
        startActivity(i);
    }
}

class AppActivity extends Activity {
    public void onCreate(Bundle b) {
        sink(getIntent().getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_link_case("phase4_implicit_activity", manifest, app, 1);
    });
}

#[test]
fn test_android_phase4_broadcast_receiver_extra_flow() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <receiver android:name=".MyReceiver" android:exported="true">
                    <intent-filter>
                        <action android:name="com.example.RECEIVE" />
                    </intent-filter>
                </receiver>
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public class Sender extends Context {
    public void go(String tainted) {
        Intent i = new Intent("com.example.RECEIVE");
        i.putExtra("k", tainted);
        sendBroadcast(i);
    }
}

class MyReceiver extends BroadcastReceiver {
    public void onReceive(Context c, Intent i) {
        sink(i.getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_link_case("phase4_broadcast", manifest, app, 1);
    });
}

#[test]
fn test_android_phase4_started_service_extra_flow() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <service android:name=".MyService" android:exported="false" />
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Service;
import android.content.Context;
import android.content.Intent;

public class Sender extends Context {
    public void go(String tainted) {
        Intent i = new Intent(this, MyService.class);
        i.putExtra("k", tainted);
        startService(i);
    }
}

class MyService extends Service {
    public int onStartCommand(Intent i, int flags, int startId) {
        sink(i.getStringExtra("k"));
        return 0;
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_link_case("phase4_started_service", manifest, app, 1);
    });
}

#[test]
fn test_android_phase4_explicit_activity_extra_tainted_path() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <activity android:name=".AppActivity" android:exported="false" />
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Activity;
import android.content.Intent;

public class Sender extends Activity {
    public void go(String tainted) {
        Intent i = new Intent(this, AppActivity.class);
        i.putExtra("k", tainted);
        startActivity(i);
    }
}

class AppActivity extends Activity {
    public void onNewIntent(Intent i) {
        sink(i.getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_flow_case(
            "phase4_explicit_activity_flow",
            manifest,
            app,
            "Lcom/example/AppActivity;",
        );
    });
}

#[test]
fn test_android_phase4_explicit_activity_get_intent_extra_tainted_path() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <activity android:name=".AppActivity" android:exported="false" />
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Activity;
import android.os.Bundle;
import android.content.Intent;

public class Sender extends Activity {
    public void go(String tainted) {
        Intent i = new Intent(this, AppActivity.class);
        i.putExtra("k", tainted);
        startActivity(i);
    }
}

class AppActivity extends Activity {
    public void onCreate(Bundle b) {
        sink(getIntent().getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_flow_case(
            "phase4_explicit_activity_get_intent_flow",
            manifest,
            app,
            "Lcom/example/AppActivity;",
        );
    });
}

#[test]
fn test_android_phase4_broadcast_receiver_extra_tainted_path() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <receiver android:name=".MyReceiver" android:exported="true">
                    <intent-filter>
                        <action android:name="com.example.RECEIVE" />
                    </intent-filter>
                </receiver>
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public class Sender extends Context {
    public void go(String tainted) {
        Intent i = new Intent("com.example.RECEIVE");
        i.putExtra("k", tainted);
        sendBroadcast(i);
    }
}

class MyReceiver extends BroadcastReceiver {
    public void onReceive(Context c, Intent i) {
        sink(i.getStringExtra("k"));
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_flow_case(
            "phase4_broadcast_flow",
            manifest,
            app,
            "Lcom/example/MyReceiver;",
        );
    });
}

#[test]
fn test_android_phase4_started_service_extra_tainted_path() {
    run_store_test(|| {
        if !android_tools_available() {
            return;
        }
        let manifest = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
            <application>
                <service android:name=".MyService" android:exported="false" />
            </application>
        </manifest>"#;
        let app = r#"package com.example;

import android.app.Service;
import android.content.Context;
import android.content.Intent;

public class Sender extends Context {
    public void go(String tainted) {
        Intent i = new Intent(this, MyService.class);
        i.putExtra("k", tainted);
        startService(i);
    }
}

class MyService extends Service {
    public int onStartCommand(Intent i, int flags, int startId) {
        sink(i.getStringExtra("k"));
        return 0;
    }
    static void sink(String s) {}
}
"#;
        run_android_icc_flow_case(
            "phase4_started_service_flow",
            manifest,
            app,
            "Lcom/example/MyService;",
        );
    });
}

fn android_tools_available() -> bool {
    which::which("javac").is_ok() && which::which("dx").is_ok()
}

fn run_android_icc_link_case(name: &str, manifest: &str, app_source: &str, expected_pairs: usize) {
    use ctadl_ascent::facts::schema;

    let (project, _dir) = import_index_android_icc_case(name, manifest, app_source, None);
    let index_path = project.index_path().unwrap();
    let pairs = schema::intent_pair::try_load(&index_path).unwrap();
    assert_eq!(
        pairs.len(),
        expected_pairs,
        "unexpected ICC pairs for {name}"
    );
    let calls = schema::call::try_load(&index_path).unwrap();
    assert!(
        calls.iter().any(|(caller, insn, callee)| pairs.iter().any(
            |(pair_caller, pair_insn, pair_callee, _)| caller == pair_caller
                && insn == pair_insn
                && callee == pair_callee
        )),
        "expected derived ICC pair to be persisted in final call graph for {name}"
    );
}

fn run_android_icc_flow_case(name: &str, manifest: &str, app_source: &str, sink_parent: &str) {
    use ctadl_ascent::facts::schema;
    use ctadl_ascent::query_engine::formatter::SarifProfile;

    let (project, dir) = import_index_android_icc_case(
        name,
        manifest,
        app_source,
        Some(android_icc_model(sink_parent)),
    );
    let model = dir.path().join("query.json");
    let index_path = project.index_path().unwrap();
    let pairs = schema::intent_pair::try_load(&index_path).unwrap();
    assert!(
        !pairs.is_empty(),
        "expected at least one ICC pair for {name}"
    );
    let calls = schema::call::try_load(&index_path).unwrap();
    assert!(
        calls.iter().any(|(caller, insn, callee)| pairs.iter().any(
            |(pair_caller, pair_insn, pair_callee, _)| caller == pair_caller
                && insn == pair_insn
                && callee == pair_callee
        )),
        "expected derived ICC pair to be persisted in final call graph for {name}"
    );
    let assign_like = schema::assign::try_load(&index_path).unwrap();
    let paths = schema::paths::try_load(&index_path).unwrap();
    assert!(
        paths.iter().any(|(p,)| p.to_dot_string() == ".<extras>"),
        "missing extras path for {name}; paths={paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|(p,)| p.to_dot_string() == ".<intent>.<extras>"),
        "missing composed activity intent extras path for {name}; paths={paths:?}"
    );
    assert!(
        assign_like.iter().any(
            |(_, _, dst, _, src)| dst.to_dot_string().contains("<extras>")
                || src.to_dot_string().contains("<extras>")
        ),
        "missing extras assign_like rows for {name}; assign_like={assign_like:?}"
    );

    let sarif = dir.path().join("out.sarif");
    cli::query(&project, &[model], &sarif, SarifProfile::default(), None).unwrap();

    let text = std::fs::read_to_string(&sarif).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let findings = doc["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|result| {
            result["ruleId"]
                .as_str()
                .is_some_and(|id| id.contains("tainted-path"))
                && result["kind"].as_str() == Some("fail")
        })
        .count();
    if findings == 0 {
        panic!(
            "expected an ICC tainted-path finding for {name}; pairs={pairs:?}; SARIF was {text}"
        );
    }
}

fn import_index_android_icc_case(
    name: &str,
    manifest: &str,
    app_source: &str,
    model_text: Option<String>,
) -> (AnalysisProject, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let apk = build_android_icc_apk(dir.path(), name, manifest, app_source);
    let import = ArtifactImport::try_create(name, ArtifactLanguage::Apk, &apk).unwrap();
    cli::import(
        &import,
        cli::ImportOptions {
            native_libs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let project = AnalysisProject::try_create(&format!("{name}_project"), &[name]).unwrap();
    let models = if let Some(model_text) = model_text {
        let model = dir.path().join("query.json");
        std::fs::write(&model, model_text).unwrap();
        vec![model]
    } else {
        Vec::new()
    };
    cli::index(&project, &[], &models, false, cli::IndexOptions::default()).unwrap();
    (project, dir)
}

fn android_icc_model(sink_parent: &str) -> String {
    format!(
        r#"{{
  "model_generators": [
    {{
      "find": "callsites",
      "where": [{{"constraint": "signature_match", "name": "putExtra", "parent": "Landroid/content/Intent;"}}],
      "model": {{"sources": [{{"kind": "UserInput", "port": "Argument(2)"}}]}}
    }},
    {{
      "find": "methods",
      "where": [{{"constraint": "signature_match", "name": "sink", "parent": "{sink_parent}"}}],
      "model": {{"sinks": [{{"kind": "TaintedData", "port": "Argument(0)"}}]}}
    }}
  ]
}}"#
    )
}

fn build_android_icc_apk(
    dir: &std::path::Path,
    name: &str,
    manifest: &str,
    app_source: &str,
) -> PathBuf {
    let src = dir.join("src");
    let classes = dir.join("classes");
    std::fs::create_dir_all(src.join("android/app")).unwrap();
    std::fs::create_dir_all(src.join("android/content")).unwrap();
    std::fs::create_dir_all(src.join("android/os")).unwrap();
    std::fs::create_dir_all(src.join("com/example")).unwrap();

    std::fs::write(src.join("android/content/Context.java"), ANDROID_CONTEXT).unwrap();
    std::fs::write(src.join("android/content/Intent.java"), ANDROID_INTENT).unwrap();
    std::fs::write(
        src.join("android/content/BroadcastReceiver.java"),
        ANDROID_RECEIVER,
    )
    .unwrap();
    std::fs::write(
        src.join("android/content/ComponentName.java"),
        ANDROID_COMPONENT_NAME,
    )
    .unwrap();
    std::fs::write(
        src.join("android/content/ServiceConnection.java"),
        ANDROID_SERVICE_CONNECTION,
    )
    .unwrap();
    std::fs::write(src.join("android/app/Activity.java"), ANDROID_ACTIVITY).unwrap();
    std::fs::write(src.join("android/app/Service.java"), ANDROID_SERVICE).unwrap();
    std::fs::write(src.join("android/os/Bundle.java"), ANDROID_BUNDLE).unwrap();
    std::fs::write(src.join("android/os/IBinder.java"), ANDROID_IBINDER).unwrap();
    std::fs::write(src.join("com/example/Sender.java"), app_source).unwrap();

    std::fs::create_dir_all(&classes).unwrap();
    let mut sources = Vec::new();
    collect_java_sources(&src, &mut sources);
    let mut javac = Command::new("javac");
    javac
        .args(["--release", "8", "-encoding", "UTF-8", "-d"])
        .arg(&classes)
        .args(&sources);
    assert!(javac.status().unwrap().success(), "javac failed");

    let dex = dir.join("classes.dex");
    let class_files = collect_class_files_for_dx(&classes);
    let mut dx = Command::new("dx");
    dx.current_dir(&classes)
        .arg("--dex")
        .arg("--min-sdk-version=24")
        .arg(format!("--output={}", dex.display()))
        .args(class_files);
    assert!(dx.status().unwrap().success(), "dx failed");

    let apk = dir.join(format!("{name}.apk"));
    let file = std::fs::File::create(&apk).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    use std::io::Write as _;
    zip.start_file("AndroidManifest.xml", options).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file("classes.dex", options).unwrap();
    zip.write_all(&std::fs::read(dex).unwrap()).unwrap();
    zip.finish().unwrap();
    apk
}

fn collect_java_sources(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_java_sources(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("java") {
            out.push(path);
        }
    }
}

fn collect_class_files_for_dx(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_class_files_relative(dir, dir, &mut out);
    out
}

fn collect_class_files_relative(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<PathBuf>,
) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_class_files_relative(root, &path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("class") {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

const ANDROID_CONTEXT: &str = r#"package android.content;
public class Context {
  public void startActivity(Intent i) {}
  public void sendBroadcast(Intent i) {}
  public ComponentName startService(Intent i) { return null; }
  public boolean bindService(Intent i, ServiceConnection c, int flags) { return true; }
}"#;

const ANDROID_INTENT: &str = r#"package android.content;
public class Intent {
  public Intent() {}
  public Intent(String action) {}
  public Intent(Context c, Class cls) {}
  public Intent setAction(String s) { return this; }
  public Intent setType(String s) { return this; }
  public Intent setDataAndType(Object d, String s) { return this; }
  public Intent putExtra(String k, String v) { return this; }
  public String getStringExtra(String k) { return null; }
}"#;

const ANDROID_ACTIVITY: &str = r#"package android.app;
public class Activity extends android.content.Context {
  public android.content.Intent getIntent() { return null; }
  public void setIntent(android.content.Intent i) {}
}"#;

const ANDROID_RECEIVER: &str = r#"package android.content;
public class BroadcastReceiver {
  public void onReceive(Context c, Intent i) {}
}"#;

const ANDROID_SERVICE: &str = r#"package android.app;
public class Service extends android.content.Context {
}"#;

const ANDROID_BUNDLE: &str = "package android.os; public class Bundle {}";
const ANDROID_IBINDER: &str = "package android.os; public interface IBinder {}";
const ANDROID_COMPONENT_NAME: &str = "package android.content; public class ComponentName { public ComponentName(Context c, String s) {} public ComponentName(String p, String c) {} }";
const ANDROID_SERVICE_CONNECTION: &str =
    "package android.content; public interface ServiceConnection {}";

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
            matches!(err, ctadl_import::Error::NothingToImport { .. }),
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
//            ArtifactImport::try_create("test_index_artifact", ArtifactLanguage::Dex, &dex_file);
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
        project.write_index_config(None).unwrap();
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
            Err(ctadl_import::Error::IncompatibleIndex {
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
            Err(ctadl_import::Error::IncompatibleIndex { found, .. }) => {
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
// `inspect --dump-index-graph`: rendering the index graph from a finished index.
//
// The graph used to come out of `ctadl index --dump-index-graph`, as a side effect of a run
// that already had `assign_like` in memory. It now comes out of `inspect`, which reads the two
// tables it needs back from the index directory -- so what these cover is that read path:
// `assign.parquet` for the edges, `function_id.parquet` for the labels.
// ---------------------------------------------------------------------------

/// Imports and indexes `xfer.c` into a project of its own, ready to dump.
fn index_xfer_project(import_name: &str, project_name: &str) -> AnalysisProject {
    let import =
        ArtifactImport::try_create(import_name, ArtifactLanguage::C, &c_fixture("xfer.c")).unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();
    let project = AnalysisProject::try_create(project_name, &[import_name]).unwrap();
    cli::index(
        &project,
        &[],
        &[c_fixture("xfer.json")],
        false,
        cli::IndexOptions::default(),
    )
    .unwrap();
    project
}

/// The DOT lines that declare an edge. Skips the leading legend, which explains the `A -> B`
/// convention and would otherwise count as an edge.
fn edge_lines(dot: &str) -> Vec<&str> {
    dot.lines()
        .filter(|l| l.contains("->") && !l.trim_start().starts_with("//"))
        .collect()
}

#[test]
fn inspect_index_graph_writes_a_dot_file() {
    run_store_test(|| {
        let project = index_xfer_project("dump_graph_c", "dump_graph_c_proj");

        let out_dir = tempdir().unwrap();
        let dot_path = out_dir.path().join("index.dot");
        cli::inspect_index_graph(&project, &dot_path).unwrap();

        let dot = std::fs::read_to_string(&dot_path).unwrap();
        // The legend is embedded as a leading DOT comment so the file documents itself.
        assert!(
            dot.starts_with("// Index (assign-like) graph."),
            "expected the legend comment first:\n{dot}"
        );
        assert!(dot.contains("digraph index_graph"), "not a digraph:\n{dot}");
        assert!(
            !edge_lines(&dot).is_empty(),
            "expected at least one edge:\n{dot}"
        );
    });
}

/// The dumped graph is the index's `assign` relation, labelled through its `IdMap`.
///
/// One edge line per stored row, and the labels name the functions the fixture declares rather
/// than the `func_<n>` fallback `render_index_graph` uses when a lookup misses -- which is what
/// says `function_id.parquet` was loaded and really does resolve the ids in `assign.parquet`.
#[test]
fn inspect_index_graph_matches_the_stored_assign_relation() {
    use ctadl_ascent::facts;

    run_store_test(|| {
        let project = index_xfer_project("dump_graph_eq_c", "dump_graph_eq_c_proj");

        let out_dir = tempdir().unwrap();
        let dot_path = out_dir.path().join("index.dot");
        cli::inspect_index_graph(&project, &dot_path).unwrap();
        let dot = std::fs::read_to_string(&dot_path).unwrap();

        let index_path = project.index_path().unwrap();
        let assign = facts::schema::assign::try_load(&index_path).unwrap();
        assert!(!assign.is_empty(), "the fixture must index to something");
        assert_eq!(
            edge_lines(&dot).len(),
            assign.len(),
            "one edge line per assign row:\n{dot}"
        );

        assert!(
            dot.contains("transfer"),
            "expected a function name from the fixture, not the func_<n> fallback:\n{dot}"
        );
        // `render_index_graph` falls back to `func_<n>` for an id the IdMap does not know, and
        // a label always opens with the function name, so this is that fallback and nothing else.
        assert!(
            !dot.contains("[label=\"func_"),
            "every function id must resolve through the IdMap:\n{dot}"
        );
    });
}

#[test]
fn inspect_index_graph_without_an_index_fails() {
    run_store_test(|| {
        // Created but never indexed: `has_index` is false, so this must not reach a table.
        let project =
            AnalysisProject::try_create("dump_graph_noindex", &["nonexistent_import"]).unwrap();
        let out_dir = tempdir().unwrap();
        let dot_path = out_dir.path().join("index.dot");

        match cli::inspect_index_graph(&project, &dot_path) {
            Err(ctadl_ascent::error::Error::Import(ctadl_import::Error::MissingIndex {
                project: p,
            })) => assert_eq!(p, "dump_graph_noindex"),
            other => panic!("expected MissingIndex, got: {other:?}"),
        }
        assert!(
            !dot_path.exists(),
            "a failed dump must not leave a file behind"
        );
    });
}

/// An index this build cannot read is refused by the version gate, before any table is touched
/// -- so this needs no parquet files at all.
#[test]
fn inspect_index_graph_rejects_a_stale_index() {
    run_store_test(|| {
        let project =
            AnalysisProject::try_create("dump_graph_stale", &["nonexistent_import"]).unwrap();
        let config = project.index_path().unwrap().join(INDEX_CONFIG_FILE);
        std::fs::write(&config, r#"{"version":"1"}"#).unwrap();

        let out_dir = tempdir().unwrap();
        match cli::inspect_index_graph(&project, &out_dir.path().join("index.dot")) {
            Err(ctadl_ascent::error::Error::Import(ctadl_import::Error::IncompatibleIndex {
                found,
                ..
            })) => assert_eq!(found, "1"),
            other => panic!("expected IncompatibleIndex, got: {other:?}"),
        }
    });
}

/// `ctadl inspect <name>` on a project reports what the store knows about it. A project that was
/// never indexed says so -- and, since this is a read-only view, leaves no `index/` behind
/// claiming it was.
#[test]
fn inspect_project_reports_a_project_that_was_never_indexed() {
    run_store_test(|| {
        let import = ArtifactImport::try_create(
            "inspect_proj_import",
            ArtifactLanguage::C,
            &c_fixture("xfer.c"),
        )
        .unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();
        let project =
            AnalysisProject::try_create("inspect_proj_none", &["inspect_proj_import"]).unwrap();

        let summary = cli::summarize_project(&project).unwrap();
        assert_eq!(summary.name, "inspect_proj_none");
        assert_eq!(summary.imports.len(), 1);
        assert_eq!(summary.imports[0].name, "inspect_proj_import");
        assert!(
            summary.imports[0].problem.is_none(),
            "a readable import has nothing to report: {:?}",
            summary.imports[0]
        );
        assert!(
            matches!(summary.index, cli::IndexStatus::Missing),
            "expected no index, got: {:?}",
            summary.index
        );
        assert!(
            !project.dir().join("index").exists(),
            "inspecting must not create the index directory"
        );
    });
}

/// A project outlives the imports it names. The one that is gone is named in its own line
/// instead of failing the whole summary.
#[test]
fn inspect_project_names_an_import_that_is_gone() {
    run_store_test(|| {
        let project =
            AnalysisProject::try_create("inspect_proj_missing", &["inspect_proj_absent"]).unwrap();

        let summary = cli::summarize_project(&project).unwrap();
        assert_eq!(summary.imports.len(), 1);
        assert!(
            summary.imports[0]
                .problem
                .as_deref()
                .is_some_and(|s| s.contains("missing"))
        );
    });
}

/// An import's config is written before the artifact is translated, so a config on its own is
/// not a finished import. The stored program is what says so.
#[test]
fn inspect_project_flags_an_import_that_never_finished() {
    run_store_test(|| {
        // `try_create` writes the config; without `cli::import` there is no stored program,
        // which is exactly what an import that died during translation leaves behind.
        ArtifactImport::try_create(
            "inspect_proj_partial",
            ArtifactLanguage::C,
            &c_fixture("xfer.c"),
        )
        .unwrap();
        let project =
            AnalysisProject::try_create("inspect_proj_half", &["inspect_proj_partial"]).unwrap();

        let summary = cli::summarize_project(&project).unwrap();
        let problem = summary.imports[0].problem.as_deref().unwrap_or("");
        assert!(
            problem.contains("never finished"),
            "expected the half-written import to be flagged, got: {problem:?}"
        );
    });
}

/// Tables with no stamp are an index that never finished. They are still counted: the row count
/// comes from the parquet footer, not from the encoding an unreadable index gets wrong, and how
/// big the last index was is what decides whether to re-run `ctadl index`.
#[test]
fn inspect_project_counts_the_tables_of_an_unfinished_index() {
    run_store_test(|| {
        use ctadl_ascent::facts::FunctionId;
        use ctadl_ascent::facts::schema::external_function;

        let project =
            AnalysisProject::try_create("inspect_proj_partial_index", &["inspect_proj_absent"])
                .unwrap();
        let index = project.index_path().unwrap();
        external_function::try_save(&index, vec![(FunctionId::new(1),), (FunctionId::new(2),)])
            .unwrap();

        match cli::summarize_project(&project).unwrap().index {
            cli::IndexStatus::Unfinished { tables } => {
                assert_eq!(tables.len(), 1);
                assert_eq!(tables[0].name, "external_function");
                assert_eq!(tables[0].rows, Some(2));
            }
            other => panic!("expected an unfinished index, got: {other:?}"),
        }
    });
}

/// A stamp naming another format is a stale index, which is a different report from an
/// unfinished one: it finished, this build just cannot read it.
#[test]
fn inspect_project_reports_a_stale_index() {
    run_store_test(|| {
        let project =
            AnalysisProject::try_create("inspect_proj_stale", &["inspect_proj_absent"]).unwrap();
        let config = project.index_path().unwrap().join(INDEX_CONFIG_FILE);
        std::fs::write(&config, r#"{"version":"1"}"#).unwrap();

        match cli::summarize_project(&project).unwrap().index {
            cli::IndexStatus::Stale {
                found, expected, ..
            } => {
                assert_eq!(found, "1");
                assert_eq!(expected, INDEX_FORMAT_VERSION);
            }
            other => panic!("expected a stale index, got: {other:?}"),
        }
    });
}

/// A re-index drops the stamp before it overwrites the first table, so a run that dies partway
/// leaves an index that reads as unfinished rather than one the old stamp still vouches for.
#[cfg(unix)]
#[test]
fn a_failed_reindex_leaves_no_stamp() {
    run_store_test(|| {
        use std::os::unix::fs::PermissionsExt;

        let project = index_xfer_project("reindex_c", "reindex_c_proj");
        assert!(project.has_index(), "the first index must stamp");

        // Make one table unwritable so the second run fails after it has overwritten others.
        let table = project.index_path().unwrap().join("summary.parquet");
        let readonly = std::fs::Permissions::from_mode(0o444);
        std::fs::set_permissions(&table, readonly).unwrap();
        let result = cli::index(
            &project,
            &[],
            &[c_fixture("xfer.json")],
            false,
            cli::IndexOptions::default(),
        );
        std::fs::set_permissions(&table, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(result.is_err(), "the re-index was supposed to fail");
        assert!(
            !project.has_index(),
            "a half-written index must not carry the previous run's stamp"
        );
        assert!(
            matches!(
                cli::summarize_project(&project).unwrap().index,
                cli::IndexStatus::Unfinished { .. }
            ),
            "the tables are there, so this is an unfinished index, not a missing one"
        );
    });
}

/// A stamped index reads back as ready, one entry per table.
#[test]
fn inspect_project_lists_the_tables_of_a_readable_index() {
    run_store_test(|| {
        use ctadl_ascent::facts::FunctionId;
        use ctadl_ascent::facts::schema::external_function;

        let project =
            AnalysisProject::try_create("inspect_proj_ready", &["inspect_proj_absent"]).unwrap();
        let index = project.index_path().unwrap();
        external_function::try_save(&index, vec![(FunctionId::new(7),)]).unwrap();
        project.write_index_config(None).unwrap();

        match cli::summarize_project(&project).unwrap().index {
            cli::IndexStatus::Ready { tables } => {
                assert_eq!(tables.len(), 1);
                assert_eq!(tables[0].name, "external_function");
                assert_eq!(tables[0].rows, Some(1));
                assert!(tables[0].bytes > 0);
            }
            other => panic!("expected a readable index, got: {other:?}"),
        }
    });
}
