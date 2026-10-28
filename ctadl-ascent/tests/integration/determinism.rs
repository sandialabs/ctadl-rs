//! Printed output does not depend on the order values were interned in (spec C3).
//!
//! A handle's `Ord` is its intern id, and which id a value gets depends on what was interned
//! before it. So any output sorted by handle comes out in a different order in a process that
//! interned in a different order. [`output_does_not_depend_on_intern_order`] runs the fixture's
//! whole pipeline in child processes of this test binary: one to list the values the pipeline
//! interns, then one that pre-interns them in content order and one in reverse. It asserts that
//! both render byte-identical graphviz, relation dumps and SARIF.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use ctadl_ascent::cli;
use ctadl_ascent::facts::{Path as AccessPath, PathId, Str};
use ctadl_ascent::project::*;
use ctadl_ascent::query_engine::formatter::SarifProfile;

/// Selects the child's role: `collect`, `forward` or `reverse`.
const ROLE_ENV: &str = "CTADL_TEST_INTERN_ROLE";
/// The child's output directory.
const OUT_ENV: &str = "CTADL_TEST_INTERN_OUT";
/// The values file `collect` writes and the other roles pre-intern from.
const VALUES_ENV: &str = "CTADL_TEST_INTERN_VALUES";

/// What a child pre-interns before it runs the pipeline. A new handle type joins the test by
/// adding a kind here, with its arms in [`collect_values`] and [`pre_intern`].
#[derive(Clone, Copy, Debug)]
enum Kind {
    Str,
    Path,
}

const PRE_INTERN: &[Kind] = &[Kind::Str, Kind::Path];

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Str => "Str",
            Kind::Path => "Path",
        }
    }
}

/// `Str` and `Path` ids are `((local + 1) << 8) | shard`, `local` being the value's position in
/// its shard (`immortal::id`), so ids order by position first and shard second.
const SHARD_BITS: u32 = 8;
const SHARD_MASK: u32 = (1 << SHARD_BITS) - 1;

fn shard(raw: u32) -> usize {
    (raw & SHARD_MASK) as usize
}

fn position(raw: u32) -> u32 {
    (raw >> SHARD_BITS) - 1
}

/// Raises each shard's entry in `depth` to the number of values in it, `issued` telling which
/// raw ids the interner handed out.
fn read_depths(depth: &mut [u32], issued: impl Fn(u32) -> bool) {
    for (shard, d) in depth.iter_mut().enumerate() {
        while issued(((*d + 1) << SHARD_BITS) | shard as u32) {
            *d += 1;
        }
    }
}

/// Whether `raw` is a `PathId` the interner handed out.
fn path_issued(raw: u32) -> bool {
    PathId::from_raw(raw).is_some()
}

/// Every value of `kind` this process has interned, in content order.
///
/// A `Str` comes with the shard it hashes to, which depends on the string alone. A `Path`
/// comes alone, and only if it is no other path's tail: interning it interns its suffixes, and
/// its shard hashes its segments' `ArcIntern` pointers, which differ between processes.
fn collect_values(kind: Kind) -> Vec<serde_json::Value> {
    match kind {
        Kind::Str => {
            let entries = Str::stats().entries;
            let mut values: Vec<(String, usize)> = (1..=u32::MAX)
                .filter_map(Str::from_raw)
                .take(entries)
                .map(|s| (s.as_str().to_owned(), shard(s.raw())))
                .collect();
            values.sort();
            values.iter().map(|v| serde_json::json!(v)).collect()
        }
        Kind::Path => {
            let entries = PathId::stats().entries;
            let paths: Vec<AccessPath> = (1..=u32::MAX)
                .filter(|&raw| path_issued(raw))
                .filter_map(AccessPath::from_raw)
                .take(entries)
                .collect();
            let tails: HashSet<AccessPath> = paths.iter().map(|p| p.tail()).collect();
            let mut whole: Vec<AccessPath> =
                paths.into_iter().filter(|p| !tails.contains(p)).collect();
            whole.sort_unstable_by(|a, b| a.content_cmp(b));
            whole
                .iter()
                .map(|p| serde_json::to_value(p).unwrap())
                .collect()
        }
    }
}

