//! `cil:exec`: differential testing of cil-reader's flow layer against .NET.
//!
//! `flow-run` (readers/cil-reader/tools/FlowRun) calls every method of the
//! exec fixtures on .NET with random arguments and random statics, and
//! prints each call's starting state and a canonical text of its end state.
//! [`interp`] runs the same call from the same state over cil-reader's
//! `MethodFlow`, and this module renders its end state the same way. The two
//! texts must be equal.
//!
//! A wrong CFG, stack height, collision temporary, `leave`/`finally` edge or
//! handler wiring shows up either as a different answer or, earlier, as a
//! [`Stop::Bug`]: the interpreter moves only along the CFG and checks the
//! heights it reaches. Trials the interpreter does not model, or whose
//! result ECMA leaves unspecified, are counted, not compared; a method with
//! no compared trial at all fails the check, so the fixtures stay in what is
//! modeled.
//!
//! Every trial runs twice: over the CFG `MethodFlow::build` gives, and over
//! the one `split_after_throwing_in_try` gives, which ends a block after
//! every call and throw in a try.
//!
//! Everything is written under `<work>/cil-exec/`: `flow-run`'s output per
//! fixture (`<fixture>.jsonl`), every mismatch and bug in full
//! (`failures.txt`), and per-method counts (`summary.txt`).
//!
//! `flow-run` comes from `$FLOW_RUN`, else PATH (the dev shell and the
//! regression check have it); without it the check skips.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value as Json;

use cil_reader::Module;
use cil_reader::flow::FlowOptions;

use super::parallel_map;
use crate::exec::which;
use crate::regression::Outcome;

pub(super) mod interp;
pub(super) mod value;

use interp::{Interp, Obj, Prepared, Stop};
use value::{Base, Loc, Struct, Ty, Value};

pub const FLOW_RUN_ENV: &str = "FLOW_RUN";

/// The compiled fixtures this check runs.
pub const EXEC_FIXTURES: &[&str] = &["exec.cs.dll", "exec-debug.cs.dll", "exec.il.dll"];

/// Calls per method.
const TRIALS: u32 = 64;
const SEED: u32 = 1;

/// Failures listed in the outcome before the rest are counted.
const MAX_LISTED: usize = 10;

/// One call, as `flow-run` prints it.
#[derive(Deserialize)]
pub(super) struct Trial {
    pub method: u32,
    pub name: String,
    pub trial: u32,
    /// Measured by FlowRun: the reference JIT's ulong-to-double conversion.
    #[serde(rename = "uint64DoubleViaSigned")]
    pub uint64_double_via_signed: bool,
    pub args: Vec<Json>,
    pub statics: Vec<(u32, Json)>,
    pub heap: Vec<Json>,
    pub outcome: String,
}

