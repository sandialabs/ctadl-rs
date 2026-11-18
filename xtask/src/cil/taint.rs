//! `cil:taint`: the index's summaries checked against the data flows a run of the exec
//! fixtures shows.
//!
//! **Static side.** Each exec fixture is imported and indexed by `ctadl` (CIL turns on
//! `deref_paths`), and the summary rows of its methods are read back, with the admitted paths
//! (`paths.parquet`). A row `(f, i, p, j, q)` says the value at formal `i`, path `p`, on entry
//! reaches formal `j`, path `q`, on exit.
//!
//! **Dynamic side.** `flow-run`'s trials are replayed in the `cil:exec` interpreter with a
//! shadow: every value carries the labels of the input locations its data came from. At
//! entry, each argument and every location reachable from it is labeled with the path it was
//! reached by (`arg1.<C::f>.[]`); at exit, every label found at an output location (the
//! result, the thrown object, and what is reachable from each argument) is a dynamic flow.
//! The concrete end state is still compared with .NET's; a trial whose state differs is
//! `cil:exec`'s to report and is left out here.
//!
//! **Comparison.** A dynamic flow `(i, p) -> (j, q)` is covered by a row
//! `(i, p', j, q')` with `p = p'·s` and `q = q'·s`. An uncovered flow is sorted:
//!
//! 1. *unknown field*, a fail: a field of `p` or `q` no admitted path mentions;
//! 2. *not admitted*, reported: `p` or `q` is not an admitted path, so the index cannot have
//!    the row. The index works over a closed set of paths fixed before the run, and drops a
//!    flow whose path falls outside it, keeping no shorter one in its place;
//! 3. *known*, XFAIL: it matches an entry of `xtask/cil-taint-known.json5`;
//! 4. otherwise a fail.
//!
//! Only the most general uncovered flows are sorted: a flow that is another uncovered flow
//! extended by a common suffix follows from it. An entry of the known-miss file that matches
//! no uncovered flow is an XPASS, and fails.
//!
//! What CTADL leaves out by design is left out of the shadow too, each mirroring a lowering
//! policy: a branch condition taints nothing (`Test` emits nothing), and an array index does
//! not taint the element read. Statics carry no labels and are not outputs: flows through them go
//! through the `global` formal, and nothing yet checks how the index summarizes it.
//!
//! The known-miss file is JSON5, which xtask already reads, so the offline Nix build needs no
//! new crate.
//!
//! Everything is written under `<work>/cil-taint/`: per fixture the dynamic flows, the
//! summary rows, the admitted paths and the sorted uncovered flows, and `summary.txt` with
//! per-method counts, including precision: the rows no trial showed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use cil_reader::flow::FlowOptions;
use cil_reader::{MetadataToken, Module};
use ctadl_ascent::facts::schema;
use ctadl_ascent::facts::IdMap;
use ctadl_cil::names::{ModNames, Namer};
use ctadl_ir::mir::PathSegment;
use serde::Deserialize;

use super::exec::interp::{Interp, Obj, SObj, Stop};
use super::exec::value::{Base, Labels, Sh, Value};
use super::exec::{self, Trial, EXEC_FIXTURES};
use super::parallel_map;
use crate::regression::Outcome;

/// How long a path the labeling and the exit walk follow.
const MAX_PATH: usize = 4;

/// Uncovered flows listed in the outcome before the rest are counted.
const MAX_LISTED: usize = 10;

/// The known-miss file, under the xtask directory.
pub const KNOWN_FILE: &str = "cil-taint-known.json5";

/// The kinds a known miss may have; the known-miss file's header says what each means.
const KINDS: &[&str] = &[
    "engine:path-composition",
    "engine:alias-rule",
    "design:unresolved-address-write",
];

/// The formal of the normal and the exceptional return.
const RET: i16 = -1;
const THROW: i16 = -2;

/// An input or output location: a formal and a path of field symbols.
type Point = (i16, Vec<String>);

/// A dynamic flow.
type Flow = (Point, Point);

// ---- labels ----------------------------------------------------------------------------

/// Interns the input locations labels stand for.
#[derive(Default)]
struct Labeler {
    table: Vec<Point>,
    index: HashMap<Point, u32>,
}