/// Interns `values` so that their ids come out in exactly this order. Interning them one after
/// another is not enough: most share no shard, so their ids would order by shard in every
/// process. Before each value, filler values go in until the value's shard is deeper than the
/// position of every value before it.
///
/// A `Str`'s shard is known before it is interned, so only that shard is filled. A `Path`'s is
/// not, so every shard is. The suffixes a path brings with it land where they land; the order
/// asserted is the whole paths'.
fn pre_intern(kind: Kind, values: &[serde_json::Value]) {
    let mut depth = vec![0u32; 1 << SHARD_BITS];
    let mut fillers = 0u64;
    let mut floor = 0u32;
    match kind {
        Kind::Str => {
            read_depths(&mut depth, |raw| Str::from_raw(raw).is_some());
            let mut last: Option<Str> = None;
            for value in values {
                let (v, v_shard): (String, usize) = serde_json::from_value(value.clone()).unwrap();
                while depth[v_shard] < floor {
                    let f = Str::intern(&format!("\u{1}filler{fillers}"));
                    fillers += 1;
                    depth[shard(f.raw())] = position(f.raw()) + 1;
                }
                let s = Str::intern(&v);
                assert_eq!(shard(s.raw()), v_shard, "{v:?} changed shard");
                depth[v_shard] = depth[v_shard].max(position(s.raw()) + 1);
                floor = floor.max(position(s.raw()) + 1);
                if let Some(prev) = last {
                    assert!(prev < s, "{v:?} was interned before {:?}", prev.as_str());
                }
                last = Some(s);
            }
        }
        Kind::Path => {
            read_depths(&mut depth, path_issued);
            let mut last: Option<AccessPath> = None;
            for value in values {
                while depth.iter().any(|&d| d < floor) {
                    let f = AccessPath::from_symbol_names([format!("\u{1}filler{fillers}")]);
                    fillers += 1;
                    depth[shard(f.raw())] = position(f.raw()) + 1;
                }
                let p: AccessPath = serde_json::from_value(value.clone()).unwrap();
                // The suffixes `p` brought with it went into shards of their own.
                read_depths(&mut depth, path_issued);
                floor = floor.max(position(p.raw()) + 1);
                if let Some(prev) = last {
                    assert!(prev < p, "{p} was interned before {prev}");
                }
                last = Some(p);
            }
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "c", name]
        .iter()
        .collect()
}

/// Imports, indexes and queries `ties.c` into a store under `out`, writing every rendering
/// the test compares into `out`.
fn run_pipeline(out: &Path) {
    init_store_path(Some(&out.join("store"))).unwrap();
    let import =
        ArtifactImport::try_create("ties", ArtifactLanguage::C, &fixture("ties.c")).unwrap();
    cli::import(&import, cli::ImportOptions::default()).unwrap();
    let project = AnalysisProject::try_create("ties_proj", &["ties"], SubImports::All).unwrap();
    let models = vec![fixture("ties.json")];
    cli::index(&project, &[], &models, false, cli::IndexOptions::default()).unwrap();
    cli::inspect_index_graph(&project, &out.join("index.dot")).unwrap();
    for (profile, name) in [
        (SarifProfile::Human, "human"),
        (SarifProfile::Agent, "agent"),
        (SarifProfile::Debug, "debug"),
    ] {
        let taint_dot = out.join(format!("taint-{name}.dot"));
        cli::query(
            &project,
            &models,
            &out.join(format!("{name}.sarif")),
            profile,
            Some(&taint_dot),
        )
        .unwrap();
    }
}

fn child(role: &str) {
    let out = PathBuf::from(std::env::var(OUT_ENV).unwrap());
    let values_path = PathBuf::from(std::env::var(VALUES_ENV).unwrap());
    if role == "collect" {
        run_pipeline(&out);
        let values: BTreeMap<&str, Vec<serde_json::Value>> = PRE_INTERN
            .iter()
            .map(|&k| (k.name(), collect_values(k)))
            .collect();
        std::fs::write(&values_path, serde_json::to_string(&values).unwrap()).unwrap();
        return;
    }
    let mut values: BTreeMap<String, Vec<serde_json::Value>> =
        serde_json::from_str(&std::fs::read_to_string(&values_path).unwrap()).unwrap();
    for &kind in PRE_INTERN {
        let mut vs = values.remove(kind.name()).unwrap();
        match role {
            "forward" => {}
            "reverse" => vs.reverse(),
            _ => panic!("unknown {ROLE_ENV} {role}"),
        }
        pre_intern(kind, &vs);
    }
    run_pipeline(&out);
}

/// Runs this test as a child process in `role`, writing into `out`.
fn spawn(role: &str, out: &Path, values: &Path) {
    std::fs::create_dir_all(out.join("dump")).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "determinism::output_does_not_depend_on_intern_order",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(ROLE_ENV, role)
        .env(OUT_ENV, out)
        .env(VALUES_ENV, values)
        .env("CTADL_DUMP_RELATIONS", out.join("dump"))
        .status()
        .unwrap();
    assert!(status.success(), "the {role} child failed");
}

/// The files the comparison reads, by path relative to `out`, normalized: SARIF loses its
/// timestamps and the output directory, and a relation dump is sorted, since the engine's
/// row order varies between runs whatever the intern order (`docs/debugging.md`).
fn renderings(out: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut add = |rel: String, path: &Path| {
        let mut text = std::fs::read_to_string(path).unwrap();
        if rel.ends_with(".sarif") {
            let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
            for inv in doc["runs"][0]["invocations"]
                .as_array_mut()
                .into_iter()
                .flatten()
            {
                let inv = inv.as_object_mut().unwrap();
                inv.remove("startTimeUtc");
                inv.remove("endTimeUtc");
            }
            text = serde_json::to_string_pretty(&doc).unwrap();
        } else if rel.starts_with("dump/") {
            let mut lines: Vec<&str> = text.lines().collect();
            lines.sort_unstable();
            text = lines.join("\n");
        }
        files.insert(rel, text.replace(out.to_str().unwrap(), "<out>"));
    };
    for entry in std::fs::read_dir(out).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name.ends_with(".dot") || name.ends_with(".sarif") {
            add(name, &path);
        }
    }
    for entry in std::fs::read_dir(out.join("dump")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        add(format!("dump/{name}"), &path);
    }
    files
}