/// Runs `flow-run` on one fixture, keeping its output in `dir`; the trials it printed.
pub(super) fn flow_run_trials(
    flow_run: &Path,
    path: &Path,
    dir: &Path,
    fixture: &str,
    filter: Option<&str>,
) -> Result<Vec<Trial>> {
    let mut cmd = Command::new(flow_run);
    cmd.arg("--trials")
        .arg(TRIALS.to_string())
        .arg("--seed")
        .arg(SEED.to_string());
    if let Some(f) = filter {
        cmd.arg("--filter").arg(f);
    }
    let output = cmd
        .arg(path)
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .output()
        .with_context(|| format!("running {}", flow_run.display()))?;
    std::fs::write(dir.join(format!("{fixture}.jsonl")), &output.stdout)?;
    std::fs::write(dir.join(format!("{fixture}.stderr")), &output.stderr)?;
    if !output.status.success() {
        anyhow::bail!(
            "flow-run failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    let mut trials = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        trials.push(serde_json::from_str(line).context("a flow-run line")?);
    }
    Ok(trials)
}

/// What became of one method's trials.
#[derive(Default)]
struct Counts {
    compared: usize,
    mismatched: usize,
    bugs: usize,
    unspecified: BTreeMap<String, usize>,
    unsupported: BTreeMap<String, usize>,
}

#[derive(Default)]
struct Report {
    methods: BTreeMap<String, Counts>,
    /// One line per failure, for the outcome.
    failures: Vec<String>,
    /// Every failure in full, for failures.txt.
    detail: String,
}

pub fn locate_flow_run() -> Option<PathBuf> {
    std::env::var_os(FLOW_RUN_ENV)
        .map(PathBuf::from)
        .or_else(|| which("flow-run"))
}

pub fn run(fixtures: &Path, work: &Path) -> Result<Outcome> {
    let Some(flow_run) = locate_flow_run() else {
        return Ok(Outcome::Skip(format!(
            "`flow-run` not on PATH and ${FLOW_RUN_ENV} unset; enter the dev shell or \
             `nix build .#flow-run`"
        )));
    };
    let dir = work.join("cil-exec");
    std::fs::create_dir_all(&dir)?;
    let reports = parallel_map(EXEC_FIXTURES, |fixture| {
        run_fixture(&flow_run, &fixtures.join(fixture), &dir, fixture, None)
    });

    let mut failures = Vec::new();
    let mut summary = String::new();
    let mut detail = String::new();
    for (fixture, report) in EXEC_FIXTURES.iter().zip(reports) {
        let report = match report {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{fixture}: {e:#}"));
                continue;
            }
        };
        let mut total = Counts::default();
        for (method, c) in &report.methods {
            let _ = writeln!(summary, "{fixture} {method} {}", c.describe());
            if c.compared == 0 && c.bugs == 0 {
                failures.push(format!(
                    "{fixture} {method}: no trial compared ({})",
                    c.describe()
                ));
            }
            total.add(c);
        }
        let _ = writeln!(summary, "{fixture} TOTAL {}", total.describe());
        failures.extend(report.failures.iter().map(|f| format!("{fixture} {f}")));
        let _ = write!(detail, "{}", report.detail);
    }
    std::fs::write(dir.join("summary.txt"), &summary)?;
    std::fs::write(dir.join("failures.txt"), &detail)?;
    if failures.is_empty() {
        return Ok(Outcome::Pass);
    }
    let mut msg = format!(
        "{} exec failure(s), all in {}:",
        failures.len(),
        dir.join("failures.txt").display()
    );
    for f in failures.iter().take(MAX_LISTED) {
        let _ = write!(msg, "\n{f}");
    }
    if failures.len() > MAX_LISTED {
        let _ = write!(msg, "\n... and {} more", failures.len() - MAX_LISTED);
    }
    Ok(Outcome::Fail(msg))
}

/// `cargo xtask cil exec`: the check on its own, optionally on the methods
/// whose name contains `filter`, with the report printed.
pub fn run_standalone(fixtures: &Path, work: &Path, filter: Option<&str>) -> Result<bool> {
    let flow_run = locate_flow_run().context("`flow-run` not found; enter the dev shell")?;
    let dir = work.join("cil-exec");
    std::fs::create_dir_all(&dir)?;
    let mut ok = true;
    for fixture in EXEC_FIXTURES {
        let report = run_fixture(&flow_run, &fixtures.join(fixture), &dir, fixture, filter)?;
        for (method, c) in &report.methods {
            println!("{fixture} {method} {}", c.describe());
        }
        for f in &report.failures {
            println!("FAIL {fixture} {f}");
            ok = false;
        }
        print!("{}", report.detail);
    }
    Ok(ok)
}

fn run_fixture(
    flow_run: &Path,
    path: &Path,
    dir: &Path,
    fixture: &str,
    filter: Option<&str>,
) -> Result<Report> {
    let trials = flow_run_trials(flow_run, path, dir, fixture, filter)?;
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let module = Module::read(&data).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut interps = [
        ("", Interp::new(&module, FlowOptions::default())),
        (
            " [split]",
            Interp::new(
                &module,
                FlowOptions {
                    split_after_throwing_in_try: true,
                },
            ),
        ),
    ];
    let mut report = Report::default();
    for trial in &trials {
        for (mode, interp) in &mut interps {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                interp.reset();
                replay(interp, trial)
            }));
            let result =
                result.unwrap_or_else(|_| Err(Stop::Bug("the interpreter panicked".into())));
            let at = format!("{} trial {}{mode}", trial.name, trial.trial);
            record(&mut report, fixture, trial, &at, result);
        }
    }
    Ok(report)
}