impl Labeler {
    fn label(&mut self, p: Point) -> u32 {
        if let Some(&l) = self.index.get(&p) {
            return l;
        }
        let l = self.table.len() as u32;
        self.table.push(p.clone());
        self.index.insert(p, l);
        l
    }
}

fn merge(dst: &mut Sh, src: Sh) {
    dst.labels.extend(src.labels);
    for (f, s) in src.fields {
        merge(dst.field_mut(f), s);
    }
}

/// The field symbols of a module's field tokens, as the frontend spells them.
#[derive(Default)]
struct Symbols {
    names: HashMap<u32, String>,
}

impl Symbols {
    fn field(&mut self, names: &ModNames<'_, '_, '_, '_>, token: u32) -> String {
        self.names
            .entry(token)
            .or_insert_with(|| {
                names
                    .field_symbol(MetadataToken(token))
                    .unwrap_or_else(|| format!("<?{token:08x}>"))
            })
            .clone()
    }
}

/// Labels a trial's inputs and reads its outputs.
struct Shadow<'a, 'm, 'd, 'n, 'x> {
    interp: &'a mut Interp<'m, 'd>,
    names: &'a ModNames<'n, 'x, 'x, 'x>,
    symbols: &'a mut Symbols,
    labeler: &'a mut Labeler,
}

impl Shadow<'_, '_, '_, '_, '_> {
    /// The shadow of argument `k`, holding `v`, with every location reachable from it labeled.
    fn label_arg(&mut self, k: i16, v: &Value) -> Sh {
        let mut seen = HashSet::new();
        self.label_value(k, v, &[], &mut seen)
    }

    fn label_value(
        &mut self,
        k: i16,
        v: &Value,
        path: &[String],
        seen: &mut HashSet<(usize, Vec<String>)>,
    ) -> Sh {
        let mut sh = Sh::of(Labels::from([self.labeler.label((k, path.to_vec()))]));
        if path.len() >= MAX_PATH {
            return sh;
        }
        match v {
            Value::Ref(o) => self.label_object(k, *o, path, seen),
            Value::Struct(s) => {
                for (f, fv) in &s.fields {
                    let p = extend(path, self.symbols.field(self.names, *f));
                    let fsh = self.label_value(k, fv, &p, seen);
                    sh.fields.insert(*f, fsh);
                }
            }
            Value::Ptr(loc) if loc.path.is_empty() => {
                if let Base::Cell(c) = loc.base {
                    let content = self.interp.cells[c].1.clone();
                    let p = extend(path, ctadl_ir::mir::DEREF_FIELD.to_string());
                    let csh = self.label_value(k, &content, &p, seen);
                    merge(&mut self.interp.scells[c], csh);
                }
            }
            _ => {}
        }
        sh
    }

    fn label_object(
        &mut self,
        k: i16,
        o: usize,
        path: &[String],
        seen: &mut HashSet<(usize, Vec<String>)>,
    ) {
        if !seen.insert((o, path.to_vec())) {
            return;
        }
        match self.interp.heap[o].clone() {
            Obj::Object { fields, .. } => {
                for (f, fv) in fields {
                    let p = extend(path, self.symbols.field(self.names, f));
                    let fsh = self.label_value(k, &fv, &p, seen);
                    if let SObj::Object(shs) = &mut self.interp.sheap[o] {
                        merge(shs.entry(f).or_default(), fsh);
                    }
                }
            }
            Obj::Array { items, .. } => {
                let p = extend(path, ctadl_cil::names::ELEMENT.to_string());
                for (i, item) in items.iter().enumerate() {
                    let ish = self.label_value(k, item, &p, seen);
                    if let SObj::Array { items, .. } = &mut self.interp.sheap[o] {
                        merge(&mut items[i], ish);
                    }
                }
                let l = self
                    .labeler
                    .label((k, extend(path, ctadl_cil::names::LENGTH.to_string())));
                if let SObj::Array { length, .. } = &mut self.interp.sheap[o] {
                    length.insert(l);
                }
            }
            // A box holds its value at the box's own path: the frontend lowers `box` as a
            // copy.
            Obj::Boxed { value, .. } => {
                let csh = self.label_value(k, &value, path, seen);
                if let SObj::Boxed(c) = &mut self.interp.sheap[o] {
                    merge(c, csh);
                }
            }
            // FlowRun passes no delegate in: an argument never reaches one.
            Obj::Delegate { .. } => {}
        }
    }

    /// Every location reachable from an output, with its labels. `record` is false for a
    /// location the caller does not see change: an argument variable itself, or a field of a
    /// struct passed by value. Objects the walk reaches are shared, so are always recorded.
    fn collect(
        &mut self,
        v: &Value,
        sh: &Sh,
        path: Vec<String>,
        record: bool,
        seen: &mut HashSet<(usize, Vec<String>)>,
        out: &mut Vec<(Vec<String>, Labels)>,
    ) {
        if record {
            out.push((path.clone(), sh.labels.clone()));
        }
        if path.len() >= MAX_PATH {
            return;
        }
        match v {
            Value::Ref(o) => {
                if !seen.insert((*o, path.clone())) {
                    return;
                }
                match (self.interp.heap[*o].clone(), self.interp.sheap[*o].clone()) {
                    (Obj::Object { fields, .. }, SObj::Object(shs)) => {
                        for (f, fv) in fields {
                            let fsh = shs.get(&f).cloned().unwrap_or_default();
                            let p = extend(&path, self.symbols.field(self.names, f));
                            self.collect(&fv, &fsh, p, true, seen, out);
                        }
                    }
                    (Obj::Array { items, .. }, SObj::Array { items: shs, length }) => {
                        let p = extend(&path, ctadl_cil::names::ELEMENT.to_string());
                        for (item, ish) in items.iter().zip(&shs) {
                            self.collect(item, ish, p.clone(), true, seen, out);
                        }
                        out.push((
                            extend(&path, ctadl_cil::names::LENGTH.to_string()),
                            length.clone(),
                        ));
                    }
                    (Obj::Boxed { value, .. }, SObj::Boxed(c)) => {
                        self.collect(&value, &c, path.clone(), true, seen, out);
                    }
                    _ => {}
                }
            }
            Value::Struct(s) => {
                for (f, fv) in &s.fields {
                    let p = extend(&path, self.symbols.field(self.names, *f));
                    self.collect(fv, &sh.field(*f), p, record, seen, out);
                }
            }
            Value::Ptr(loc) if loc.path.is_empty() => {
                if let Base::Cell(c) = loc.base {
                    let content = self.interp.cells[c].1.clone();
                    let csh = self.interp.scells[c].clone();
                    let p = extend(&path, ctadl_ir::mir::DEREF_FIELD.to_string());
                    self.collect(&content, &csh, p, true, seen, out);
                }
            }
            _ => {}
        }
    }
}