#[test]
fn output_does_not_depend_on_intern_order() {
    if let Ok(role) = std::env::var(ROLE_ENV) {
        child(&role);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let values = dir.path().join("values.json");
    spawn("collect", &dir.path().join("collect"), &values);
    let forward = dir.path().join("forward");
    let reverse = dir.path().join("reverse");
    spawn("forward", &forward, &values);
    spawn("reverse", &reverse, &values);

    let a = renderings(&forward);
    let b = renderings(&reverse);
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "the two runs wrote different files"
    );
    assert!(a.contains_key("index.dot") && a.contains_key("human.sarif"));
    assert!(
        a["human.sarif"].contains("C0001.tainted-path"),
        "the fixture must report a taint flow"
    );
    let differing: Vec<&String> = a.keys().filter(|k| a[*k] != b[*k]).collect();
    for name in &differing {
        let (x, y) = (&a[*name], &b[*name]);
        let first = x
            .lines()
            .zip(y.lines())
            .position(|(l, r)| l != r)
            .unwrap_or(x.lines().count().min(y.lines().count()));
        eprintln!(
            "{name} differs from line {}:\n  forward: {:?}\n  reverse: {:?}",
            first + 1,
            x.lines().nth(first),
            y.lines().nth(first)
        );
    }
    assert!(
        differing.is_empty(),
        "output depends on intern order: {differing:?}"
    );
}
