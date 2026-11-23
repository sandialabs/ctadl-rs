use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::driver::diff_with;
use crate::{Atoms, CanonError, ErrorKind, Options, Report, diff_query, read_runs};

/// A step at `line` of `a.c`, in the function at `index` of the run's table.
fn step(order: i64, line: i64, index: usize) -> Value {
    json!({
        "executionOrder": order,
        "kinds": ["taint"],
        "location": {
            "physicalLocation": {
                "artifactLocation": {"uri": "a.c", "uriBaseId": "SRC0"},
                "region": {"startLine": line}
            },
            "logicalLocations": [{"index": index}]
        }
    })
}

/// A `C0001.tainted-path` result whose flow visits `lines`, all in `main`.
fn flow(lines: &[i64]) -> Value {
    let steps: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(i, &l)| step(i as i64 + 1, l, 0))
        .collect();
    json!({
        "ruleId": "C0001.tainted-path",
        "kind": "fail",
        "level": "error",
        "message": {"text": "a flow"},
        "locations": [{
            "physicalLocation": {
                "artifactLocation": {"uri": "a.c", "uriBaseId": "SRC0"},
                "region": {"startLine": lines[0]}
            },
            "logicalLocations": [{"index": 0}]
        }],
        "codeFlows": [{"threadFlows": [{"locations": steps}]}],
        "properties": {
            "taintLabels": ["L"],
            "sourceFunctions": ["src"],
            "sinkFunctions": ["snk"]
        }
    })
}

/// A `C0003.taint-source` result in `main`, at `line`.
fn source(line: i64) -> Value {
    json!({
        "ruleId": "C0003.taint-source",
        "kind": "informational",
        "level": "none",
        "message": {"text": "a source"},
        "locations": [{
            "physicalLocation": {
                "artifactLocation": {"uri": "a.c", "uriBaseId": "SRC0"},
                "region": {"startLine": line}
            },
            "logicalLocations": [{"index": 0}]
        }],
        "properties": {"taintLabels": ["L"], "taintVertex": "call-arg(12, 0)"}
    })
}

fn doc(results: Vec<Value>) -> Value {
    json!({
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {"name": "ctadl"}},
            "logicalLocations": [{"kind": "function", "name": "main", "fullyQualifiedName": "main"}],
            "results": results
        }]
    })
}

/// `d` with `invocations` as its run's invocations.
fn invoked(mut d: Value, invocations: Vec<Value>) -> Value {
    d["runs"][0]["invocations"] = json!(invocations);
    d
}

/// An invocation with status `ok` and `exit_code`, and execution `notifications`.
fn invocation(ok: bool, exit_code: Option<i64>, notifications: Vec<Value>) -> Value {
    let mut i = json!({"executionSuccessful": ok, "toolExecutionNotifications": notifications});
    if let Some(c) = exit_code {
        i["exitCode"] = json!(c);
    }
    i
}

/// An execution notification `id` at `level` about `function`, which is named only in the
/// message's text and arguments.
fn notification(id: &str, level: &str, function: &str) -> Value {
    json!({
        "descriptor": {"id": id},
        "level": level,
        "message": {
            "id": "call-without-body",
            "arguments": [function],
            "text": format!("{function} has no body")
        },
        "properties": {"calls": 3}
    })
}

fn base() -> Vec<Value> {
    vec![flow(&[3, 5, 7]), source(3), source(9)]
}

struct Files(TempDir);

impl Files {
    fn new() -> Self {
        Files(tempfile::tempdir().unwrap())
    }