fn extend(path: &[String], seg: String) -> Vec<String> {
    let mut p = path.to_vec();
    p.push(seg);
    p
}

// ---- static side -----------------------------------------------------------------------

/// A summary row: `src` on entry reaches `dst` on exit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Row {
    src: Point,
    dst: Point,
}

/// What the index says about a fixture.
#[derive(Default)]
struct Static {
    rows: HashMap<String, Vec<Row>>,
    admitted: HashSet<Vec<String>>,
    /// Every field an admitted path mentions.
    fields: HashSet<String>,
}

fn segments(path: &ctadl_ascent::facts::Path) -> Vec<String> {
    path.iter()
        .map(|s| match s {
            PathSegment::Symbol(sym) => sym.to_string(),
            PathSegment::Offset(o) => format!("[{}]", o.0),
        })
        .collect()
}

/// Imports and indexes `fixture` with `ctadl` into `dir`, and reads the index back.
fn index_fixture(ctadl: &Path, fixture: &Path, dir: &Path) -> Result<Static> {
    let store = dir.join("store");
    if store.exists() {
        std::fs::remove_dir_all(&store)?;
    }
    let name = "fixture";
    for args in [
        vec![
            "import".to_string(),
            "--store".to_string(),
            store.to_string_lossy().into_owned(),
            "-l".to_string(),
            "cil".to_string(),
            "-n".to_string(),
            name.to_string(),
            fixture.to_string_lossy().into_owned(),
        ],
        vec![
            "index".to_string(),
            "--store".to_string(),
            store.to_string_lossy().into_owned(),
            name.to_string(),
        ],
    ] {
        let mut cmd = Command::new(ctadl);
        cmd.args(&args);
        let output = cmd
            .output()
            .with_context(|| format!("running {}", ctadl.display()))?;
        let log = dir.join(format!("ctadl-{}.log", args[0]));
        let mut text = output.stdout.clone();
        text.extend_from_slice(&output.stderr);
        std::fs::write(&log, &text)?;
        if !output.status.success() {
            bail!("ctadl {} failed; see {}", args[0], log.display());
        }
    }
    let index = store.join("projects").join(name).join("index");
    let ids = IdMap::try_load(&index).map_err(|e| anyhow::anyhow!("{e}"))?;
    let rows = schema::summary::try_load(&index).map_err(|e| anyhow::anyhow!("{e}"))?;
    let paths = schema::paths::try_load(&index).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut st = Static::default();
    for (f, dst_i, dst_p, src_i, src_p) in rows {
        let Some(name) = ids.get_function(f) else {
            continue;
        };
        st.rows
            .entry(name.0.as_str().to_string())
            .or_default()
            .push(Row {
                src: (*src_i, segments(&src_p)),
                dst: (*dst_i, segments(&dst_p)),
            });
    }
    for rows in st.rows.values_mut() {
        rows.sort();
        rows.dedup();
    }
    for (p,) in paths {
        let segs = segments(&p);
        st.fields.extend(segs.iter().cloned());
        st.admitted.insert(segs);
    }
    Ok(st)
}

