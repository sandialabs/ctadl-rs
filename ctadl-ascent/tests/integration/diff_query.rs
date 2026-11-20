//! `diff-query` on SARIF from real `ctadl query` runs. An edit that changes nothing a result
//! means (order where SARIF gives it none, the numbering of `run.logicalLocations`, instruction
//! and function ids, message text) reports nothing, and an edit that does is reported as
//! exactly what it changed.
//!
//! The fixture is `ties.c` indexed with `ties.json` and queried in every profile. Each test
//! edits that SARIF as JSON, or queries `ties.c` indexed with other ids, and first checks that
//! the edit changed something, so an edit that is a no-op cannot pass.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ctadl_ascent::cli;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use ctadl_query_diff::{ENDPOINTS_RULE, Options, Report, diff_query};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use crate::common::store_dir;

const PROFILES: [(SarifProfile, &str); 4] = [
    (SarifProfile::Human, "human"),
    (SarifProfile::Agent, "agent"),
    (SarifProfile::Machine, "machine"),
    (SarifProfile::Debug, "debug"),
];

const IMPORT: &str = "diff_query_ties";

fn fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "c", name]
        .iter()
        .collect()
}

/// Queries `project` in every profile, writing `<profile>.sarif` into a new directory `out`
/// in the store.
fn query_all(project: &AnalysisProject, out: &str) -> PathBuf {
    let dir = store_dir().join(out);
    std::fs::create_dir_all(&dir).unwrap();
    for (profile, name) in PROFILES {
        let sarif = dir.join(format!("{name}.sarif"));
        cli::query(project, &[fixture("ties.json")], &sarif, profile, None).unwrap();
    }
    dir
}

/// The directory of `ties.c`'s SARIF in every profile.
fn ties() -> &'static Path {
    static TIES: OnceLock<PathBuf> = OnceLock::new();
    TIES.get_or_init(|| {
        store_dir();
        let import =
            ArtifactImport::try_create(IMPORT, ArtifactLanguage::C, &fixture("ties.c")).unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();
        let project =
            AnalysisProject::try_create("diff_query_ties_proj", &[IMPORT], SubImports::All)
                .unwrap();
        let models = [fixture("ties.json")];
        cli::index(&project, &[], &models, false, cli::IndexOptions::default()).unwrap();
        query_all(&project, "diff_query_ties")
    })
}

/// The SARIF of `ties.c` in `profile`.
fn sarif(profile: &str) -> PathBuf {
    ties().join(format!("{profile}.sarif"))
}

/// A program with no sources or sinks, imported before `ties.c` in [`renumbered`].
const PAD_C: &str =
    "int pad_one(int v) { return v + 1; }\nint pad_two(int v) { return pad_one(v) * 2; }\n";

/// The directory of the SARIF, in every profile, of `ties.c` indexed after an import that
/// takes the first function and instruction ids, so `ties.c`'s are all different.
///
/// It is a new index rather than `ties.c`'s with its ids rewritten: `query` resolves an
/// index's function and instruction ids against the imports too (callee names, for one), so
/// an index renumbered on its own no longer agrees with them. Span ids are numbered per
/// import, so they stay the same.
fn renumbered() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // Imports `ties.c`.
        ties();
        let pad = "diff_query_aaa_pad";
        let src = store_dir().join("diff_query_aaa_pad.c");
        std::fs::write(&src, PAD_C).unwrap();
        let import = ArtifactImport::try_create(pad, ArtifactLanguage::C, &src).unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();
        let project = AnalysisProject::try_create(
            "diff_query_ties_renumbered_proj",
            &[pad, IMPORT],
            SubImports::All,
        )
        .unwrap();
        let models = [fixture("ties.json")];
        cli::index(&project, &[], &models, false, cli::IndexOptions::default()).unwrap();
        query_all(&project, "diff_query_ties_renumbered")
    })
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Writes `doc` into a new temporary directory.
fn write(doc: &Value) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("edited.sarif");
    std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
    (tmp, path)
}