/// Count one replay of `trial`, and keep its failure if it is one.
fn record(
    report: &mut Report,
    fixture: &str,
    trial: &Trial,
    at: &str,
    result: Result<String, Stop>,
) {
    let counts = report.methods.entry(trial.name.clone()).or_default();
    match result {
        Ok(actual) if actual == trial.outcome => counts.compared += 1,
        Ok(actual) => {
            counts.compared += 1;
            counts.mismatched += 1;
            let (line, first) = first_difference(&trial.outcome, &actual);
            report
                .failures
                .push(format!("{at}: differs at line {line}: {first}"));
            let _ = write!(
                report.detail,
                "== MISMATCH {fixture} {at}\nargs: {}\nexpected (.NET):\n{}actual (interpreter):\n{}\n",
                serde_json::to_string(&trial.args).unwrap_or_default(),
                trial.outcome,
                actual
            );
        }
        Err(Stop::Bug(msg)) => {
            counts.bugs += 1;
            report.failures.push(format!("{at}: {msg}"));
            let _ = write!(
                report.detail,
                "== BUG {fixture} {at}\nargs: {}\n{msg}\nexpected (.NET):\n{}\n",
                serde_json::to_string(&trial.args).unwrap_or_default(),
                trial.outcome
            );
        }
        Err(Stop::Unspecified(why)) => *counts.unspecified.entry(why).or_default() += 1,
        Err(Stop::Unsupported(why)) => *counts.unsupported.entry(why).or_default() += 1,
        Err(Stop::Throw(_)) => unreachable!("replay renders exceptions"),
    }
}

/// The first line where the two texts differ (1-based), shown both ways.
fn first_difference(expected: &str, actual: &str) -> (usize, String) {
    let mut e = expected.lines();
    let mut a = actual.lines();
    let mut n = 1;
    loop {
        match (e.next(), a.next()) {
            (Some(x), Some(y)) if x == y => n += 1,
            (x, y) => {
                return (
                    n,
                    format!(
                        ".NET {:?}, interpreter {:?}",
                        x.unwrap_or(""),
                        y.unwrap_or("")
                    ),
                );
            }
        }
    }
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        self.compared += other.compared;
        self.mismatched += other.mismatched;
        self.bugs += other.bugs;
        for (k, v) in &other.unspecified {
            *self.unspecified.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.unsupported {
            *self.unsupported.entry(k.clone()).or_default() += v;
        }
    }

    fn describe(&self) -> String {
        let mut s = format!(
            "compared={} mismatched={} bugs={}",
            self.compared, self.mismatched, self.bugs
        );
        if !self.unspecified.is_empty() {
            let _ = write!(s, " unspecified={:?}", self.unspecified);
        }
        if !self.unsupported.is_empty() {
            let _ = write!(s, " unsupported={:?}", self.unsupported);
        }
        s
    }
}

// ---- replay --------------------------------------------------------------------

/// Run one trial from its starting state; the end state's canonical text.
fn replay(interp: &mut Interp<'_, '_>, trial: &Trial) -> Result<String, Stop> {
    let (p, args) = setup(interp, trial)?;
    let original = args.clone();
    let result = match interp.call(trial.method, args) {
        Ok(v) => Ok(v),
        Err(Stop::Throw(e)) => Err(e),
        Err(other) => return Err(other),
    };
    render(interp, trial, &p, &original, &result)
}

/// A trial's starting state, built in `interp`: the method, and its arguments (`this` first,
/// a `ref`/`out` argument as a pointer to its cell).
pub(super) fn setup(
    interp: &mut Interp<'_, '_>,
    trial: &Trial,
) -> Result<(std::rc::Rc<Prepared>, Vec<Value>), Stop> {
    let bad = |what: &str| Stop::Bug(format!("flow-run printed a bad {what}"));
    interp.uint64_double_via_signed = trial.uint64_double_via_signed;
    let p = interp.prepare(trial.method)?;

    // The heap first, as placeholders, so values can refer to any object.
    for _ in &trial.heap {
        interp.heap.push(Obj::Object {
            ty: String::new(),
            fields: Vec::new(),
        });
    }
    for (i, h) in trial.heap.iter().enumerate() {
        let o = if let Some(ty) = h.get("t").and_then(Json::as_str) {
            let fields = decode_fields(interp, h.get("f"))?;
            Obj::Object {
                ty: ty.to_string(),
                fields,
            }
        } else if let Some(code) = h.get("a").and_then(Json::as_str) {
            let elem = interp.ty_of_code(code)?;
            let dims = h
                .get("n")
                .and_then(Json::as_array)
                .ok_or_else(|| bad("array"))?
                .iter()
                .map(|d| d.as_u64().map(|d| d as usize))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| bad("array length"))?;
            let mut items = Vec::new();
            for v in h
                .get("v")
                .and_then(Json::as_array)
                .ok_or_else(|| bad("array"))?
            {
                items.push(decode(interp, v, &elem)?);
            }
            Obj::Array {
                elem,
                code: code.to_string(),
                dims,
                items,
            }
        } else if let Some(ty) = h.get("b").and_then(Json::as_str) {
            let vty = interp.ty_of_boxed(ty)?;
            let value = decode(interp, h.get("v").ok_or_else(|| bad("box"))?, &vty)?;
            Obj::Boxed {
                ty: ty.to_string(),
                vty,
                value,
            }
        } else {
            return Err(bad("heap entry"));
        };
        interp.heap[i] = o;
    }
    for (f, v) in &trial.statics {
        let ty = interp.field_ty(*f)?;
        let v = decode(interp, v, &ty)?;
        interp.statics.insert(*f, v);
    }
    if trial.args.len() != p.params.len() {
        return Err(bad("argument count"));
    }
    let mut args = Vec::new();
    for (k, a) in trial.args.iter().enumerate() {
        match &p.byref[k] {
            Some(pointee) => {
                let v = decode(interp, a, pointee)?;
                interp.cells.push((pointee.clone(), v));
                args.push(Value::Ptr(Loc {
                    base: Base::Cell(interp.cells.len() - 1),
                    path: Vec::new(),
                }));
            }
            None => args.push(decode(interp, a, &p.params[k])?),
        }
    }
    interp.sync_heap();
    Ok((p, args))
}