fn covered(rows: &[Row], (src, dst): &Flow) -> bool {
    rows.iter().any(|r| {
        r.src.0 == src.0
            && r.dst.0 == dst.0
            && src.1.starts_with(&r.src.1)
            && dst.1.starts_with(&r.dst.1)
            && src.1[r.src.1.len()..] == dst.1[r.dst.1.len()..]
    })
}

/// Whether `general` implies `specific`: the same formals, and `specific` is `general`
/// extended on both sides by one suffix.
fn implies(general: &Flow, specific: &Flow) -> bool {
    general != specific
        && general.0 .0 == specific.0 .0
        && general.1 .0 == specific.1 .0
        && specific.0 .1.starts_with(&general.0 .1)
        && specific.1 .1.starts_with(&general.1 .1)
        && specific.0 .1[general.0 .1.len()..] == specific.1 .1[general.1 .1.len()..]
}

// ---- known misses ----------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
struct Known {
    method: String,
    flow: String,
    kind: String,
    why: String,
}

/// One step of a known-miss flow pattern: a segment, and whether it may repeat (`f*`).
#[derive(Debug, Clone)]
struct PatSeg {
    seg: String,
    star: bool,
}

#[derive(Debug, Clone)]
struct Pattern {
    src: (i16, Vec<PatSeg>),
    dst: (i16, Vec<PatSeg>),
}

fn read_known(path: &Path) -> Result<Vec<(Known, Pattern)>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading CIL known exceptions {}", path.display()))?;
    let entries: Vec<Known> =
        json5::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut out = Vec::new();
    for k in entries {
        if !KINDS.contains(&k.kind.as_str()) {
            bail!(
                "{}: unknown kind `{}` (known: {KINDS:?})",
                path.display(),
                k.kind
            );
        }
        if k.why.trim().is_empty() {
            bail!("{}: `{}` has no `why`", path.display(), k.flow);
        }
        if k.kind.starts_with("engine:") && !k.why.contains(".tnt") {
            bail!(
                "{}: the engine miss `{}` names no .tnt reproducer",
                path.display(),
                k.flow
            );
        }
        let pattern = parse_pattern(&k.flow)
            .with_context(|| format!("{}: bad flow `{}`", path.display(), k.flow))?;
        out.push((k, pattern));
    }
    Ok(out)
}

fn parse_pattern(flow: &str) -> Result<Pattern> {
    let (src, dst) = flow.split_once("->").context("a flow is `src -> dst`")?;
    Ok(Pattern {
        src: parse_point(src.trim())?,
        dst: parse_point(dst.trim())?,
    })
}