/// Asserts that `a` and `b` serialize differently: key order counts.
fn assert_changed(a: &Value, b: &Value, what: &str) {
    assert_ne!(
        a.to_string(),
        b.to_string(),
        "{what}: the edit changed nothing"
    );
}

fn results(doc: &Value) -> &Vec<Value> {
    doc["runs"][0]["results"].as_array().unwrap()
}

fn results_mut(doc: &mut Value) -> &mut Vec<Value> {
    doc["runs"][0]["results"].as_array_mut().unwrap()
}

fn rule(result: &Value) -> &str {
    result["ruleId"].as_str().unwrap()
}

/// The steps of a result's first thread flow, if it has one.
fn steps_mut(result: &mut Value) -> Option<&mut Vec<Value>> {
    result
        .pointer_mut("/codeFlows/0/threadFlows/0/locations")?
        .as_array_mut()
}

fn steps(result: &Value) -> Option<&Vec<Value>> {
    result
        .pointer("/codeFlows/0/threadFlows/0/locations")?
        .as_array()
}

/// A step without what the comparison drops from it, so two steps with equal keys compare
/// equal.
fn step_key(step: &Value) -> String {
    let mut s = step.clone();
    let obj = s.as_object_mut().unwrap();
    obj.remove("executionOrder");
    if let Some(loc) = obj.get_mut("location").and_then(Value::as_object_mut) {
        loc.remove("message");
    }
    s.to_string()
}

/// Numbers `steps` 1, 2, ... in array order.
fn renumber_steps(steps: &mut [Value]) {
    for (k, step) in steps.iter_mut().enumerate() {
        step["executionOrder"] = json!(k + 1);
    }
}

fn diff(a: &Path, b: &Path, endpoints_only: bool) -> Report {
    let opts = Options {
        limit: usize::MAX,
        endpoints_only,
    };
    diff_query(a, b, &opts).unwrap_or_else(|e| panic!("{e}"))
}

/// Asserts `a` and `b` are identical, with and without the endpoints rule.
fn assert_identical(a: &Path, b: &Path, what: &str) {
    for endpoints_only in [false, true] {
        let report = diff(a, b, endpoints_only);
        assert!(
            report.identical(),
            "{what} (endpoints only: {endpoints_only}):\n{}",
            report.to_text()
        );
    }
}

/// The `(count in A, count in B)` of each result one side of a report shows.
type Counts = Vec<(usize, usize)>;

/// Asserts that `report` has `in_a` results more often in A and `in_b` more often in B, and
/// returns each side's counts.
fn assert_reported(report: &Report, in_a: usize, in_b: usize, what: &str) -> (Counts, Counts) {
    let text = report.to_text();
    assert_eq!(
        (report.more_in_a.total(), report.more_in_b.total()),
        (in_a, in_b),
        "{what}:\n{text}"
    );
    let counts =
        |s: &ctadl_query_diff::Samples| s.rows.iter().map(|r| (r.count_a, r.count_b)).collect();
    (counts(&report.more_in_a), counts(&report.more_in_b))
}

/// A seeded generator of permutations.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize % n
    }

    /// A permutation of `0..n` that moves something when `n >= 2`: `p[j]` is the old position
    /// of what goes to `j`.
    fn permutation(&mut self, n: usize) -> Vec<usize> {
        let mut p: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            p.swap(i, self.below(i + 1));
        }
        if p.iter().enumerate().all(|(j, &i)| i == j) {
            p.rotate_left(1);
        }
        p
    }
}

/// How many arrays and objects of each kind a shuffle reordered.
type Reordered = BTreeMap<&'static str, usize>;

