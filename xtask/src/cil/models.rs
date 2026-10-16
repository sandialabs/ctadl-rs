//! `cil:models`: the default .NET models (`ctadl-ascent/src/models/defaults/cil-index.jsonl`)
//! checked against what .NET does.
//!
//! **Which methods.** Every class a generator names in `parents` is listed by `model-run`
//! (`frontends/ctadl-cil/tools/ModelRun`), which spells each public method and constructor by
//! reflection as the CIL frontend spells it. Those rows become a `VirtualMethodTable::Cil`, and
//! the shipped file is matched against it by the loader `ctadl index` uses, so the methods
//! checked are exactly the overloads the generators select, and the ports are the ones the
//! loader decodes. A class that does not load, or lives in another assembly than the one named
//! (the frontend canonicalizes through forwarders), fails, as does a generator that selects
//! nothing.
//!
//! **What they do.** `model-run` calls each selected method with marked arguments, many times,
//! and reports two things: a *flow*, a marker from input `(i, p)` found at output `(j, q)` that
//! was not there before the call; and a *dependence*, input `i` redrawn alone changed output
//! `j`. Paths are the models' abstraction: `[]` an element, `deref` a pointee, and a library
//! object's insides are the object.
//!
//! **Comparison.** A flow is covered by a modeled pair `(i, p') -> (j, q')` with `p = p'·s` and
//! `q = q'·s`, as in `cil:taint`, so a model of the whole object covers its elements. An
//! uncovered flow fails, unless an entry of `xtask/cil-models-known.json5` accepts it. A modeled
//! pair that covers no flow and has no dependence behind it in any trial is *unconfirmed*: it is
//! listed, not failed, since a pair may be right and only out of the runner's reach (an index
//! the trials never hit, a lambda that drops its argument). A method no trial could call is
//! listed with the reason.
//!
//! Everything is written under `<work>/cil-models/`: the listing, the selected methods with
//! their modeled pairs, `model-run`'s rows, and `report.txt`, one block per method.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use ctadl_ascent::models::{
    try_check_jsonl_models, FormalIndexTypeTag, ImportScope, MatchedFunctions, ModelPort,
    ProgramMatchIndex, ProgramModelMatches, CIL_DEFAULT_MODELS,
};
use ctadl_ir::mir::call::{CilClass, CilMethod, CilSignature, CilSimpleName, VirtualMethodTable};
use ctadl_ir::mir::{
    BasicBlockData, FunctionData, Functions, ParameterType, PathSegment, Program, ProgramInfo,
    Statement, StatementKind,
};
use serde::Deserialize;
use serde_json::Value;

use crate::regression::Outcome;

/// A `model-run` launcher to use instead of looking on PATH.
pub const MODEL_RUN_ENV: &str = "CTADL_MODEL_RUN";
pub const KNOWN_FILE: &str = "cil-models-known.json5";
const TRIALS: u32 = 32;
const SEED: u32 = 1;
/// Failures listed in the outcome before the rest are counted.
const MAX_LISTED: usize = 12;

/// A port as a model numbers it: `Argument(n)` is `n`, `Return` is -1.
type Port = i32;
/// A port and a path, the path as segments: `["deref", "[]"]`.
type Loc = (Port, Vec<String>);