/// `argN`, `ret` or `throw`, then `.segment`s: `<...>` (which may hold dots), `[]`,
/// `length` or `deref`, each optionally followed by `*`.
fn parse_point(s: &str) -> Result<(i16, Vec<PatSeg>)> {
    let root_end = s.find('.').unwrap_or(s.len());
    let root = &s[..root_end];
    let formal = match root {
        "ret" => RET,
        "throw" => THROW,
        r if r.starts_with("arg") => r[3..].parse().context("argN")?,
        other => bail!("unknown root `{other}`"),
    };
    let mut segs = Vec::new();
    let mut rest = &s[root_end..];
    while let Some(r) = rest.strip_prefix('.') {
        let (seg, after) = if r.starts_with('<') {
            let end = r.find('>').context("an unclosed `<`")?;
            (&r[..=end], &r[end + 1..])
        } else {
            let end = r.find(['.', '*']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        };
        let (star, after) = match after.strip_prefix('*') {
            Some(a) => (true, a),
            None => (false, after),
        };
        segs.push(PatSeg {
            seg: seg.to_string(),
            star,
        });
        rest = after;
    }
    if !rest.is_empty() {
        bail!("trailing `{rest}`");
    }
    Ok((formal, segs))
}

/// Whether a pattern segment names a path segment: equal, or `<name>` / `<Class::name>`
/// naming a field `<[Asm]Ns.Class::name>`.
fn seg_matches(pat: &str, seg: &str) -> bool {
    if pat == seg {
        return true;
    }
    let (Some(p), Some(s)) = (
        pat.strip_prefix('<').and_then(|p| p.strip_suffix('>')),
        seg.strip_prefix('<').and_then(|s| s.strip_suffix('>')),
    ) else {
        return false;
    };
    let s_no_asm = s.split_once(']').map(|(_, r)| r).unwrap_or(s);
    let name = s.rsplit_once("::").map(|(_, n)| n).unwrap_or(s);
    p == name || s_no_asm == p || s_no_asm.ends_with(&format!(".{p}"))
}

fn path_matches(pat: &[PatSeg], path: &[String]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((p, rest)) if p.star => {
            path_matches(rest, path)
                || (path.first().is_some_and(|s| seg_matches(&p.seg, s))
                    && path_matches(pat, &path[1..]))
        }
        Some((p, rest)) => {
            path.first().is_some_and(|s| seg_matches(&p.seg, s)) && path_matches(rest, &path[1..])
        }
    }
}

fn method_matches(pat: &str, fid: &str) -> bool {
    let no_asm = fid.split_once(']').map(|(_, r)| r).unwrap_or(fid);
    no_asm == pat || no_asm.starts_with(&format!("{pat}("))
}

fn known_matches(k: &Known, p: &Pattern, fid: &str, (src, dst): &Flow) -> bool {
    method_matches(&k.method, fid)
        && p.src.0 == src.0
        && p.dst.0 == dst.0
        && path_matches(&p.src.1, &src.1)
        && path_matches(&p.dst.1, &dst.1)
}

fn show_point((formal, path): &Point) -> String {
    let root = match *formal {
        RET => "ret".to_string(),
        THROW => "throw".to_string(),
        n => format!("arg{n}"),
    };
    let mut s = root;
    for seg in path {
        s.push('.');
        s.push_str(seg);
    }
    s
}

fn show_flow((src, dst): &Flow) -> String {
    format!("{} -> {}", show_point(src), show_point(dst))
}

// ---- the check -------------------------------------------------------------------------

/// The dynamic flows of one method, and the trials that showed them.
#[derive(Default)]
struct Method {
    trials: usize,
    flows: BTreeMap<Flow, u32>,
}

