//! `diff-query` on SARIF from real `ctadl query` runs. An edit that changes nothing compared
//! (order where SARIF gives it none, including the order of execution notifications; the
//! numbering of `run.logicalLocations`; instruction and function ids, also inside a step's
//! `taintVertex`; message text, and a notification's message arguments) reports nothing. An
//! edit that changes something compared (a result, a step's `taintVertex` or `callee`, a
//! notification's level or property, an invocation's `executionSuccessful` or `exitCode`) is
//! reported as exactly what it changed, and nothing else.
//!
//! The fixture is `ties.c` indexed with `ties.json` and queried in every profile. Its interior
//! steps are all summarized, so the tests of step properties also run on a Flowy fixture whose
//! interior steps have a `callee`. The step properties are the ones `ctadl query` wrote, and
//! every query checks each step of every flow it writes. Each test edits that SARIF as JSON, or
//! queries a program indexed with other ids, and first checks that the edit changed something,
//! so an edit that is a no-op cannot pass. A program whose one call site has two callees is
//! indexed twice with the two callees' ids in opposite orders.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ctadl_ascent::cli;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use ctadl_query_diff::{ENDPOINTS_RULE, Options, Report, diff_query};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use crate::common::{self, store_dir};

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

/// Queries `project` with `models` in every profile, writing `<profile>.sarif` into a new
/// directory `out` in the store. The query checks every step of every flow it writes.
fn query_all(project: &AnalysisProject, models: &[PathBuf], out: &str) -> PathBuf {
    let dir = store_dir().join(out);
    std::fs::create_dir_all(&dir).unwrap();
    for (profile, name) in PROFILES {
        let sarif = dir.join(format!("{name}.sarif"));
        common::query(project, models, &sarif, profile, None).unwrap();
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
        query_all(&project, &models, "diff_query_ties")
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
        query_all(&project, &models, "diff_query_ties_renumbered")
    })
}

/// Imports the Flowy program `src` as `name`.
fn import_flowy(name: &str, src: &Path) {
    store_dir();
    let import = ArtifactImport::try_create(name, ArtifactLanguage::Flowy, src).unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();
}

/// Indexes the project `<name>_proj` of `imports`, which hold their own sources and sinks, and
/// queries it in every profile into the directory `name` in the store.
fn index_flowy(name: &str, imports: &[&str]) -> PathBuf {
    let project =
        AnalysisProject::try_create(&format!("{name}_proj"), imports, SubImports::All).unwrap();
    cli::index(&project, &[], &[], false, cli::IndexOptions::default()).unwrap();
    query_all(&project, &[], name)
}

fn tnt_fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "tnt", name]
        .iter()
        .collect()
}

/// The fixture [`one_frame_down`] queries: a function pointer called one frame below `main`,
/// which resolves to two functions. Each of its flows enters `get` and then one of the two, so
/// its interior step has a `callee`.
const ONE_FRAME_DOWN: &str = "funcptr_one_frame_down.tnt";

/// The directory of the SARIF of [`ONE_FRAME_DOWN`] in every profile.
fn one_frame_down() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let name = "diff_query_one_frame_down";
        import_flowy(name, &tnt_fixture(ONE_FRAME_DOWN));
        index_flowy(name, &[name])
    })
}

/// The import of `tests/tnt/step_props_two_callees.tnt`, whose one indirect call resolves to
/// `get_a` and `get_b`.
const TWO_CALLEES: &str = "diff_query_two_callees";

/// A program that mentions the two callees of [`TWO_CALLEES`] in the opposite order to the
/// fixture's, without defining or calling them. A function's id is given the first time any
/// import of the project mentions its name, in the order the project lists its imports.
const PAD_TNT: &str =
    "def pad(o) {\nstart:\n  o.x = ptr<get_b>;\n  o.y = ptr<get_a>;\n  return;\n}\n";