fn shuffle_array(v: Option<&mut Value>, what: &'static str, rng: &mut Rng, out: &mut Reordered) {
    let Some(a) = v.and_then(Value::as_array_mut) else {
        return;
    };
    if a.len() < 2 {
        return;
    }
    let before = serde_json::to_string(a).unwrap();
    let old = std::mem::take(a);
    *a = rng
        .permutation(old.len())
        .into_iter()
        .map(|i| old[i].clone())
        .collect();
    if serde_json::to_string(a).unwrap() != before {
        *out.entry(what).or_default() += 1;
    }
}

/// Reorders the keys of an object of arrays, and shuffles each array.
fn shuffle_object(
    v: Option<&mut Value>,
    keys: &'static str,
    values: &'static str,
    rng: &mut Rng,
    out: &mut Reordered,
) {
    let Some(obj) = v.and_then(Value::as_object_mut) else {
        return;
    };
    for value in obj.values_mut() {
        shuffle_array(Some(value), values, rng, out);
    }
    if obj.len() < 2 {
        return;
    }
    let before: Vec<String> = obj.keys().cloned().collect();
    let entries: Vec<(String, Value)> = std::mem::take(obj).into_iter().collect();
    let order = rng.permutation(entries.len());
    *obj = order
        .into_iter()
        .map(|i| entries[i].clone())
        .collect::<Map<_, _>>();
    if obj.keys().cloned().collect::<Vec<_>>() != before {
        *out.entry(keys).or_default() += 1;
    }
}

fn shuffle_location(loc: &mut Value, rng: &mut Rng, out: &mut Reordered) {
    shuffle_array(
        loc.get_mut("logicalLocations"),
        "location.logicalLocations",
        rng,
        out,
    );
}

/// Shuffles every collection of `doc` whose order SARIF gives no meaning: results, a result's
/// locations and code flows, a code flow's thread flows, a location's logical locations, a
/// step's kinds, and the set- and map-valued properties. Steps keep their order.
fn shuffle(doc: &mut Value, rng: &mut Rng) -> Reordered {
    let mut out = Reordered::new();
    for result in results_mut(doc) {
        if let Some(locs) = result.get_mut("locations").and_then(Value::as_array_mut) {
            for loc in locs {
                shuffle_location(loc, rng, &mut out);
            }
        }
        shuffle_array(
            result.get_mut("locations"),
            "result.locations",
            rng,
            &mut out,
        );
        if let Some(flows) = result.get_mut("codeFlows").and_then(Value::as_array_mut) {
            for flow in flows {
                if let Some(threads) = flow.get_mut("threadFlows").and_then(Value::as_array_mut) {
                    for thread in threads {
                        for step in thread["locations"].as_array_mut().unwrap() {
                            shuffle_array(step.get_mut("kinds"), "step.kinds", rng, &mut out);
                            if let Some(loc) = step.get_mut("location") {
                                shuffle_location(loc, rng, &mut out);
                            }
                        }
                    }
                }
                shuffle_array(flow.get_mut("threadFlows"), "threadFlows", rng, &mut out);
            }
        }
        shuffle_array(result.get_mut("codeFlows"), "codeFlows", rng, &mut out);
        if let Some(props) = result.get_mut("properties") {
            for key in ["taintLabels", "sourceFunctions", "sinkFunctions"] {
                shuffle_array(props.get_mut(key), key, rng, &mut out);
            }
            shuffle_object(
                props.get_mut("taintVertices"),
                "taintVertices keys",
                "taintVertices values",
                rng,
                &mut out,
            );
            shuffle_object(
                props.get_mut("taintedFormals"),
                "taintedFormals keys",
                "taintedFormals values",
                rng,
                &mut out,
            );
        }
    }
    shuffle_array(
        Some(&mut doc["runs"][0]["results"]),
        "results",
        rng,
        &mut out,
    );
    out
}