/// Replays a fixture's trials with the shadow; the dynamic flows of each method by function
/// id, and the number of trials left out because they did not match .NET.
fn dynamic(
    module: &Module<'_>,
    trials: &[Trial],
) -> (BTreeMap<String, Method>, usize, Vec<String>) {
    let loader = cil_reader::model::DirectoryLoader::new(ctadl_cil::framework_reference_dirs());
    let cache = cil_reader::model::AssemblyCache::new(&loader);
    let namer = Namer::new(&cache);
    let names = ModNames::new(module, &namer);
    let mut symbols = Symbols::default();
    let mut interp = Interp::new(module, FlowOptions::default());
    let mut methods: BTreeMap<String, Method> = BTreeMap::new();
    let mut skipped = 0;
    let mut errors = Vec::new();
    for trial in trials {
        let Some(fid) = names
            .method_key(MetadataToken(trial.method))
            .map(|k| k.fid())
        else {
            continue;
        };
        interp.reset();
        let mut labeler = Labeler::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_trial(&mut interp, &names, &mut symbols, &mut labeler, trial)
        }))
        .unwrap_or_else(|_| Err(Stop::Bug("the interpreter panicked".into())));
        match result {
            Ok(Some(flows)) => {
                let m = methods.entry(fid).or_default();
                m.trials += 1;
                for f in flows {
                    *m.flows.entry(f).or_default() += 1;
                }
            }
            Ok(None) => skipped += 1,
            Err(Stop::Bug(msg)) => {
                errors.push(format!("{} trial {}: {msg}", trial.name, trial.trial))
            }
            Err(_) => {}
        }
    }
    (methods, skipped, errors)
}

/// One trial with the shadow: its dynamic flows, or `None` when its end state does not match
/// .NET's.
fn run_trial(
    interp: &mut Interp<'_, '_>,
    names: &ModNames<'_, '_, '_, '_>,
    symbols: &mut Symbols,
    labeler: &mut Labeler,
    trial: &Trial,
) -> Result<Option<Vec<Flow>>, Stop> {
    let (p, args) = exec::setup(interp, trial)?;
    let original = args.clone();
    let mut shs = Vec::with_capacity(args.len());
    {
        let mut shadow = Shadow {
            interp: &mut *interp,
            names,
            symbols: &mut *symbols,
            labeler: &mut *labeler,
        };
        for (k, a) in args.iter().enumerate() {
            shs.push(shadow.label_arg(k as i16, a));
        }
    }
    let entry_shs = shs.clone();
    let (result, ret_sh) = match interp.call_sh(trial.method, args, shs) {
        Ok((v, sh)) => (Ok(v), sh),
        Err(Stop::Throw(e)) => (Err(e), Sh::default()),
        Err(other) => return Err(other),
    };
    let text = exec::render(interp, trial, &p, &original, &result)?;
    if text != trial.outcome {
        return Ok(None);
    }
    let mut outputs: Vec<(i16, Vec<(Vec<String>, Labels)>)> = Vec::new();
    let mut shadow = Shadow {
        interp: &mut *interp,
        names,
        symbols,
        labeler: &mut *labeler,
    };
    match &result {
        Ok(Some(v)) => {
            let mut out = Vec::new();
            shadow.collect(v, &ret_sh, Vec::new(), true, &mut HashSet::new(), &mut out);
            outputs.push((RET, out));
        }
        Ok(None) => {}
        Err(e) => {
            let sh = shadow.interp.exc_shadow(e);
            let mut out = Vec::new();
            shadow.collect(e, &sh, Vec::new(), true, &mut HashSet::new(), &mut out);
            outputs.push((THROW, out));
        }
    }
    for (k, a) in original.iter().enumerate() {
        let mut out = Vec::new();
        shadow.collect(
            a,
            &entry_shs[k],
            Vec::new(),
            false,
            &mut HashSet::new(),
            &mut out,
        );
        outputs.push((k as i16, out));
    }
    let mut flows = BTreeSet::new();
    for (j, locations) in outputs {
        for (q, labels) in locations {
            for l in labels {
                let src = labeler.table[l as usize].clone();
                let dst = (j, q.clone());
                if src != dst {
                    flows.insert((src, dst));
                }
            }
        }
    }
    Ok(Some(flows.into_iter().collect()))
}

/// What became of one method's uncovered flows.
#[derive(Default)]
struct Sorted {
    unknown_field: Vec<Flow>,
    not_admitted: Vec<Flow>,
    known: Vec<(Flow, String)>,
    failed: Vec<Flow>,
}