/// The canonical text of a trial's end state, as FlowRun prints it.
pub(super) fn render(
    interp: &mut Interp<'_, '_>,
    trial: &Trial,
    p: &Prepared,
    original: &[Value],
    result: &Result<Option<Value>, Value>,
) -> Result<String, Stop> {
    let mut canon = Canon::default();
    let mut text = String::new();
    match result {
        Err(e) => {
            let _ = writeln!(text, "throw {}", canon.value(interp, e, &Ty::Ref)?);
        }
        Ok(None) => text.push_str("ret void\n"),
        Ok(Some(v)) => {
            let ty = p
                .ret
                .clone()
                .ok_or_else(|| Stop::Bug("a void method returned".into()))?;
            let _ = writeln!(text, "ret {}", canon.value(interp, v, &ty)?);
        }
    }
    let first = if p.has_this {
        let _ = writeln!(
            text,
            "this {}",
            canon.value(interp, &original[0], &Ty::Ref)?
        );
        1
    } else {
        0
    };
    let mut cell = 0;
    for (k, arg) in original.iter().enumerate().skip(first) {
        let rendered = match &p.byref[k] {
            Some(_) => {
                let (ty, v) = interp.cells[cell].clone();
                cell += 1;
                if result.is_err() {
                    "-".to_string()
                } else {
                    canon.value(interp, &v, &ty)?
                }
            }
            None => canon.value(interp, arg, &p.params[k])?,
        };
        let _ = writeln!(text, "arg{} {rendered}", k - first);
    }
    for (f, _) in &trial.statics {
        let ty = interp.field_ty(*f)?;
        let v = interp.statics[f].clone();
        let _ = writeln!(text, "static {f:08x} {}", canon.value(interp, &v, &ty)?);
    }
    canon.heap(interp, &mut text)?;
    Ok(text)
}

fn decode_fields(interp: &mut Interp<'_, '_>, j: Option<&Json>) -> Result<Vec<(u32, Value)>, Stop> {
    let bad = || Stop::Bug("flow-run printed bad fields".into());
    let mut fields = Vec::new();
    for pair in j.and_then(Json::as_array).ok_or_else(bad)? {
        let (Some(f), Some(v)) = (pair.get(0).and_then(Json::as_u64), pair.get(1)) else {
            return Err(bad());
        };
        let f = f as u32;
        let ty = interp.field_ty(f)?;
        fields.push((f, decode(interp, v, &ty)?));
    }
    Ok(fields)
}