#[derive(Deserialize)]
struct Row {
    cls: String,
    fid: Option<String>,
    simple: Option<String>,
    desc: Option<String>,
    arity: Option<usize>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct Run {
    fid: String,
    #[serde(default)]
    ran: u32,
    #[serde(default)]
    threw: BTreeMap<String, u32>,
    skip: Option<String>,
    #[serde(default)]
    flows: Vec<Flow>,
    #[serde(default)]
    depends: Vec<(Port, Port)>,
}

#[derive(Deserialize)]
struct Flow {
    from: Vec<Loc>,
    to: Loc,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Known {
    /// Classes `model-run` cannot load because they are not part of .NET.
    #[serde(default)]
    unavailable: Vec<Unavailable>,
    /// Flows .NET shows that the models leave out on purpose.
    #[serde(default)]
    flows: Vec<KnownFlow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unavailable {
    cls: String,
    why: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnownFlow {
    /// A regex over the function id.
    fid: String,
    /// `src -> dst` in port spelling, `Argument(0).\[] -> Return`; `src` is any of the flow's
    /// alternatives.
    flow: String,
    why: String,
}

/// The `model-run` command: a launcher, or `dotnet` and the built `ModelRun.dll`.
pub fn locate_model_run(xtask_dir: &Path, work: &Path) -> Result<Option<Vec<OsString>>> {
    if let Some(path) = std::env::var_os(MODEL_RUN_ENV) {
        return Ok(Some(vec![path]));
    }
    if let Some(path) = crate::exec::which("model-run") {
        return Ok(Some(vec![path.into_os_string()]));
    }
    // Outside the dev shell's launcher, build it, as long as there is a `dotnet` to build with.
    let Some(dotnet) = crate::exec::which("dotnet") else {
        return Ok(None);
    };
    let source = xtask_dir.join("../frontends/ctadl-cil/tools/ModelRun");
    if !source.join("ModelRun.csproj").is_file() {
        return Ok(None);
    }
    let out = work.join("model-run-build");
    let mut cmd = Command::new(&dotnet);
    cmd.current_dir(&source)
        .args(["build", "-c", "Release", "--nologo", "-v", "q", "-p:UseAppHost=false", "-o"])
        .arg(&out)
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1");
    crate::exec::run_checked(cmd, "dotnet build ModelRun")?;
    Ok(Some(vec![
        dotnet.into_os_string(),
        out.join("ModelRun.dll").into_os_string(),
    ]))
}

/// Runs `model-run` in `sandbox`, an empty directory: a library call given a marker for a file
/// name must not leave it anywhere that matters, and what it leaves there fails the check.
fn model_run(
    cmd: &[OsString],
    args: &[&str],
    input: &Path,
    out: &Path,
    sandbox: &Path,
) -> Result<String> {
    let input = std::path::absolute(input)?;
    let output = Command::new(&cmd[0])
        .current_dir(sandbox)
        .args(&cmd[1..])
        .args(args)
        .arg(input)
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .output()
        .context("running model-run")?;
    std::fs::write(out, &output.stdout)?;
    std::fs::write(out.with_extension("stderr"), &output.stderr)?;
    if !output.status.success() {
        bail!(
            "model-run {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn run(xtask_dir: &Path, work: &Path) -> Result<Outcome> {
    let dir = work.join("cil-models");
    std::fs::create_dir_all(&dir)?;
    let Some(cmd) = locate_model_run(xtask_dir, &dir)? else {
        return Ok(Outcome::Skip(format!(
            "`model-run` not on PATH, ${MODEL_RUN_ENV} unset, and no `dotnet` to build it with; \
             enter the dev shell"
        )));
    };
    let known_path = xtask_dir.join(KNOWN_FILE);
    let known: Known = if known_path.is_file() {
        json5::from_str(&std::fs::read_to_string(&known_path)?)
            .with_context(|| format!("parsing {}", known_path.display()))?
    } else {
        Known::default()
    };
    let unavailable: BTreeSet<&str> = known.unavailable.iter().map(|u| u.cls.as_str()).collect();
    for u in &known.unavailable {
        if u.why.trim().is_empty() {
            bail!("{}: `{}` has no `why`", known_path.display(), u.cls);
        }
    }
    let known_flows = known
        .flows
        .iter()
        .map(|k| {
            if k.why.trim().is_empty() {
                bail!("{}: `{}` has no `why`", known_path.display(), k.flow);
            }
            Ok((regex::Regex::new(&k.fid)?, k))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut failures: Vec<String> = Vec::new();

    // ---- which methods
    let (name, contents) = CIL_DEFAULT_MODELS;
    let generators = generator_lines(contents)?;
    let classes: BTreeSet<String> = generators.iter().flat_map(|g| parents(g)).collect();
    let classes_file = dir.join("classes.txt");
    std::fs::write(
        &classes_file,
        classes.iter().map(|c| format!("{c}\n")).collect::<String>(),
    )?;
    let sandbox = dir.join("sandbox");
    if sandbox.exists() {
        std::fs::remove_dir_all(&sandbox)?;
    }
    std::fs::create_dir_all(&sandbox)?;
    let listing = model_run(
        &cmd,
        &["list"],
        &classes_file,
        &dir.join("listing.jsonl"),
        &sandbox,
    )?;
    let mut rows = Vec::new();
    for line in listing.lines() {
        let row: Row = serde_json::from_str(line).context("a model-run list row")?;
        if let Some(error) = &row.error {
            if !unavailable.contains(row.cls.as_str()) {
                failures.push(format!("{}: {error}", row.cls));
            }
            continue;
        }
        rows.push(row);
    }
    for cls in &unavailable {
        if !listing.contains(&format!("\"cls\":{}", serde_json::to_string(cls)?)) {
            failures.push(format!("{KNOWN_FILE}: unavailable class {cls} is not modeled"));
        }
    }

    // ---- what the loader matches
    let arity: BTreeMap<&str, usize> = rows
        .iter()
        .map(|r| (r.fid.as_deref().unwrap(), r.arity.unwrap_or(0)))
        .collect();
    let program_info = program(&rows);
    let index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    let (report, errors) =
        try_check_jsonl_models(&index, BufReader::new(contents), usize::MAX, &mut matches);
    if let Some(e) = errors.first() {
        bail!("{name} does not load: {e:?}");
    }
    for (n, g) in generators.iter().enumerate() {
        let live = match report.matched.get(&n) {
            Some(MatchedFunctions::All) => true,
            Some(MatchedFunctions::Some { total, .. }) => *total > 0,
            None => false,
        };
        let only_unavailable = parents(g).iter().all(|c| unavailable.contains(c.as_str()));
        if !live && !only_unavailable {
            failures.push(format!("{name}: generator {n} selects no method: {g}"));
        }
    }
    let mut modeled: BTreeMap<String, BTreeSet<(Loc, Loc)>> = BTreeMap::new();
    for p in &matches.propagations {
        let fid = p.function.to_string();
        let n = arity.get(fid.as_str()).copied().unwrap_or(0);
        for src in expand(&p.src, n) {
            for dst in expand(&p.dst, n) {
                modeled
                    .entry(fid.clone())
                    .or_default()
                    .insert((src.clone(), dst));
            }
        }
    }
    let mut selected = String::new();
    for (fid, pairs) in &modeled {
        let _ = writeln!(selected, "{fid}");
        for (s, d) in pairs {
            let _ = writeln!(selected, "    {} -> {}", show(s), show(d));
        }
    }
    std::fs::write(dir.join("modeled.txt"), selected)?;

    // ---- what .NET does
    let fids_file = dir.join("fids.txt");
    std::fs::write(
        &fids_file,
        modeled.keys().map(|f| format!("{f}\n")).collect::<String>(),
    )?;
    let runs = model_run(
        &cmd,
        &["run", "--trials", &TRIALS.to_string(), "--seed", &SEED.to_string()],
        &fids_file,
        &dir.join("runs.jsonl"),
        &sandbox,
    )?;
    let litter: Vec<String> = std::fs::read_dir(&sandbox)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if !litter.is_empty() {
        failures.push(format!(
            "model-run wrote {} file(s) into its working directory, e.g. {:?}; a generated \
             argument reached the file system",
            litter.len(),
            &litter[..litter.len().min(3)]
        ));
    }

    // ---- comparison
    let mut known_hits = vec![0usize; known_flows.len()];
    let mut text = String::new();
    let (mut checked, mut unexercised, mut unconfirmed, mut confirmed, mut missing) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut skips: BTreeMap<String, usize> = BTreeMap::new();
    for line in runs.lines() {
        let run: Run = serde_json::from_str(line).context("a model-run run row")?;
        let pairs = &modeled[&run.fid];
        let _ = writeln!(text, "{}", run.fid);
        if run.ran == 0 {
            unexercised += 1;
            let why = run.skip.clone().unwrap_or_else(|| {
                format!(
                    "every trial threw: {}",
                    run.threw
                        .iter()
                        .map(|(e, n)| format!("{e} x{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
            let _ = writeln!(text, "  unexercised: {why}");
            *skips.entry(why.split(':').next().unwrap().to_string()).or_default() += 1;
            continue;
        }
        checked += 1;
        let _ = writeln!(text, "  ran {} of {} trials", run.ran, TRIALS);
        let mut used: BTreeSet<&(Loc, Loc)> = BTreeSet::new();
        for flow in &run.flows {
            let covering: Vec<&(Loc, Loc)> = pairs
                .iter()
                .filter(|(s, d)| flow.from.iter().any(|f| covers(s, d, f, &flow.to)))
                .collect();
            if !covering.is_empty() {
                used.extend(covering);
                continue;
            }
            let rendered: Vec<String> = flow
                .from
                .iter()
                .map(|f| format!("{} -> {}", show(f), show(&flow.to)))
                .collect();
            let accepted = known_flows.iter().position(|(rx, k)| {
                rx.is_match(&run.fid) && rendered.iter().any(|r| *r == k.flow)
            });
            match accepted {
                Some(i) => {
                    known_hits[i] += 1;
                    let _ = writeln!(text, "  known:   {}", rendered.join(" | "));
                }
                None => {
                    missing += 1;
                    let _ = writeln!(text, "  MISSING: {}", rendered.join(" | "));
                    failures.push(format!("{}: {} is not modeled", run.fid, rendered.join(" | ")));
                }
            }
        }
        for pair in pairs {
            let (s, d) = pair;
            let n = arity.get(run.fid.as_str()).copied().unwrap_or(0) as Port;
            if s.0 >= n || d.0 >= n {
                continue;
            }
            if used.contains(pair) || run.depends.contains(&(s.0, d.0)) {
                confirmed += 1;
                let _ = writeln!(text, "  ok:          {} -> {}", show(s), show(d));
            } else {
                unconfirmed += 1;
                let _ = writeln!(text, "  unconfirmed: {} -> {}", show(s), show(d));
            }
        }
    }
    for (i, (_, k)) in known_flows.iter().enumerate() {
        if known_hits[i] == 0 {
            failures.push(format!(
                "{KNOWN_FILE}: `{}` on `{}` matches no unmodeled flow (XPASS); remove it",
                k.flow, k.fid
            ));
        }
    }
    let summary = format!(
        "{} methods selected; {checked} called, {unexercised} not ({}); modeled pairs: \
         {confirmed} confirmed, {unconfirmed} unconfirmed; {missing} flows unmodeled",
        modeled.len(),
        skips
            .iter()
            .map(|(k, n)| format!("{k}: {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::fs::write(dir.join("report.txt"), format!("{summary}\n\n{text}"))?;
    println!("cil:models: {summary} (see {})", dir.join("report.txt").display());
    if failures.is_empty() {
        return Ok(Outcome::Pass);
    }
    let n = failures.len();
    let mut why = failures
        .into_iter()
        .take(MAX_LISTED)
        .collect::<Vec<_>>()
        .join("\n");
    if n > MAX_LISTED {
        let _ = write!(why, "\n... and {} more", n - MAX_LISTED);
    }
    let _ = write!(why, "\n{summary}; see {}", dir.join("report.txt").display());
    Ok(Outcome::Fail(why))
}

/// The generator lines of a JSONL model file, in generator-index order.
fn generator_lines(contents: &[u8]) -> Result<Vec<String>> {
    Ok(std::str::from_utf8(contents)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(str::to_string)
        .collect())
}

/// The classes a generator's `signature_match` constraints name.
fn parents(generator: &str) -> Vec<String> {
    let value: Value = serde_json::from_str(generator).unwrap_or_default();
    let mut out = Vec::new();
    for w in value["where"].as_array().into_iter().flatten() {
        if let Some(p) = w["parent"].as_str() {
            out.push(p.to_string());
        }
        for p in w["parents"].as_array().into_iter().flatten() {
            out.extend(p.as_str().map(str::to_string));
        }
    }
    out
}

/// A program whose method table is the listing: one function per method, with its arity.
fn program(rows: &[Row]) -> ProgramInfo {
    let methods: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                CilClass(r.cls.as_str().into()),
                CilSimpleName(r.simple.as_deref().unwrap_or_default().into()),
                CilSignature(r.desc.as_deref().unwrap_or_default().into()),
                CilMethod(r.fid.as_deref().unwrap_or_default().into()),
            )
        })
        .collect();
    let functions = rows.iter().map(|r| {
        let mut f = FunctionData::default();
        f.set_name(r.fid.clone().unwrap_or_default());
        for _ in 0..r.arity.unwrap_or(0) {
            f.params.parameters.push(ParameterType::ByRef);
        }
        let blocks = f.blocks.blocks_mut();
        let body = blocks.push(BasicBlockData::new(None));
        blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
        f
    });
    ProgramInfo {
        vmt: VirtualMethodTable::Cil {
            methods,
            hierarchy: Default::default(),
            interfaces: Default::default(),
            abstract_methods: Default::default(),
            pinvokes: Default::default(),
        },
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

/// A model port as the locations it names; `Argument(*)` is every argument.
fn expand(port: &ModelPort, arity: usize) -> Vec<Loc> {
    let path: Vec<String> = port
        .path
        .iter()
        .map(|seg| match seg {
            PathSegment::Symbol(s) => {
                let s: &str = s.as_ref();
                s.to_string()
            }
            other => other.to_string(),
        })
        .collect();
    match port.tag {
        FormalIndexTypeTag::Return => vec![(-1, path)],
        FormalIndexTypeTag::Index => vec![(port.index.unwrap_or(0) as Port, path)],
        FormalIndexTypeTag::AnyArgument => (0..arity as Port).map(|i| (i, path.clone())).collect(),
        _ => Vec::new(),
    }
}

/// Whether the pair `(s, d)` covers the flow `f -> t`: the same ports, and the flow's paths are
/// the pair's extended by one suffix.
fn covers(s: &Loc, d: &Loc, f: &Loc, t: &Loc) -> bool {
    s.0 == f.0
        && d.0 == t.0
        && f.1.starts_with(&s.1)
        && t.1.starts_with(&d.1)
        && f.1[s.1.len()..] == t.1[d.1.len()..]
}

/// A location in port spelling: `Argument(0).\[]`, `Return.deref`.
fn show(l: &Loc) -> String {
    let mut s = if l.0 < 0 {
        "Return".to_string()
    } else {
        format!("Argument({})", l.0)
    };
    for seg in &l.1 {
        s.push('.');
        s.push_str(&ctadl_ir::mir::path_syntax::segment_to_string(
            &PathSegment::symbol(seg.as_str()),
        ));
    }
    s
}

pub fn resolve_xtask_dir() -> PathBuf {
    super::taint::resolve_xtask_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(port: Port, path: &[&str]) -> Loc {
        (port, path.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_pair_covers_its_extensions_by_one_suffix() {
        // `Argument(0) -> Return` covers the elements too ...
        assert!(covers(&loc(0, &[]), &loc(-1, &[]), &loc(0, &["[]"]), &loc(-1, &["[]"])));
        // ... but does not take an element out of its collection.
        assert!(!covers(&loc(0, &[]), &loc(-1, &[]), &loc(0, &["[]"]), &loc(-1, &[])));
        assert!(covers(&loc(0, &["[]"]), &loc(-1, &[]), &loc(0, &["[]"]), &loc(-1, &[])));
        assert!(!covers(&loc(0, &["[]"]), &loc(-1, &[]), &loc(1, &["[]"]), &loc(-1, &[])));
    }

    #[test]
    fn locations_print_in_port_spelling() {
        assert_eq!(show(&loc(0, &["deref", "[]"])), r"Argument(0).deref.\[]");
        assert_eq!(show(&loc(-1, &[])), "Return");
    }
}