    fn write(&self, name: &str, v: &Value) -> PathBuf {
        let p = self.0.path().join(name);
        std::fs::write(&p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
        p
    }
}

fn diff(a: &Path, b: &Path) -> Report {
    diff_query(a, b, &Options::default()).unwrap()
}

type Counts = (Vec<(usize, usize)>, Vec<(usize, usize)>);

fn pair_counts(a: &crate::Samples, b: &crate::Samples) -> Counts {
    let c = |s: &crate::Samples| s.rows.iter().map(|r| (r.count_a, r.count_b)).collect();
    (c(a), c(b))
}

fn counts(r: &Report) -> Counts {
    pair_counts(&r.more_in_a, &r.more_in_b)
}

fn notification_counts(r: &Report) -> Counts {
    pair_counts(&r.notifications_more_in_a, &r.notifications_more_in_b)
}

fn invocation_counts(r: &Report) -> Counts {
    pair_counts(&r.invocations_more_in_a, &r.invocations_more_in_b)
}

#[test]
fn identical_self() {
    let f = Files::new();
    let a = f.write("a.sarif", &doc(base()));
    let r = diff(&a, &a);
    assert!(r.identical());
    assert_eq!(r.results_a, 3);
    assert!(r.to_text().starts_with("IDENTICAL\n"));
}

#[test]
fn reordered_and_renumbered_is_identical() {
    let f = Files::new();
    let a = invoked(
        doc(base()),
        vec![
            invocation(
                true,
                Some(0),
                vec![
                    notification("N1", "warning", "f"),
                    notification("N2", "note", "g"),
                ],
            ),
            invocation(true, Some(0), vec![]),
        ],
    );
    let a = f.write("a.sarif", &a);
    // Results reversed, messages changed, and a second table entry in front of `main`, with
    // every reference renumbered to match. The notifications are reversed, their message text
    // and arguments changed, and split over the two invocations, whose statuses are equal.
    let mut results = base();
    results.reverse();
    let mut b = invoked(
        doc(results),
        vec![
            invocation(true, Some(0), vec![notification("N2", "note", "h")]),
            invocation(true, Some(0), vec![notification("N1", "warning", "k")]),
        ],
    );
    b["runs"][0]["logicalLocations"] = json!([{"name": "other"}, {"name": "main"}]);
    let mut text = b.to_string().replace("\"index\":0", "\"index\":1");
    text = text.replace("a flow", "another flow");
    let b = f.write("b.sarif", &serde_json::from_str(&text).unwrap());
    let r = diff(&a, &b);
    assert!(r.identical(), "{}", r.to_text());
    assert_eq!((r.notifications_a, r.notifications_b), (2, 2));
    assert_eq!((r.invocations_a, r.invocations_b), (2, 2));
}

#[test]
fn deleted_result() {
    let f = Files::new();
    let a = f.write("a.sarif", &doc(base()));
    let mut fewer = base();
    fewer.pop();
    let b = f.write("b.sarif", &doc(fewer));
    let r = diff(&a, &b);
    assert!(!r.identical());
    assert_eq!(counts(&r), (vec![(1, 0)], vec![]));
    assert!(
        r.to_text()
            .contains("more in A: 1 results, 0 not shown\n  [A 1, B 0] ")
    );
}

#[test]
fn duplicate_counts() {
    let f = Files::new();
    let mut more = base();
    more.push(source(9));
    let n = || notification("N1", "warning", "f");
    let a = invoked(doc(more), vec![invocation(true, Some(0), vec![n()])]);
    let b = invoked(doc(base()), vec![invocation(true, Some(0), vec![n(), n()])]);
    let a = f.write("a.sarif", &a);
    let b = f.write("b.sarif", &b);
    let r = diff(&a, &b);
    assert_eq!(counts(&r), (vec![(2, 1)], vec![]));
    assert_eq!(notification_counts(&r), (vec![], vec![(1, 2)]));
    assert_eq!(invocation_counts(&r), (vec![], vec![]));
    assert!(
        r.to_text()
            .contains("more in B: 1 notifications, 0 not shown\n  [A 1, B 2] ")
    );
}

#[test]
fn small_diff_text() {
    let f = Files::new();
    let invocations = |level: &str, exit_code: i64| {
        vec![invocation(
            true,
            Some(exit_code),
            vec![
                notification("CTADL0111", level, "f"),
                notification("CTADL0101", "note", "g"),
            ],
        )]
    };
    let a = invoked(
        doc(vec![flow(&[3, 5, 7]), source(3)]),
        invocations("warning", 0),
    );
    let b = invoked(
        doc(vec![flow(&[3, 6, 7]), source(3)]),
        invocations("error", 1),
    );
    let a = f.write("a.sarif", &a);
    let b = f.write("b.sarif", &b);
    let r = diff(&a, &b);
    let step = |line: i64| {
        format!(
            r#"{{"kinds":["taint"],"location":{{"physicalLocation":{{"artifactLocation":{{"uriBaseId":"SRC0","uri":"a.c"}},"region":{{"startLine":{line}}}}},"logicalLocations":["main"]}}}}"#
        )
    };
    let result = |mid: i64| {
        format!(
            r#"{{"ruleId":"C0001.tainted-path","kind":"fail","level":"error","locations":[{{"physicalLocation":{{"artifactLocation":{{"uriBaseId":"SRC0","uri":"a.c"}},"region":{{"startLine":3}}}},"logicalLocations":["main"]}}],"codeFlows":[{{"threadFlows":[{{"locations":[{},{},{}]}}]}}],"properties":{{"taintLabels":["L"],"sourceFunctions":["src"],"sinkFunctions":["snk"]}}}}"#,
            step(3),
            step(mid),
            step(7)
        )
    };
    let note = |level: &str| {
        format!(
            r#"{{"descriptor":{{"id":"CTADL0111"}},"level":"{level}","message":{{"id":"call-without-body"}},"properties":{{"calls":3}}}}"#
        )
    };
    let want = format!(
        "DIFFERENT\nA: {}\nB: {}\nresults: A 2, B 2\n\
         more in A: 1 results, 0 not shown\n  [A 1, B 0] {}\n\
         more in B: 1 results, 0 not shown\n  [A 0, B 1] {}\n\
         notifications: A 2, B 2\n\
         more in A: 1 notifications, 0 not shown\n  [A 1, B 0] {}\n\
         more in B: 1 notifications, 0 not shown\n  [A 0, B 1] {}\n\
         invocations: A 1, B 1\n\
         more in A: 1 invocations, 0 not shown\n  \
         [A 1, B 0] {{\"executionSuccessful\":true,\"exitCode\":0}}\n\
         more in B: 1 invocations, 0 not shown\n  \
         [A 0, B 1] {{\"executionSuccessful\":true,\"exitCode\":1}}\n",
        a.display(),
        b.display(),
        result(5),
        result(6),
        note("warning"),
        note("error"),
    );
    assert_eq!(r.to_text(), want);
}

#[test]
fn swap_is_mirror() {
    let f = Files::new();
    let a = f.write("a.sarif", &doc(base()));
    let b = f.write(
        "b.sarif",
        &doc(vec![flow(&[3, 6, 7]), source(3), source(3), source(4)]),
    );
    let ab = diff(&a, &b);
    let ba = diff(&b, &a);
    let swap = |s: &crate::Samples| crate::Samples {
        rows: s
            .rows
            .iter()
            .map(|r| crate::Sample {
                row: r.row.clone(),
                count_a: r.count_b,
                count_b: r.count_a,
            })
            .collect(),
        not_shown: s.not_shown,
    };
    assert_eq!(ba.more_in_a, swap(&ab.more_in_b));
    assert_eq!(ba.more_in_b, swap(&ab.more_in_a));
    assert_eq!((ba.results_a, ba.results_b), (ab.results_b, ab.results_a));
    assert_eq!(
        (ba.a.as_str(), ba.b.as_str()),
        (ab.b.as_str(), ab.a.as_str())
    );
    assert!(!ab.more_in_a.rows.is_empty() && !ab.more_in_b.rows.is_empty());
}

#[test]
fn output_is_deterministic() {
    let f = Files::new();
    let a = f.write("a.sarif", &doc(base()));
    let b = f.write(
        "b.sarif",
        &doc(vec![source(4), flow(&[1, 2]), source(2), flow(&[9, 8])]),
    );
    let opts = Options::default();
    let first = diff(&a, &b);
    assert!(!first.identical());
    assert_eq!(first.to_text(), diff(&a, &b).to_text());
    assert_eq!(first.to_json(), diff(&a, &b).to_json());

    // Interning B's strings first, or unrelated strings, gives every string another atom.
    let mut atoms = Atoms::new();
    read_runs(&b, &mut atoms).unwrap();
    let b_first = diff_with(&a, &b, &opts, atoms).unwrap();
    assert_eq!(first.to_text(), b_first.to_text());
    let mut atoms = Atoms::new();
    for s in ["zzz", "src", "snk", "main", "a.c", "L"] {
        atoms.intern(s);
    }
    let other = diff_with(&a, &b, &opts, atoms).unwrap();
    assert_eq!(first.to_json(), other.to_json());
}

#[test]
fn limit_bounds_samples() {
    let f = Files::new();
    // Each side also has two notifications and two statuses the other has not.
    let side = |ok: bool, codes: [i64; 2], ids: [&str; 2]| {
        codes
            .iter()
            .zip(ids)
            .map(|(&c, id)| invocation(ok, Some(c), vec![notification(id, "warning", "f")]))
            .collect()
    };
    let a = invoked(
        doc((100..130).map(source).collect()),
        side(true, [0, 2], ["N1", "N2"]),
    );
    let b = invoked(doc(Vec::new()), side(false, [1, 3], ["N3", "N4"]));
    let a = f.write("a.sarif", &a);
    let b = f.write("b.sarif", &b);
    let with = |limit| {
        diff_query(
            &a,
            &b,
            &Options {
                limit,
                ..Options::default()
            },
        )
        .unwrap()
    };
    let r = with(20);
    // The first by printed form: lines 100..=119.
    assert!(r.more_in_a.rows[0].row.contains("\"startLine\":100"));
    assert!(r.more_in_a.rows[19].row.contains("\"startLine\":119"));
    for limit in [0, 1, 20, 30, 100] {
        let r = with(limit);
        assert!(!r.identical(), "limit {limit}");
        assert_eq!(r.results_differ(), 30, "limit {limit}");
        assert_eq!(r.notifications_differ(), 4, "limit {limit}");
        assert_eq!(r.invocations_differ(), 4, "limit {limit}");
        assert_eq!(r.more_in_a.rows.len(), limit.min(30));
        assert_eq!(r.more_in_b.rows.len(), 0);
        for s in [
            &r.notifications_more_in_a,
            &r.notifications_more_in_b,
            &r.invocations_more_in_a,
            &r.invocations_more_in_b,
        ] {
            assert_eq!(s.rows.len(), limit.min(2), "limit {limit}");
            assert_eq!(s.total(), 2, "limit {limit}");
        }
    }
    for limit in [0, 1, 20] {
        let r = diff_query(
            &a,
            &a,
            &Options {
                limit,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(r.identical(), "limit {limit}");
    }
}

#[test]
fn json_matches_text() {
    let f = Files::new();
    let a = invoked(
        doc(base()),
        vec![invocation(
            true,
            Some(0),
            vec![notification("N1", "warning", "f")],
        )],
    );
    let a = f.write("a.sarif", &a);
    let mut fewer = base();
    fewer.remove(0);
    let b = invoked(
        doc(fewer),
        vec![invocation(
            false,
            Some(1),
            vec![notification("N1", "error", "f")],
        )],
    );
    let b = f.write("b.sarif", &b);
    for (x, y) in [(&a, &a), (&a, &b)] {
        let r = diff(x, y);
        let j: Value = serde_json::from_str(&r.to_json()).unwrap();
        let first = r.to_text().lines().next().unwrap().to_string();
        assert_eq!(j["result"], json!(first));
        assert_eq!(j["results_differ"], json!(r.results_differ()));
        assert_eq!(j["notifications_differ"], json!(r.notifications_differ()));
        assert_eq!(j["invocations_differ"], json!(r.invocations_differ()));
        assert_eq!(
            j["notifications_more_in_b"]["rows"]
                .as_array()
                .unwrap()
                .len(),
            r.notifications_more_in_b.rows.len()
        );
        assert_eq!(
            j["invocations_more_in_a"]["rows"].as_array().unwrap().len(),
            r.invocations_more_in_a.rows.len()
        );
        assert_eq!(
            j["more_in_a"]["rows"].as_array().unwrap().len(),
            r.more_in_a.rows.len()
        );
        assert_eq!(j["a"], json!(x.display().to_string()));
        assert_eq!(j["endpoints_only"], json!(false));
    }
    let r = diff(&a, &b);
    assert_eq!((r.notifications_differ(), r.invocations_differ()), (2, 2));
}

#[test]
fn endpoints_only() {
    let f = Files::new();
    let a = f.write("a.sarif", &doc(vec![flow(&[3, 5, 6, 7])]));
    let b = f.write("b.sarif", &doc(vec![flow(&[3, 6, 5, 7])]));
    let ends = Options {
        endpoints_only: true,
        ..Options::default()
    };
    assert!(!diff(&a, &b).identical());
    let r = diff_query(&a, &b, &ends).unwrap();
    assert!(r.identical());
    assert!(r.to_text().contains("\nflag: --endpoints-only\n"));

    // Two flows with the same endpoints still count twice.
    let two = f.write("two.sarif", &doc(vec![flow(&[3, 5, 7]), flow(&[3, 6, 7])]));
    let one = f.write("one.sarif", &doc(vec![flow(&[3, 4, 7])]));
    let r = diff_query(&two, &one, &ends).unwrap();
    assert_eq!(counts(&r), (vec![(2, 1)], vec![]));
    assert!(r.more_in_a.rows[0].row.contains("\"endpoints\":["));

    // Other rules are compared in full.
    let a = f.write("sa.sarif", &doc(vec![source(3)]));
    let b = f.write("sb.sarif", &doc(vec![source(4)]));
    assert!(!diff_query(&a, &b, &ends).unwrap().identical());

    // Notifications and statuses are compared as without the flag.
    let with = |ok: bool, level: &str| {
        invoked(
            doc(vec![flow(&[3, 5, 7])]),
            vec![invocation(
                ok,
                Some(0),
                vec![notification("N1", level, "f")],
            )],
        )
    };
    let a = f.write("na.sarif", &with(true, "warning"));
    for (name, b, want) in [
        ("nb.sarif", with(true, "error"), (2, 0)),
        ("ib.sarif", with(false, "warning"), (0, 2)),
    ] {
        let b = f.write(name, &b);
        let r = diff_query(&a, &b, &ends).unwrap();
        assert!(!r.identical(), "{name}");
        assert_eq!(r.results_differ(), 0, "{name}");
        assert_eq!(
            (r.notifications_differ(), r.invocations_differ()),
            want,
            "{name}"
        );
    }
}

#[test]
fn status_compared() {
    let f = Files::new();
    let with = |invocations: Vec<Value>| invoked(doc(base()), invocations);
    let status = |ok, code| with(vec![invocation(ok, code, vec![])]);
    let a = f.write("a.sarif", &status(true, Some(0)));
    for (name, b, want) in [
        ("failed.sarif", status(false, Some(0)), (1, 1)),
        ("code.sarif", status(true, Some(1)), (1, 1)),
        ("absent.sarif", status(true, None), (1, 1)),
        ("none.sarif", doc(base()), (1, 0)),
    ] {
        let b = f.write(name, &b);
        let r = diff(&a, &b);
        assert!(!r.identical(), "{name}");
        assert_eq!(r.results_differ(), 0, "{name}");
        assert_eq!(r.notifications_differ(), 0, "{name}");
        assert_eq!(
            (
                r.invocations_more_in_a.total(),
                r.invocations_more_in_b.total()
            ),
            want,
            "{name}"
        );
    }
    let absent = f.write("absent_a.sarif", &status(true, None));
    let absent_too = f.write("absent_b.sarif", &status(true, None));
    assert!(diff(&absent, &absent_too).identical());
}

#[test]
fn errors_are_errors() {
    let f = Files::new();
    let good = f.write("good.sarif", &doc(base()));
    let missing = f.0.path().join("missing.sarif");
    let not_json = f.0.path().join("not.sarif");
    std::fs::write(&not_json, "{").unwrap();

    let mut out_of_range = doc(base());
    out_of_range["runs"][0]["results"][1]["locations"][0]["logicalLocations"][0]["index"] =
        json!(1);
    let out_of_range = f.write("range.sarif", &out_of_range);

    let mut not_increasing = doc(base());
    not_increasing["runs"][0]["results"][0]["codeFlows"][0]["threadFlows"][0]["locations"][2]["executionOrder"] =
        json!(2);
    let not_increasing = f.write("order.sarif", &not_increasing);

    let mut empty = doc(base());
    empty["runs"][0]["results"][0]["codeFlows"][0]["threadFlows"][0]["locations"] = json!([]);
    let empty = f.write("empty.sarif", &empty);

    for endpoints_only in [false, true] {
        let opts = Options {
            endpoints_only,
            ..Options::default()
        };
        for (bad, canon) in [
            (&missing, None),
            (&not_json, None),
            (&out_of_range, Some(CanonError::OutOfRange)),
            (&not_increasing, Some(CanonError::NotIncreasing)),
            (&empty, Some(CanonError::EmptyFlow)),
        ] {
            for (x, y) in [(&good, bad), (bad, &good)] {
                let e = diff_query(x, y, &opts).unwrap_err();
                assert_eq!(&e.file, bad);
                match (&e.kind, canon) {
                    (ErrorKind::Parse(_), None) => {}
                    (ErrorKind::Canon(k), Some(c)) => assert_eq!(*k, c),
                    (k, c) => panic!("{k:?} for {c:?}"),
                }
                assert!(e.to_string().starts_with(&bad.display().to_string()));
            }
        }
    }
}