pub fn run(ctadl: &Path, fixtures: &Path, xtask_dir: &Path, work: &Path) -> Result<Outcome> {
    let Some(flow_run) = exec::locate_flow_run() else {
        return Ok(Outcome::Skip(format!(
            "`flow-run` not on PATH and ${} unset; enter the dev shell or `nix build \
             .#flow-run`",
            exec::FLOW_RUN_ENV
        )));
    };
    let dir = work.join("cil-taint");
    std::fs::create_dir_all(&dir)?;
    let known = read_known(&xtask_dir.join(KNOWN_FILE))?;
    let known_hits: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(vec![0; known.len()]);

    let reports = parallel_map(EXEC_FIXTURES, |fixture| {
        check_fixture(
            ctadl,
            &flow_run,
            &fixtures.join(fixture),
            &dir,
            fixture,
            &known,
            &known_hits,
        )
    });

    let mut failures = Vec::new();
    let mut summary = String::new();
    let mut not_admitted = 0usize;
    let mut xfails: BTreeMap<String, usize> = BTreeMap::new();
    for (fixture, report) in EXEC_FIXTURES.iter().zip(reports) {
        match report {
            Ok(r) => {
                summary.push_str(&r.summary);
                failures.extend(r.failures);
                not_admitted += r.not_admitted;
                for (k, n) in r.xfails {
                    *xfails.entry(k).or_default() += n;
                }
            }
            Err(e) => failures.push(format!("{fixture}: {e:#}")),
        }
    }
    let hits = known_hits.into_inner().unwrap();
    for ((k, _), n) in known.iter().zip(hits) {
        if n == 0 {
            failures.push(format!(
                "XPASS {} `{}`: matches no uncovered flow; remove it from {KNOWN_FILE}",
                k.method, k.flow
            ));
        }
    }
    let _ = writeln!(
        summary,
        "TOTAL not-admitted={not_admitted} xfail={xfails:?} failures={}",
        failures.len()
    );
    std::fs::write(dir.join("summary.txt"), &summary)?;
    if failures.is_empty() {
        return Ok(Outcome::Pass);
    }
    let mut msg = format!(
        "{} uncovered flow(s), all in {}:",
        failures.len(),
        dir.display()
    );
    for f in failures.iter().take(MAX_LISTED) {
        let _ = write!(msg, "\n{f}");
    }
    if failures.len() > MAX_LISTED {
        let _ = write!(msg, "\n... and {} more", failures.len() - MAX_LISTED);
    }
    Ok(Outcome::Fail(msg))
}

struct FixtureReport {
    summary: String,
    failures: Vec<String>,
    not_admitted: usize,
    xfails: BTreeMap<String, usize>,
}