/// Applies `f` to every `logicalLocations[].index` under `v`, and returns how many it changed.
fn map_logical_refs(v: &mut Value, f: &dyn Fn(u64) -> u64) -> usize {
    let mut changed = 0;
    match v {
        Value::Object(obj) => {
            for (key, value) in obj.iter_mut() {
                if key == "logicalLocations"
                    && let Value::Array(refs) = value
                {
                    for r in refs.iter_mut() {
                        if let Some(i) = r.get("index").and_then(Value::as_u64) {
                            changed += usize::from(f(i) != i);
                            r["index"] = json!(f(i));
                        }
                    }
                }
                changed += map_logical_refs(value, f);
            }
        }
        Value::Array(items) => {
            for item in items {
                changed += map_logical_refs(item, f);
            }
        }
        _ => {}
    }
    changed
}

fn logical_count(doc: &Value) -> u64 {
    doc["runs"][0]["logicalLocations"].as_array().unwrap().len() as u64
}

/// `v` with every logical location reference under it moved `k` entries along the table of
/// `n` entries: a value that names other functions.
fn shifted(v: &Value, k: u64, n: u64) -> Value {
    let mut v = v.clone();
    map_logical_refs(&mut v, &|i| (i + k) % n);
    v
}

/// Every location under `v`: a value holding `logicalLocations`.
fn for_each_location(v: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    match v {
        Value::Object(obj) => {
            for value in obj.values_mut() {
                for_each_location(value, f);
            }
            if obj.contains_key("logicalLocations") {
                f(v);
            }
        }
        Value::Array(items) => {
            for item in items {
                for_each_location(item, f);
            }
        }
        _ => {}
    }
}

/// `doc` with a second, different element in each collection that has one, so that shuffling
/// has something to reorder at every level: each location names a second function, each
/// result has a second location, each code flow a second thread flow, each result a second
/// code flow, and each step a second kind.
fn widened(doc: &Value) -> Value {
    let mut doc = doc.clone();
    let n = logical_count(&doc);
    for result in results_mut(&mut doc) {
        for_each_location(result, &mut |loc| {
            let refs = loc["logicalLocations"].as_array_mut().unwrap();
            if let Some(i) = refs.first().and_then(|r| r["index"].as_u64()) {
                refs.push(json!({"index": (i + 1) % n}));
            }
        });
        if let Some(locs) = result.get_mut("locations").and_then(Value::as_array_mut)
            && let Some(first) = locs.first()
        {
            locs.push(shifted(first, 2, n));
        }
        if let Some(flows) = result.get_mut("codeFlows").and_then(Value::as_array_mut) {
            for flow in flows.iter_mut() {
                let threads = flow["threadFlows"].as_array_mut().unwrap();
                threads.push(shifted(&threads[0], 3, n));
                for thread in threads {
                    for step in thread["locations"].as_array_mut().unwrap() {
                        if let Some(kinds) = step.get_mut("kinds").and_then(Value::as_array_mut) {
                            let other = if kinds.contains(&json!("call")) {
                                "return"
                            } else {
                                "call"
                            };
                            kinds.push(json!(other));
                        }
                    }
                }
            }
            let first = flows[0].clone();
            flows.push(shifted(&first, 4, n));
        }
    }
    doc
}

/// The rules each profile's SARIF is expected to hold results of, by the rule id's prefix.
fn expected_rules(profile: &str) -> &'static [&'static str] {
    match profile {
        "human" => &["C0001", "C0003", "C0004"],
        "agent" => &["C0001", "C0003", "C0004", "C0007"],
        "machine" => &["C0002", "C0003", "C0004"],
        "debug" => &["C0001", "C0002", "C0003", "C0004"],
        other => panic!("no profile {other}"),
    }
}

/// Whether `profile`'s results have thread flows: the profiles with `C0001`.
fn has_flows(profile: &str) -> bool {
    expected_rules(profile).contains(&"C0001")
}

