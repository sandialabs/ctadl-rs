//! SARIF for a project that indexes more than one artifact.
//!
//! A source span id is an index into *one* import's source-info database. Function and
//! instruction ids are project-global, span ids are not, so a span read against the wrong
//! import's database still resolves -- to an unrelated line in an unrelated file. The formatter
//! used to render every result once per import and resolve every span in each, which turned a
//! two-import project's log into each finding twice: once where it is, once carrying whatever
//! the other artifact happened to have at that span id. `ctadl index` now records the import
//! each span was numbered in (`index_source_map.import_id`) and the formatter resolves each span
//! only there.
//!
//! Lua is the frontend used here for the same reason `port_semantics.rs` uses it: it is a real
//! frontend that needs no external toolchain, so this runs everywhere `cargo test` does. The
//! defect is frontend-independent -- it is about how many source-info databases a project has,
//! not what wrote them. The two-artifact case that motivated it is JNI (a Dex plus a shared
//! library), pinned end to end by `nightly/tests/jni/`, which needs Ghidra and a Java toolchain.

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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

/// First artifact: a source-to-sink flow on lines 11 and 12.
const PROGRAM_A: &str = r#"-- multi_import_a.lua
local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main_a()
  local x = source()
  sink(x)
end

main_a()
"#;

/// Second artifact: the same shape, deliberately padded so its flow lands on entirely
/// different lines (19 and 20) from the first's. Line numbers that cannot coincide are the
/// point: a location resolved against the wrong import's database would otherwise be free to
/// look plausible.
const PROGRAM_B: &str = r#"-- multi_import_b.lua
--
--
--
--
--
--
--
--
local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main_b()
  local y = source()
  sink(y)
end

main_b()
"#;

const QUERY_MODEL: &str = r#"{"model_generators":[
{"find":"methods","where":[{"constraint":"signature_match","names":["source"]}],
 "model":{"sources":[{"port":"Return","kind":"UserInput"}]}},
{"find":"methods","where":[{"constraint":"signature_match","names":["sink"]}],
 "model":{"sinks":[{"port":"Argument(0)","kind":"UserInput"}]}}
]}"#;

/// Import both programs, index them as one project, and write the SARIF for `profile`.
fn query_two_import_project(case: &str, profile: SarifProfile) -> Value {
    let dir = store_dir();
    let imports: Vec<String> = [("a", PROGRAM_A), ("b", PROGRAM_B)]
        .into_iter()
        .map(|(half, text)| {
            let name = format!("{case}_{half}");
            let src = dir.join(format!("{name}.lua"));
            std::fs::write(&src, text).expect("writing lua source");
            let import = ArtifactImport::try_create(&name, ArtifactLanguage::Lua, &src)
                .expect("import args");
            cli::import(&import, cli::ImportOptions::default()).expect("importing lua");
            name
        })
        .collect();

    let project = AnalysisProject::try_create(case, &imports).expect("project");
    cli::index(
        &project,
        &[],
        &[],
        false,
        false,
        CallResolutionStrategy::Mixed,
        true,
        true,
        None,
    )
    .expect("indexing");

    let query_models = dir.join(format!("{case}-query.json"));
    std::fs::write(&query_models, QUERY_MODEL).expect("writing query model");
    let sarif: PathBuf = dir.join(format!("{case}-{profile:?}.sarif"));
    cli::query(&project, &[query_models], false, &sarif, profile, None).expect("querying");

    let text = std::fs::read_to_string(&sarif).expect("reading sarif");
    serde_json::from_str(&text).expect("parsing sarif")
}