fn check_fixture(
    ctadl: &Path,
    flow_run: &Path,
    path: &Path,
    dir: &Path,
    fixture: &str,
    known: &[(Known, Pattern)],
    known_hits: &std::sync::Mutex<Vec<usize>>,
) -> Result<FixtureReport> {
    let fdir = dir.join(fixture);
    std::fs::create_dir_all(&fdir)?;
    let st = index_fixture(ctadl, path, &fdir)?;
    let trials = exec::flow_run_trials(flow_run, path, &fdir, fixture, None)?;
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let module = Module::read(&data).map_err(|e| anyhow::anyhow!("{e}"))?;
    let (methods, skipped, errors) = dynamic(&module, &trials);

    let mut report = FixtureReport {
        summary: String::new(),
        failures: errors
            .into_iter()
            .map(|e| format!("{fixture} {e}"))
            .collect(),
        not_admitted: 0,
        xfails: BTreeMap::new(),
    };
    let mut dynamic_txt = String::new();
    let mut rows_txt = String::new();
    let mut sorted_txt = String::new();
    let no_rows = Vec::new();
    for (fid, m) in &methods {
        let rows = st.rows.get(fid).unwrap_or(&no_rows);
        let _ = writeln!(dynamic_txt, "{fid} ({} trials)", m.trials);
        for (f, n) in &m.flows {
            let _ = writeln!(dynamic_txt, "  {} [{n}]", show_flow(f));
        }
        let _ = writeln!(rows_txt, "{fid}");
        for r in rows {
            let _ = writeln!(rows_txt, "  {}", show_flow(&(r.src.clone(), r.dst.clone())));
        }
        let uncovered: Vec<&Flow> = m.flows.keys().filter(|f| !covered(rows, f)).collect();
        let general: Vec<&Flow> = uncovered
            .iter()
            .copied()
            .filter(|f| !uncovered.iter().any(|g| implies(g, f)))
            .collect();
        let mut sorted = Sorted::default();
        for f in general {
            let fields_known = f.0 .1.iter().chain(&f.1 .1).all(|s| st.fields.contains(s));
            let admitted = |p: &Vec<String>| p.is_empty() || st.admitted.contains(p);
            if !fields_known {
                sorted.unknown_field.push(f.clone());
            } else if !admitted(&f.0 .1) || !admitted(&f.1 .1) {
                sorted.not_admitted.push(f.clone());
            } else if let Some(i) = known.iter().position(|(k, p)| known_matches(k, p, fid, f)) {
                known_hits.lock().unwrap()[i] += 1;
                sorted.known.push((f.clone(), known[i].0.kind.clone()));
            } else {
                sorted.failed.push(f.clone());
            }
        }
        let shown: usize = rows
            .iter()
            .filter(|r| m.flows.keys().any(|f| covered(std::slice::from_ref(*r), f)))
            .count();
        let _ = writeln!(
            report.summary,
            "{fixture} {fid} trials={} flows={} covered={} not-admitted={} xfail={} \
             unknown-field={} failed={} rows={} rows-unseen={}",
            m.trials,
            m.flows.len(),
            m.flows.len() - uncovered.len(),
            sorted.not_admitted.len(),
            sorted.known.len(),
            sorted.unknown_field.len(),
            sorted.failed.len(),
            rows.len(),
            rows.len() - shown
        );
        if !sorted.unknown_field.is_empty()
            || !sorted.not_admitted.is_empty()
            || !sorted.known.is_empty()
            || !sorted.failed.is_empty()
        {
            let _ = writeln!(sorted_txt, "{fid}");
        }
        for f in &sorted.unknown_field {
            let _ = writeln!(sorted_txt, "  UNKNOWN FIELD {}", show_flow(f));
            report.failures.push(format!(
                "{fixture} {fid}: a field no admitted path mentions: {}",
                show_flow(f)
            ));
        }
        for f in &sorted.not_admitted {
            let _ = writeln!(sorted_txt, "  NOT ADMITTED {}", show_flow(f));
        }
        for (f, kind) in &sorted.known {
            let _ = writeln!(sorted_txt, "  XFAIL ({kind}) {}", show_flow(f));
            *report.xfails.entry(kind.clone()).or_default() += 1;
        }
        for f in &sorted.failed {
            let _ = writeln!(sorted_txt, "  FAIL {}", show_flow(f));
            report
                .failures
                .push(format!("{fixture} {fid}: uncovered: {}", show_flow(f)));
        }
        report.not_admitted += sorted.not_admitted.len();
    }
    let _ = writeln!(
        report.summary,
        "{fixture} TOTAL methods={} trials-not-matching-.NET={skipped}",
        methods.len()
    );
    let mut paths_txt = String::new();
    let mut admitted: Vec<&Vec<String>> = st.admitted.iter().collect();
    admitted.sort();
    for p in admitted {
        let _ = writeln!(paths_txt, ".{}", p.join("."));
    }
    std::fs::write(fdir.join("dynamic.txt"), dynamic_txt)?;
    std::fs::write(fdir.join("summaries.txt"), rows_txt)?;
    std::fs::write(fdir.join("paths.txt"), paths_txt)?;
    std::fs::write(fdir.join("sorted.txt"), sorted_txt)?;
    if methods.is_empty() {
        report
            .failures
            .push(format!("{fixture}: no trial ran with the shadow"));
    }
    Ok(report)
}

/// The xtask directory, where the known-miss file lives: `xtask` from the repo root, or the
/// current directory inside the Nix check.
pub fn resolve_xtask_dir() -> PathBuf {
    for candidate in ["xtask", "../xtask", "."] {
        let p = PathBuf::from(candidate);
        if p.join("cil-goldens").is_dir() {
            return p;
        }
    }
    PathBuf::from("xtask")
}
