//! Sources and sinks on a global, end to end (R26-R28 of the compositional-globals spec).
//!
//! A global is shared by every instance of every function (G2), so a source on one is anchored
//! at no function and fanned out to no call site, and a flow from it reaches a load of it in any
//! function, with no call between them. Its SARIF location names the program and the global.

use crate::common::store_dir;
use ctadl_ascent::cli;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_ascent::query_engine::formatter::SarifProfile;

/// `consume` reads `g_data`, which nothing writes and nothing calls `consume` with; `quiet`
/// reads `other`, which `store` writes from `input`.
const PROGRAM: &str = r"
void sink(int x);
int input(void);
int g_data;
int other;

void consume(void) { sink(g_data); }
void quiet(void) { sink(other); }
void store(void) { other = input(); }
";

/// Imports and indexes `PROGRAM` as `case`, queries it with `query`, and returns the SARIF.
fn run(case: &str, query: &str) -> serde_json::Value {
    let dir = store_dir().join(case);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("g.c");
    std::fs::write(&src, PROGRAM).unwrap();
    let models = dir.join("query.ctadl");
    std::fs::write(&models, query).unwrap();

    let import = ArtifactImport::try_create(case, ArtifactLanguage::C, &src).expect("import args");
    cli::import(&import, cli::ImportOptions::default()).expect("importing");
    let project =
        AnalysisProject::try_create(case, &[case.to_string()], SubImports::All).expect("project");
    cli::index(&project, &[], &[], true, cli::IndexOptions::default()).expect("indexing");
    let sarif = dir.join("out.sarif");
    cli::query(&project, &[models], &sarif, SarifProfile::Human, None).expect("querying");
    serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap()
}

fn results(sarif: &serde_json::Value) -> Vec<&serde_json::Value> {
    sarif["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .collect()
}

fn flow_found(sarif: &serde_json::Value) -> bool {
    results(sarif)
        .iter()
        .any(|r| r["ruleId"] == "C0001.tainted-path" && r["kind"] == "fail")
}

/// R26, R28: a source on `G::self.deref` reaches a sink in a function no one calls.
#[test]
fn a_source_on_a_global_reaches_a_sink_with_no_call_between() {
    let sarif = run(
        "global_source",
        r#"source(G::self.deref, kind = "Config") :- global(G, name = "g_data");
           sink(F::arg(0), kind = "Out") :- fun(F, name = "sink");"#,
    );
    assert!(flow_found(&sarif), "{sarif:#}");

    // R27: the source is located by the program and the global, not by a function.
    let source = results(&sarif)
        .into_iter()
        .find(|r| r["ruleId"] == "C0003.taint-source")
        .expect("a source result");
    let text = source["message"]["text"].as_str().unwrap();
    assert!(
        text.contains("$g_data.deref in global global_source::$g_data"),
        "{text}"
    );
    let index = source["locations"][0]["logicalLocations"][0]["index"]
        .as_u64()
        .unwrap() as usize;
    let logical = &sarif["runs"][0]["logicalLocations"][index];
    assert_eq!(logical["fullyQualifiedName"], "global_source::$g_data");
    assert_eq!(logical["kind"], "variable");
    // And at its declaration, the declarator `g_data` of `int g_data;` on line 4.
    let physical = &source["locations"][0]["physicalLocation"];
    assert_eq!(physical["artifactLocation"]["uri"], "g.c", "{physical:#}");
    assert_eq!(physical["region"]["startLine"], 4, "{physical:#}");
    assert_eq!(physical["region"]["startColumn"], 5, "{physical:#}");
    assert_eq!(physical["region"]["endColumn"], 10, "{physical:#}");
}

/// A source on another global reaches nothing: the flow above is through `g_data` itself.
#[test]
fn a_source_on_an_unread_global_reaches_nothing() {
    let sarif = run(
        "global_source_unread",
        r#"source(G::self.deref, kind = "Config") :- global(G, name = "absent_from_sinks");
           source(G::self.f, kind = "Config") :- global(G, name = "g_data");
           sink(F::arg(0), kind = "Out") :- fun(F, name = "sink");"#,
    );
    assert!(!flow_found(&sarif), "{sarif:#}");
}

/// R26: a sink on a global is reached from a store to it in any function, and a sink on another
/// global is not.
#[test]
fn a_sink_on_a_global_is_reached_from_any_store() {
    let sarif = run(
        "global_sink",
        r#"source(F::return, kind = "In") :- fun(F, name = "input");
           sink(G::self.deref, kind = "Stored") :- global(G, name = "other");"#,
    );
    assert!(flow_found(&sarif), "{sarif:#}");
    let sarif = run(
        "global_sink_other",
        r#"source(F::return, kind = "In") :- fun(F, name = "input");
           sink(G::self.deref, kind = "Stored") :- global(G, name = "g_data");"#,
    );
    assert!(!flow_found(&sarif), "{sarif:#}");
}