#[test]
fn profiles_have_expected_rules() {
    for (_, profile) in PROFILES {
        let doc = read(&sarif(profile));
        assert_eq!(doc["runs"].as_array().unwrap().len(), 1, "{profile}");
        let rules: BTreeSet<&str> = results(&doc)
            .iter()
            .map(|r| rule(r).split('.').next().unwrap())
            .collect();
        let expected: BTreeSet<&str> = expected_rules(profile).iter().copied().collect();
        assert_eq!(rules, expected, "{profile}");
        let flows = results(&doc).iter().filter(|r| steps(r).is_some()).count();
        assert_eq!(
            flows > 0,
            has_flows(profile),
            "{profile}: {flows} thread flows"
        );
    }
}

#[test]
fn self_identical() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        assert_identical(&a, &a, profile);
    }
}

#[test]
fn shuffled_arrays() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (_, profile) in PROFILES {
        let what = format!("{profile}: shuffled");
        let a = sarif(profile);
        let base = read(&a);
        let mut doc = base.clone();
        let reordered = shuffle(&mut doc, &mut rng);
        assert_changed(&base, &doc, &what);
        assert!(reordered.contains_key("results"), "{what}: {reordered:?}");
        assert!(
            ["taintLabels", "taintVertices keys", "taintVertices values"]
                .iter()
                .any(|k| reordered.contains_key(k)),
            "{what}: no property was reordered: {reordered:?}"
        );
        let (_tmp, b) = write(&doc);
        assert_identical(&a, &b, &what);

        // The real SARIF has one location per result, one flow and so on, so the same with
        // a second element everywhere.
        let what = format!("{profile}: widened, shuffled");
        let wide = widened(&base);
        let mut doc = wide.clone();
        let reordered = shuffle(&mut doc, &mut rng);
        assert_changed(&wide, &doc, &what);
        let mut expected = vec!["results", "result.locations", "location.logicalLocations"];
        if has_flows(profile) {
            expected.extend(["codeFlows", "threadFlows", "step.kinds"]);
        }
        for k in expected {
            assert!(
                reordered.contains_key(k),
                "{what}: no {k} reordered: {reordered:?}"
            );
        }
        let (_tmp_a, a) = write(&wide);
        let (_tmp_b, b) = write(&doc);
        assert_identical(&a, &b, &what);
    }
}

#[test]
fn renumbered_logical_locations() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for (_, profile) in PROFILES {
        let what = format!("{profile}: logicalLocations renumbered");
        let a = sarif(profile);
        let base = read(&a);
        let mut doc = base.clone();
        let table = doc["runs"][0]["logicalLocations"].as_array_mut().unwrap();
        assert!(table.len() >= 2, "{what}: {} entries", table.len());
        let p = rng.permutation(table.len());
        let old = std::mem::take(table);
        *table = p.iter().map(|&i| old[i].clone()).collect();
        let mut new_of = vec![0; p.len()];
        for (j, &i) in p.iter().enumerate() {
            new_of[i] = j as u64;
        }
        let changed = map_logical_refs(&mut doc["runs"][0]["results"], &|i| new_of[i as usize]);
        assert!(changed > 0, "{what}: no reference changed");
        assert_changed(&base, &doc, &what);
        let (_tmp, b) = write(&doc);
        assert_identical(&a, &b, &what);
    }
}

/// The instruction ids in the `call-arg` vertices of `doc`'s result properties.
fn call_arg_ids(doc: &Value) -> BTreeSet<u64> {
    let mut ids = BTreeSet::new();
    for result in results(doc) {
        let text = result["properties"].to_string();
        for (at, _) in text.match_indices("call-arg(") {
            let digits: String = text[at + "call-arg(".len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            ids.insert(digits.parse().unwrap());
        }
    }
    ids
}

/// Each debug id property's values over `doc`'s results.
fn debug_ids(doc: &Value) -> BTreeMap<&'static str, BTreeSet<u64>> {
    let mut ids: BTreeMap<&'static str, BTreeSet<u64>> = BTreeMap::new();
    for result in results(doc) {
        for key in ["fileSpanId", "funcId", "insnId"] {
            if let Some(id) = result["properties"].get(key).and_then(Value::as_u64) {
                ids.entry(key).or_default().insert(id);
            }
        }
    }
    ids
}