fn results(sarif: &Value) -> Vec<&Value> {
    sarif["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|run| run["results"].as_array())
        .flatten()
        .collect()
}

/// The file name and `region.startLine` of a location, as `"b.lua:20"`.
fn located(location: &Value) -> String {
    let physical = &location["physicalLocation"];
    let uri = physical["artifactLocation"]["uri"]
        .as_str()
        .unwrap_or("<none>");
    let file = uri.rsplit('/').next().unwrap_or(uri);
    let line = physical["region"]["startLine"].as_i64();
    match line {
        Some(line) => format!("{file}:{line}"),
        None => format!("{file}:<no line>"),
    }
}

fn code_flow_steps(result: &Value) -> Vec<String> {
    result["codeFlows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|flow| flow["threadFlows"].as_array())
        .flatten()
        .filter_map(|thread| thread["locations"].as_array())
        .flatten()
        .map(|step| located(&step["location"]))
        .collect()
}

/// Each artifact's flow is reported once, in that artifact, on that artifact's lines.
///
/// Before spans carried their import, this run reported four paths rather than two: each of the
/// two real flows a second time with its lines read out of the other file.
#[test]
fn each_flow_is_reported_once_in_its_own_artifact() {
    let sarif = query_two_import_project("multi_import_human", SarifProfile::Human);
    let paths: Vec<&Value> = results(&sarif)
        .into_iter()
        .filter(|r| r["ruleId"] == "C0001.tainted-path" && r["kind"] == "fail")
        .collect();

    let reported: Vec<String> = paths.iter().map(|r| located(&r["locations"][0])).collect();
    assert_eq!(
        reported.iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &"multi_import_human_a.lua:12".to_string(),
            &"multi_import_human_b.lua:20".to_string()
        ]),
        "each flow is reported at its own sink line: {reported:?}"
    );
    assert_eq!(
        paths.len(),
        2,
        "two flows, two results -- no per-import copies: {reported:?}"
    );

    // A flow may not wander into the other artifact, and every line it names must be one of the
    // lines that artifact's flow actually runs on.
    for (result, where_reported) in paths.iter().zip(&reported) {
        let half = where_reported.split(".lua").next().expect("file stem");
        let allowed: BTreeSet<String> = if half.ends_with("_a") {
            [11, 12].iter().map(|l| format!("{half}.lua:{l}")).collect()
        } else {
            [19, 20].iter().map(|l| format!("{half}.lua:{l}")).collect()
        };
        let steps = code_flow_steps(result);
        assert!(!steps.is_empty(), "flow at {where_reported} has no steps");
        for step in &steps {
            assert!(
                allowed.contains(step),
                "step {step} of the flow at {where_reported} is not a line of that artifact's \
                 flow (allowed {allowed:?})"
            );
        }
    }
}

/// No instruction is rendered twice.
///
/// The debug profile carries each tainted instruction's `funcId`/`insnId`, which identify it
/// project-wide, and the `import` its location was resolved in. One result per instruction, in
/// the import that instruction belongs to, is precisely what rendering per import broke.
#[test]
fn each_tainted_instruction_is_rendered_once() {
    let sarif = query_two_import_project("multi_import_debug", SarifProfile::Debug);
    let instructions: Vec<&Value> = results(&sarif)
        .into_iter()
        .filter(|r| r["ruleId"] == "C0002.tainted-instruction")
        .collect();
    assert!(
        !instructions.is_empty(),
        "the debug profile reports tainted instructions"
    );

    let mut seen: BTreeSet<(i64, i64)> = BTreeSet::new();
    for result in &instructions {
        let properties = &result["properties"];
        let site = (
            properties["funcId"].as_i64().expect("funcId"),
            properties["insnId"].as_i64().expect("insnId"),
        );
        assert!(
            seen.insert(site),
            "instruction {site:?} is reported more than once, at {}",
            located(&result["locations"][0])
        );

        // The location must come from the import the span was numbered in, not another one.
        let import = properties["import"].as_str().expect("import");
        let where_reported = located(&result["locations"][0]);
        assert!(
            where_reported.starts_with(import),
            "instruction {site:?} was resolved in import {import} but located at \
             {where_reported}"
        );
    }
}