/// A value as flow-run prints it, read as a location of type `ty` holds it.
fn decode(interp: &mut Interp<'_, '_>, j: &Json, ty: &Ty) -> Result<Value, Stop> {
    let bad = || Stop::Bug(format!("flow-run printed {j} for a {ty:?}"));
    let int = || j.as_i64().or_else(|| j.as_u64().map(|u| u as i64));
    let bits = |key: &str| j.get(key).and_then(Json::as_u64);
    let v = match ty {
        Ty::Bool | Ty::Char | Ty::I1 | Ty::U1 | Ty::I2 | Ty::U2 | Ty::I4 | Ty::U4 => {
            Value::I4(int().ok_or_else(bad)? as i32)
        }
        Ty::I8 | Ty::U8 => Value::I8(int().ok_or_else(bad)?),
        Ty::I | Ty::U => Value::I(int().ok_or_else(bad)?),
        Ty::R4 => Value::R4(f32::from_bits(bits("f").ok_or_else(bad)? as u32)),
        Ty::R8 => Value::R8(f64::from_bits(bits("d").ok_or_else(bad)?)),
        Ty::Ref if j.is_null() => Value::Null,
        Ty::Ref => Value::Ref(j.get("r").and_then(Json::as_u64).ok_or_else(bad)? as usize),
        Ty::Struct(t) => Value::Struct(Struct {
            ty: *t,
            fields: decode_fields(interp, j.get("s"))?,
        }),
        Ty::Ptr => return Err(bad()),
    };
    ty.store(v)
        .map_err(|e| Stop::Bug(format!("decoding {j}: {e}")))
}

/// The canonical text of an end state, as FlowRun's `Canon` renders it.
#[derive(Default)]
struct Canon {
    ids: BTreeMap<usize, usize>,
    order: Vec<usize>,
}

impl Canon {
    fn value(&mut self, interp: &mut Interp<'_, '_>, v: &Value, ty: &Ty) -> Result<String, Stop> {
        let bad = || Stop::Bug(format!("a {} where a {ty:?} belongs", v.describe()));
        Ok(match (ty, v) {
            (Ty::Bool, Value::I4(x)) => if *x != 0 { "1" } else { "0" }.to_string(),
            (Ty::Char | Ty::U1 | Ty::U2 | Ty::I1 | Ty::I2 | Ty::I4, Value::I4(x)) => x.to_string(),
            (Ty::U4, Value::I4(x)) => (*x as u32).to_string(),
            (Ty::I8, Value::I8(x)) | (Ty::I, Value::I(x)) => x.to_string(),
            (Ty::U8, Value::I8(x)) | (Ty::U, Value::I(x)) => (*x as u64).to_string(),
            (Ty::R4, Value::R4(x)) if x.is_nan() => "fNaN".to_string(),
            (Ty::R4, Value::R4(x)) => format!("f{:08x}", x.to_bits()),
            (Ty::R8, Value::R8(x)) if x.is_nan() => "dNaN".to_string(),
            (Ty::R8, Value::R8(x)) => format!("d{:016x}", x.to_bits()),
            (Ty::Struct(_), Value::Struct(s)) => {
                let mut parts = Vec::new();
                for (f, x) in &s.fields {
                    let fty = interp.field_ty(*f)?;
                    parts.push(format!("{f:08x}={}", self.value(interp, x, &fty)?));
                }
                format!("{{{}}}", parts.join(","))
            }
            (Ty::Ref, Value::Null) => "null".to_string(),
            (Ty::Ref, Value::Ref(o)) => {
                let next = self.order.len();
                let id = *self.ids.entry(*o).or_insert(next);
                if id == next {
                    self.order.push(*o);
                }
                format!("@{id}")
            }
            _ => return Err(bad()),
        })
    }

    /// Every object reached, in the order first reached, rendering each
    /// one's fields (which may reach more).
    fn heap(&mut self, interp: &mut Interp<'_, '_>, text: &mut String) -> Result<(), Stop> {
        let mut i = 0;
        while i < self.order.len() {
            let o = interp.heap[self.order[i]].clone();
            let line = match &o {
                Obj::Object { ty, fields } => {
                    let mut parts = Vec::new();
                    for (f, x) in fields {
                        let fty = interp.field_ty(*f)?;
                        parts.push(format!("{f:08x}={}", self.value(interp, x, &fty)?));
                    }
                    format!("{ty} {{{}}}", parts.join(","))
                }
                Obj::Array {
                    elem,
                    code,
                    dims,
                    items,
                } => {
                    let dims: Vec<String> = dims.iter().map(|d| d.to_string()).collect();
                    let mut parts = Vec::new();
                    for x in items {
                        parts.push(self.value(interp, x, elem)?);
                    }
                    format!("{code}[{}] {}", dims.join("x"), parts.join(","))
                }
                Obj::Boxed { ty, vty, value } => {
                    format!("box {ty} {}", self.value(interp, value, vty)?)
                }
                // A delegate declares no fields the module's types do.
                Obj::Delegate { ty, .. } => format!("{ty} {{}}"),
            };
            let _ = writeln!(text, "@{i} {line}");
            i += 1;
        }
        Ok(())
    }
}