#[test]
fn renumbered_ids() {
    for (_, profile) in PROFILES {
        let what = format!("{profile}: functions and instructions renumbered");
        let a = sarif(profile);
        let b = renumbered().join(format!("{profile}.sarif"));
        let (da, db) = (read(&a), read(&b));
        assert_changed(&da["runs"][0]["results"], &db["runs"][0]["results"], &what);
        let ids_a = call_arg_ids(&da);
        assert!(!ids_a.is_empty(), "{what}: no call-arg vertex");
        assert_ne!(
            ids_a,
            call_arg_ids(&db),
            "{what}: call-arg ids not renumbered"
        );
        let (debug_a, debug_b) = (debug_ids(&da), debug_ids(&db));
        if profile == "debug" {
            for key in ["funcId", "insnId"] {
                assert_ne!(debug_a[key], debug_b[key], "{what}: {key} not renumbered");
            }
        } else {
            assert!(debug_a.is_empty(), "{what}: debug ids outside debug");
        }
        assert_identical(&a, &b, &what);
    }
}

/// Replaces the text of every message in `doc`, and returns how many it replaced.
fn change_messages(v: &mut Value) -> usize {
    let mut changed = 0;
    match v {
        Value::Object(obj) => {
            for (key, value) in obj.iter_mut() {
                if key == "message"
                    && let Some(text) = value.get_mut("text")
                {
                    changed += 1;
                    *text = json!(format!("edited message {changed}"));
                } else {
                    changed += change_messages(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                changed += change_messages(item);
            }
        }
        _ => {}
    }
    changed
}

#[test]
fn changed_messages() {
    for (_, profile) in PROFILES {
        let what = format!("{profile}: messages changed");
        let a = sarif(profile);
        let base = read(&a);
        let mut doc = base.clone();
        let changed = change_messages(&mut doc["runs"][0]["results"]);
        assert!(
            changed >= results(&base).len(),
            "{what}: {changed} messages"
        );
        assert_changed(&base, &doc, &what);
        let (_tmp, b) = write(&doc);
        assert_identical(&a, &b, &what);
    }
}

/// Deleting any one result is reported as that result, once more in A.
#[test]
fn deleted_result() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        for i in 0..results(&base).len() {
            let what = format!("{profile}: result {i} deleted");
            let mut doc = base.clone();
            results_mut(&mut doc).remove(i);
            let (_tmp, b) = write(&doc);
            let (in_a, _) = assert_reported(&diff(&a, &b, false), 1, 0, &what);
            assert_eq!(in_a[0].0, in_a[0].1 + 1, "{what}");
        }
    }
}

/// Duplicating any one result is reported as that result, once more in B. A result whose
/// canonical form is unique in its file is `[A 1, B 2]`.
#[test]
fn duplicated_result() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let mut unique = 0;
        for i in 0..results(&base).len() {
            let what = format!("{profile}: result {i} duplicated");
            let mut doc = base.clone();
            let copy = results(&doc)[i].clone();
            results_mut(&mut doc).push(copy);
            let (_tmp, b) = write(&doc);
            let (_, in_b) = assert_reported(&diff(&a, &b, false), 0, 1, &what);
            assert_eq!(in_b[0].1, in_b[0].0 + 1, "{what}");
            unique += usize::from(in_b[0] == (1, 2));
        }
        assert!(unique > 0, "{profile}: no result is unique");
    }
}