/// The directories of the SARIF, in every profile, of [`TWO_CALLEES`] indexed alone and
/// indexed after [`PAD_TNT`], so that `get_a` and `get_b` take ids in opposite orders. The
/// fixture and its import are the same in both, so every location and name is too.
fn two_callees() -> &'static (PathBuf, PathBuf) {
    static DIRS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    DIRS.get_or_init(|| {
        import_flowy(TWO_CALLEES, &tnt_fixture("step_props_two_callees.tnt"));
        let pad = "diff_query_two_callees_pad";
        let src = store_dir().join(format!("{pad}.tnt"));
        std::fs::write(&src, PAD_TNT).unwrap();
        import_flowy(pad, &src);
        (
            index_flowy("diff_query_two_callees_alone", &[TWO_CALLEES]),
            index_flowy("diff_query_two_callees_padded", &[pad, TWO_CALLEES]),
        )
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

/// The execution notifications of `doc`'s only invocation.
fn notifications(doc: &Value) -> &Vec<Value> {
    doc["runs"][0]["invocations"][0]["toolExecutionNotifications"]
        .as_array()
        .unwrap()
}

fn notifications_mut(doc: &mut Value) -> &mut Vec<Value> {
    doc["runs"][0]["invocations"][0]["toolExecutionNotifications"]
        .as_array_mut()
        .unwrap()
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

/// Every step of every thread flow of every result of `doc`, in file order.
fn for_each_step(doc: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    for result in results_mut(doc) {
        let Some(flows) = result.get_mut("codeFlows").and_then(Value::as_array_mut) else {
            continue;
        };
        for flow in flows {
            for thread in flow["threadFlows"].as_array_mut().unwrap() {
                for step in thread["locations"].as_array_mut().unwrap() {
                    f(step);
                }
            }
        }
    }
}

/// A SARIF file a test edits, and its contents.
struct Base {
    /// What the file is: its program and profile.
    name: String,
    /// Its profile.
    profile: &'static str,
    path: PathBuf,
    doc: Value,
}

impl Base {
    /// The SARIF in `profile` in `dir`, one of the directories [`query_all`] writes, of the
    /// program `program`.
    fn new(dir: &Path, program: &str, profile: &'static str) -> Self {
        let path = dir.join(format!("{profile}.sarif"));
        let doc = read(&path);
        Base {
            name: format!("{program} {profile}"),
            profile,
            path,
            doc,
        }
    }
}

/// The SARIF of `ties.c` in `profile`.
fn base(profile: &'static str) -> Base {
    Base::new(ties(), "ties.c", profile)
}

/// The files the tests of step properties edit, in every profile: `ties.c`'s, whose interior
/// steps are summarized, and [`one_frame_down`]'s, whose interior steps have a `callee`.
fn step_bases() -> Vec<Base> {
    let mut bases = Vec::new();
    for (_, profile) in PROFILES {
        bases.push(base(profile));
        bases.push(Base::new(one_frame_down(), ONE_FRAME_DOWN, profile));
    }
    bases
}

/// `vertex`, a `call-arg(<id>, <formal>)...` vertex, with the next formal, where the return
/// is formal -1.
fn next_formal(vertex: &str) -> String {
    let (head, tail) = vertex
        .split_once(", ")
        .unwrap_or_else(|| panic!("{vertex} is not a call-arg vertex"));
    let (formal, rest) = tail.split_once(')').unwrap();
    let formal: i64 = formal.parse().unwrap();
    format!("{head}, {}){rest}", (formal + 1) % 3)
}

/// `s` with the instruction id of each `call-arg(<id>,` in it increased by `by`.
fn shift_call_args(s: &str, by: u64) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("call-arg(") {
        let after = at + "call-arg(".len();
        out.push_str(&rest[..after]);
        let digits: String = rest[after..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        out.push_str(&(digits.parse::<u64>().unwrap() + by).to_string());
        rest = &rest[after + digits.len()..];
    }
    out.push_str(rest);
    out
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

/// How many differing rows `report` has, more in A then more in B, for results, then
/// notifications, then invocation statuses.
fn totals(report: &Report) -> [usize; 6] {
    [
        report.more_in_a.total(),
        report.more_in_b.total(),
        report.notifications_more_in_a.total(),
        report.notifications_more_in_b.total(),
        report.invocations_more_in_a.total(),
        report.invocations_more_in_b.total(),
    ]
}

fn counts(s: &ctadl_query_diff::Samples) -> Counts {
    s.rows.iter().map(|r| (r.count_a, r.count_b)).collect()
}

/// Asserts that `report` has `in_a` results more often in A and `in_b` more often in B, and
/// no differing notification or invocation status, and returns each side's counts.
fn assert_reported(report: &Report, in_a: usize, in_b: usize, what: &str) -> (Counts, Counts) {
    let text = report.to_text();
    assert_eq!(totals(report), [in_a, in_b, 0, 0, 0, 0], "{what}:\n{text}");
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
/// step's kinds, the set- and map-valued properties, and the execution notifications. Steps
/// keep their order.
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
    shuffle_array(
        doc.pointer_mut("/runs/0/invocations/0/toolExecutionNotifications"),
        "notifications",
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
        let Base {
            name,
            path: a,
            doc: base,
            ..
        } = &base(profile);
        let what = format!("{name}: shuffled");
        let mut doc = base.clone();
        let reordered = shuffle(&mut doc, &mut rng);
        assert_changed(base, &doc, &what);
        for k in ["results", "notifications"] {
            assert!(
                reordered.contains_key(k),
                "{what}: no {k} reordered: {reordered:?}"
            );
        }
        assert!(
            ["taintLabels", "taintVertices keys", "taintVertices values"]
                .iter()
                .any(|k| reordered.contains_key(k)),
            "{what}: no property was reordered: {reordered:?}"
        );
        let (_tmp, b) = write(&doc);
        assert_identical(a, &b, &what);

        // The real SARIF has one location per result, one flow and so on, so the same
        // with a second element everywhere.
        let what = format!("{name}: widened, shuffled");
        let wide = widened(base);
        let mut doc = wide.clone();
        let reordered = shuffle(&mut doc, &mut rng);
        assert_changed(&wide, &doc, &what);
        let mut expected = vec![
            "results",
            "result.locations",
            "location.logicalLocations",
            "notifications",
        ];
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
        let Base {
            name,
            path: a,
            doc: base,
            ..
        } = &base(profile);
        let what = format!("{name}: logicalLocations renumbered");
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
        assert_changed(base, &doc, &what);
        let (_tmp, b) = write(&doc);
        assert_identical(a, &b, &what);
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

/// The function ids `doc`, a `debug` profile file, gives each function that has a
/// `C0002` result, by the function's name.
fn function_ids(doc: &Value) -> BTreeMap<String, BTreeSet<u64>> {
    let names = doc["runs"][0]["logicalLocations"].as_array().unwrap();
    let mut ids: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for result in results(doc) {
        if !rule(result).starts_with("C0002") {
            continue;
        }
        let index = result["locations"][0]["logicalLocations"][0]["index"]
            .as_u64()
            .unwrap();
        let name = names[index as usize]["name"].as_str().unwrap();
        let id = result["properties"]["funcId"].as_u64().unwrap();
        ids.entry(name.to_string()).or_default().insert(id);
    }
    ids
}

/// The one function id of `name` in `ids`.
fn function_id(ids: &BTreeMap<String, BTreeSet<u64>>, name: &str, what: &str) -> u64 {
    let found = ids
        .get(name)
        .unwrap_or_else(|| panic!("{what}: no id for {name}: {ids:?}"));
    assert_eq!(found.len(), 1, "{what}: {name} has ids {found:?}");
    *found.first().unwrap()
}

/// Each result's rule, location and `sourceFunctions` and `sinkFunctions`, as written, sorted.
fn result_functions(doc: &Value) -> Vec<String> {
    let mut out: Vec<String> = results(doc)
        .iter()
        .map(|r| {
            json!([
                r["ruleId"],
                r["locations"][0]["physicalLocation"],
                r["properties"].get("sourceFunctions"),
                r["properties"].get("sinkFunctions"),
            ])
            .to_string()
        })
        .collect();
    out.sort();
    out
}

/// The program of [`TWO_CALLEES`], whose one indirect call resolves to two functions, indexed
/// with the two in opposite orders of id, compares identical in every profile, and each result
/// names the same functions.
#[test]
fn renumbered_callee_ids() {
    let (alone, padded) = two_callees();
    let what = "two callees renumbered";
    let ids_a = function_ids(&read(&alone.join("debug.sarif")));
    let ids_b = function_ids(&read(&padded.join("debug.sarif")));
    let order = |ids| function_id(ids, "get_a", what).cmp(&function_id(ids, "get_b", what));
    assert_eq!(
        order(&ids_a),
        order(&ids_b).reverse(),
        "{what}: the callees' ids are not in opposite orders: {ids_a:?}, {ids_b:?}"
    );
    assert_ne!(order(&ids_a), std::cmp::Ordering::Equal, "{what}");

    for (_, profile) in PROFILES {
        let what = format!("{profile}: {what}");
        let a = alone.join(format!("{profile}.sarif"));
        let b = padded.join(format!("{profile}.sarif"));
        let (da, db) = (read(&a), read(&b));
        assert_changed(&da["runs"][0]["results"], &db["runs"][0]["results"], &what);
        let functions = result_functions(&da);
        assert_eq!(functions, result_functions(&db), "{what}");
        if has_flows(profile) {
            assert!(
                results(&da)
                    .iter()
                    .any(|r| r["properties"]["sinkFunctions"] == json!(["get_a", "get_b"])),
                "{what}: no result at the call with two callees"
            );
            assert!(
                results(&da)
                    .iter()
                    .filter_map(steps)
                    .flatten()
                    .any(|s| s["properties"].get("callee").is_some()),
                "{what}: no step has a callee"
            );
        }
        assert_identical(&a, &b, &what);
    }
}

/// Replaces the text and the arguments of every message in `doc`, and returns how many
/// messages it changed.
fn change_messages(v: &mut Value) -> usize {
    let mut changed = 0;
    match v {
        Value::Object(obj) => {
            for (key, value) in obj.iter_mut() {
                if key == "message"
                    && (value.get("text").is_some() || value.get("arguments").is_some())
                {
                    changed += 1;
                    if let Some(text) = value.get_mut("text") {
                        *text = json!(format!("edited message {changed}"));
                    }
                    if let Some(args) = value.get_mut("arguments") {
                        *args = json!([format!("edited argument {changed}")]);
                    }
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
        let Base {
            name,
            path: a,
            doc: base,
            ..
        } = &base(profile);
        let what = format!("{name}: messages changed");
        let mut doc = base.clone();
        let changed = change_messages(&mut doc["runs"][0]);
        assert!(
            changed >= results(base).len() + notifications(base).len(),
            "{what}: {changed} messages"
        );
        assert_changed(base, &doc, &what);
        assert_changed(
            &base["runs"][0]["invocations"],
            &doc["runs"][0]["invocations"],
            &format!("{what}, under invocations"),
        );
        let arguments = |d: &Value| -> Vec<Value> {
            notifications(d)
                .iter()
                .map(|n| n["message"]["arguments"].clone())
                .collect()
        };
        assert_ne!(
            arguments(base),
            arguments(&doc),
            "{what}: no notification's arguments changed"
        );
        let (_tmp, b) = write(&doc);
        assert_identical(a, &b, &what);
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
        let Base {
            name,
            path: a,
            doc: base,
            ..
        } = &base(profile);
        let mut swapped = 0;
        for i in 0..results(base).len() {
            let what = format!("{name}: steps of result {i} swapped");
            let mut doc = base.clone();
            let Some(steps) = steps_mut(&mut results_mut(&mut doc)[i]) else {
                continue;
            };
            let last = steps.len() - 1;
            assert_ne!(step_key(&steps[0]), step_key(&steps[last]), "{what}");
            steps.swap(0, last);
            renumber_steps(steps);
            assert_changed(base, &doc, &what);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(a, &b, false), 1, 1, &what);
            swapped += 1;
        }
        assert_eq!(swapped > 0, has_flows(profile), "{name}: {swapped} flows");
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

/// `doc` with `edit` applied to the properties of step `s` of result `i`.
fn with_step_edit(doc: &Value, i: usize, s: usize, edit: StepEdit) -> Value {
    let mut doc = doc.clone();
    let steps = steps_mut(&mut results_mut(&mut doc)[i]).unwrap();
    edit(steps[s]["properties"].as_object_mut().unwrap());
    doc
}

/// An edit of a step's properties.
type StepEdit = fn(&mut Map<String, Value>);

/// The edits of one step property each that change what is compared, by the property each
/// needs: the formal of `taintVertex` changed, the name of `callee` changed, `summarized`
/// removed.
const STEP_EDITS: [(&str, StepEdit); 3] = [
    ("taintVertex", |p| {
        let vertex = next_formal(p["taintVertex"].as_str().unwrap());
        p.insert("taintVertex".into(), json!(vertex));
    }),
    ("callee", |p| {
        let callee = format!("{}_changed", p["callee"].as_str().unwrap());
        p.insert("callee".into(), json!(callee));
    }),
    ("summarized", |p| {
        p.remove("summarized");
    }),
];

/// Asserts that every step property was edited somewhere: `edited` counts the edits of each.
fn assert_every_property_edited(edited: &BTreeMap<&str, usize>) {
    for (key, _) in STEP_EDITS {
        assert!(
            edited.get(key).is_some_and(|&n| n > 0),
            "no step has {key}: {edited:?}"
        );
    }
}

/// With the endpoints rule, a change to a thread flow's interior steps reports nothing, and a
/// deleted result still does. A changed interior `taintVertex` or `callee`, or a removed
/// interior `summarized`, reports nothing with the rule and is reported without it; a changed
/// last-step `taintVertex` is reported with it.
///
/// The flows have at most three steps, so one interior step. Each is checked two ways:
/// replaced by another flow's differing interior step, and, in a flow lengthened to four steps
/// by inserting that other step, swapped with it.
#[test]
fn endpoints_only() {
    let mut edited = BTreeMap::new();
    for Base {
        name,
        profile,
        path: a,
        doc: base,
    } in &step_bases()
    {
        let interiors: Vec<Value> = results(base)
            .iter()
            .filter_map(steps)
            .filter(|s| s.len() >= 3)
            .flat_map(|s| s[1..s.len() - 1].iter().cloned())
            .collect();
        let mut checked = 0;
        for i in 0..results(base).len() {
            if rule(&results(base)[i]) != ENDPOINTS_RULE {
                continue;
            }
            let what = format!("{name}: result {i} deleted");
            let mut doc = base.clone();
            results_mut(&mut doc).remove(i);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(a, &b, true), 1, 0, &what);

            let flow = steps(&results(base)[i]).unwrap();
            if flow.len() < 3 {
                continue;
            }
            let interior = &flow[1];
            let other = interiors
                .iter()
                .find(|s| step_key(s) != step_key(interior))
                .unwrap_or_else(|| panic!("{name}: every interior step is the same"));

            let what = format!("{name}: interior step of result {i} replaced");
            let mut doc = base.clone();
            let steps = steps_mut(&mut results_mut(&mut doc)[i]).unwrap();
            steps[1] = other.clone();
            renumber_steps(steps);
            assert_changed(base, &doc, &what);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(a, &b, false), 1, 1, &what);
            assert_identical_endpoints(a, &b, &what);

            let what = format!("{name}: interior steps of result {i} swapped");
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

            for (key, edit) in STEP_EDITS {
                if interior["properties"].get(key).is_none() {
                    continue;
                }
                let what = format!("{name}: interior {key} of result {i} edited");
                let doc = with_step_edit(base, i, 1, edit);
                assert_changed(base, &doc, &what);
                let (_tmp, b) = write(&doc);
                assert_reported(&diff(a, &b, false), 1, 1, &what);
                assert_identical_endpoints(a, &b, &what);
                *edited.entry(key).or_default() += 1;
            }

            let what = format!("{name}: last taintVertex of result {i} changed");
            let doc = with_step_edit(base, i, flow.len() - 1, STEP_EDITS[0].1);
            assert_changed(base, &doc, &what);
            let (_tmp, b) = write(&doc);
            assert_reported(&diff(a, &b, true), 1, 1, &what);
            checked += 1;
        }
        assert_eq!(checked > 0, has_flows(profile), "{name}: {checked} flows");
    }
    assert_every_property_edited(&edited);
}

fn assert_identical_endpoints(a: &Path, b: &Path, what: &str) {
    let report = diff(a, b, true);
    assert!(
        report.identical(),
        "{what} (endpoints only):\n{}",
        report.to_text()
    );
}

/// Asserts that `report` differs in one row more in A and one more in B of the section whose
/// totals start at `at` in [`totals`], and in nothing else, and returns the two rows' counts.
fn assert_one_row_each_side(report: &Report, at: usize, what: &str) -> [(usize, usize); 2] {
    let mut expected = [0; 6];
    expected[at] = 1;
    expected[at + 1] = 1;
    assert_eq!(totals(report), expected, "{what}:\n{}", report.to_text());
    let (in_a, in_b) = match at {
        2 => (
            &report.notifications_more_in_a,
            &report.notifications_more_in_b,
        ),
        4 => (&report.invocations_more_in_a, &report.invocations_more_in_b),
        _ => (&report.more_in_a, &report.more_in_b),
    };
    [counts(in_a)[0], counts(in_b)[0]]
}

/// Changing any execution notification's level, or a number in its properties, is reported as
/// that notification changed. Some notifications are equal once their message is dropped, so
/// the changed one may be held more than once by A.
#[test]
fn changed_notification() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let mut with_number = 0;
        for j in 0..notifications(&base).len() {
            let mut edits: Vec<(String, Value)> = Vec::new();

            let mut doc = base.clone();
            let n = &mut notifications_mut(&mut doc)[j];
            let level = if n["level"] == "warning" {
                "note"
            } else {
                "warning"
            };
            n["level"] = json!(level);
            edits.push((format!("{profile}: notification {j} level {level}"), doc));

            let mut doc = base.clone();
            let props = notifications_mut(&mut doc)[j]
                .get_mut("properties")
                .and_then(Value::as_object_mut);
            if let Some((key, value)) = props.and_then(|p| p.iter_mut().find(|(_, v)| v.is_u64())) {
                let what = format!("{profile}: notification {j} property {key} increased");
                *value = json!(value.as_u64().unwrap() + 1);
                edits.push((what, doc));
                with_number += 1;
            }

            for (what, doc) in edits {
                assert_changed(&base, &doc, &what);
                let (_tmp, b) = write(&doc);
                for endpoints_only in [false, true] {
                    let [in_a, in_b] =
                        assert_one_row_each_side(&diff(&a, &b, endpoints_only), 2, &what);
                    assert_eq!(in_a.0, in_a.1 + 1, "{what}");
                    assert_eq!(in_b.1, in_b.0 + 1, "{what}");
                }
            }
        }
        assert!(
            with_number > 0,
            "{profile}: no notification has a number property"
        );
    }
}

/// Changing the invocation's `executionSuccessful` or `exitCode`, or removing `exitCode`, is
/// reported as that status changed.
#[test]
fn changed_status() {
    for (_, profile) in PROFILES {
        let a = sarif(profile);
        let base = read(&a);
        let edits: [(&str, fn(&mut Map<String, Value>)); 3] = [
            ("executionSuccessful false", |inv| {
                inv.insert("executionSuccessful".into(), json!(false));
            }),
            ("exitCode 1", |inv| {
                inv.insert("exitCode".into(), json!(1));
            }),
            ("exitCode removed", |inv| {
                inv.remove("exitCode");
            }),
        ];
        for (edit, f) in edits {
            let what = format!("{profile}: {edit}");
            let mut doc = base.clone();
            f(doc["runs"][0]["invocations"][0].as_object_mut().unwrap());
            assert_changed(&base, &doc, &what);
            let (_tmp, b) = write(&doc);
            for endpoints_only in [false, true] {
                let rows = assert_one_row_each_side(&diff(&a, &b, endpoints_only), 4, &what);
                assert_eq!(rows, [(1, 0), (0, 1)], "{what}");
            }
        }
    }
}

/// Changing the formal of a flow's first `taintVertex` or the name of its first `callee`, or
/// removing its first `summarized`, is reported as that result changed, and with the endpoints
/// rule too when that step is the flow's first or last; renumbering the instruction ids in the
/// step properties reports nothing.
#[test]
fn changed_step_property() {
    let mut edited = BTreeMap::new();
    for Base {
        name,
        profile,
        path: a,
        doc: base,
    } in &step_bases()
    {
        let mut checked = 0;
        for i in 0..results(base).len() {
            let Some(flow) = steps(&results(base)[i]) else {
                continue;
            };
            for (key, edit) in STEP_EDITS {
                let Some(s) = flow
                    .iter()
                    .position(|step| step["properties"].get(key).is_some())
                else {
                    continue;
                };
                let what = format!("{name}: {key} of step {s} of result {i} edited");
                let doc = with_step_edit(base, i, s, edit);
                assert_changed(base, &doc, &what);
                let (_tmp, b) = write(&doc);
                assert_reported(&diff(a, &b, false), 1, 1, &what);
                if s == 0 || s == flow.len() - 1 {
                    assert_reported(&diff(a, &b, true), 1, 1, &what);
                } else {
                    assert_identical_endpoints(a, &b, &what);
                }
                *edited.entry(key).or_default() += 1;
            }
            checked += 1;
        }
        assert_eq!(checked > 0, has_flows(profile), "{name}: {checked} flows");

        let what = format!("{name}: instruction ids in step properties renumbered");
        let mut doc = base.clone();
        for_each_step(&mut doc, &mut |step| {
            if let Some(props) = step.get_mut("properties").and_then(Value::as_object_mut) {
                for value in props.values_mut() {
                    if let Some(s) = value.as_str() {
                        *value = json!(shift_call_args(s, 7));
                    }
                }
            }
        });
        if has_flows(profile) {
            assert_changed(base, &doc, &what);
            let (_tmp, b) = write(&doc);
            assert_identical(a, &b, &what);
        }
    }
    assert_every_property_edited(&edited);
}