/// Swapping the first and last steps of any thread flow is reported as that result changed.
#[test]
fn swapped_steps() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let mut swapped = 0;
        for i in 0..results(&base).len() {
            let what = format!("{profile}: steps of result {i} swapped");
            let mut doc = base.clone();
            let Some(steps) = steps_mut(&mut results_mut(&mut doc)[i]) else {
                continue;
            };
            let last = steps.len() - 1;
            assert_ne!(step_key(&steps[0]), step_key(&steps[last]), "{what}");
            steps.swap(0, last);
            renumber_steps(steps);
            assert_changed(&base, &doc, &what);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(&a, &b, false), 1, 1, &what);
            swapped += 1;
        }
        assert_eq!(
            swapped > 0,
            has_flows(profile),
            "{profile}: {swapped} flows"
        );
    }
}

/// Moving any result's first location down a line is reported as that result changed.
#[test]
fn changed_line() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let mut moved = 0;
        for i in 0..results(&base).len() {
            let what = format!("{profile}: line of result {i} changed");
            let mut doc = base.clone();
            let result = &mut results_mut(&mut doc)[i];
            let Some(line) = result.pointer_mut("/locations/0/physicalLocation/region/startLine")
            else {
                continue;
            };
            *line = json!(line.as_u64().unwrap() + 1);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(&a, &b, false), 1, 1, &what);
            moved += 1;
        }
        assert!(moved > 0, "{profile}: no result has a physical location");
    }
}

/// With the endpoints rule, a change to a thread flow's interior steps reports nothing, and a
/// deleted result still does.
///
/// `ties.c`'s flows have at most three steps, so one interior step. Each is checked two
/// ways: replaced by another flow's differing interior step, and, in a flow lengthened to
/// four steps by inserting that other step, swapped with it.
#[test]
fn endpoints_only() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let interiors: Vec<Value> = results(&base)
            .iter()
            .filter_map(steps)
            .filter(|s| s.len() >= 3)
            .flat_map(|s| s[1..s.len() - 1].iter().cloned())
            .collect();
        let mut checked = 0;
        for i in 0..results(&base).len() {
            if rule(&results(&base)[i]) != ENDPOINTS_RULE {
                continue;
            }
            let what = format!("{profile}: result {i} deleted");
            let mut doc = base.clone();
            results_mut(&mut doc).remove(i);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(&a, &b, true), 1, 0, &what);

            let flow = steps(&results(&base)[i]).unwrap();
            if flow.len() < 3 {
                continue;
            }
            let interior = &flow[1];
            let other = interiors
                .iter()
                .find(|s| step_key(s) != step_key(interior))
                .unwrap_or_else(|| panic!("{profile}: every interior step is the same"));

            let what = format!("{profile}: interior step of result {i} replaced");
            let mut doc = base.clone();
            let steps = steps_mut(&mut results_mut(&mut doc)[i]).unwrap();
            steps[1] = other.clone();
            renumber_steps(steps);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(&a, &b, false), 1, 1, &what);
            assert_identical_endpoints(&a, &b, &what);

            let what = format!("{profile}: interior steps of result {i} swapped");
            let mut longer = base.clone();
            let steps = steps_mut(&mut results_mut(&mut longer)[i]).unwrap();
            steps.insert(2, other.clone());
            renumber_steps(steps);
            let mut doc = longer.clone();
            let steps = steps_mut(&mut results_mut(&mut doc)[i]).unwrap();
            steps.swap(1, 2);
            renumber_steps(steps);
            assert_changed(&longer, &doc, &what);
            let (_tmp_a, la) = write(&longer);
            let (_tmp_b, lb) = write(&doc);
            assert_reported(&diff(&la, &lb, false), 1, 1, &what);
            assert_identical_endpoints(&la, &lb, &what);
            checked += 1;
        }
        assert_eq!(
            checked > 0,
            has_flows(profile),
            "{profile}: {checked} flows"
        );
    }
}

fn assert_identical_endpoints(a: &Path, b: &Path, what: &str) {
    let report = diff(a, b, true);
    assert!(
        report.identical(),
        "{what} (endpoints only):\n{}",
        report.to_text()
    );
}
