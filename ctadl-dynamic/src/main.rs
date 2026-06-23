//! Case runner for the DFSan-dynamic vs. CTADL-static taint comparison harness.
//!
//! For each case under `cases/`, this:
//!   1. runs CTADL's static taint analysis ([`analyze_c_flows`]) on `prog.c` +
//!      the inert static markers,
//!   2. compiles `prog.c` + the **instrumented** DFSan shim with
//!      `clang -fsanitize=dataflow`, runs it (ASLR disabled via `setarch -R`),
//!      and reads the observed runtime taint,
//!   3. classifies static vs. dynamic, with the hand-authored manifest oracle as
//!      a cross-check, and the manifest's `known_gap` allowlist separating
//!      intentionally-preserved findings from new regressions.
//!
//! DFSan observation is the ground truth: `dynamic=flow & static=none` is a
//! soundness gap (CTADL missed a flow that genuinely happens at runtime).
//!
//! Output / exit code (designed to drive an automated loop):
//!   - default: a human-readable table + summary on stdout.
//!   - `--json`: a machine-readable report on stdout (table+summary go to stderr).
//!   - exit 0 iff nothing *unexpected* happened: no NEW (un-allowlisted) soundness
//!     gaps and no oracle mismatches. Exit 1 otherwise; exit 2 on a harness error.
//!     A `known_gap` that stops failing is surfaced as `resolved-known-gap` (the
//!     "your fix worked" signal) but does not by itself fail the run.
//!
//! Run with: `cargo run -p ctadl-dynamic [-- --json]` (`RUST_LOG=debug` for detail).
//! Requires `clang` with the dataflow sanitizer runtime and `setarch`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use ctadl_ascent::taint_compare::analyze_c_flows;
use serde::{Deserialize, Serialize};

/// Per-case ground truth, authored by hand alongside `prog.c`.
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    description: String,
    #[serde(default = "default_label")]
    label: String,
    /// Whether taint truly flows from source to sink in this program.
    expect_flow: bool,
    /// If set, this case is a known, intentionally-preserved soundness gap (the
    /// value is the finding id, e.g. "F1", documented in KNOWN_FINDINGS.md). A
    /// soundness gap here is expected (not a failure); if the gap *disappears*,
    /// the run reports `resolved-known-gap` so the allowlist can be cleaned up.
    #[serde(default)]
    known_gap: Option<String>,
    /// If set, the CTADL **tree-sitter C frontend is expected to fail to ingest**
    /// this program (value names the construct, e.g. "array_declarator"). The
    /// frontend is incomplete; this allowlists known ingestion gaps so an
    /// *un-allowlisted* frontend error is treated as a new finding. If the
    /// frontend later parses it, the run reports `resolved-known-frontend-gap`.
    #[serde(default)]
    known_frontend_gap: Option<String>,
}

fn default_label() -> String {
    "Test".to_string()
}

/// Per-case classification along the static-vs-dynamic axis (oracle mismatch is
/// tracked separately on [`Row`]).
#[derive(Debug)]
enum Status {
    /// Static and dynamic agree.
    Ok,
    /// Soundness gap on an allowlisted case (carries the finding id). Expected.
    KnownGap(String),
    /// Soundness gap on a case NOT allowlisted — a new/regressed finding.
    NewGap,
    /// Allowlisted as a known gap, but no gap was observed — the gap is gone
    /// (likely fixed); the allowlist entry is now stale.
    ResolvedKnownGap(String),
    /// CTADL reported a flow that did not occur at runtime — imprecision.
    PrecisionGap,
    /// CTADL's C frontend failed to ingest the program, and it is NOT allowlisted
    /// — a new ingestion gap (the frontend is incomplete; this is a finding).
    FrontendError(String),
    /// Frontend ingestion failure on an allowlisted case (`known_frontend_gap`).
    KnownFrontendGap(String),
    /// Allowlisted as a frontend gap, but the frontend ingested it this time — the
    /// gap is gone (parser improved); the allowlist entry is now stale.
    ResolvedKnownFrontendGap(String),
    /// DFSan could not compile/run the case (no dynamic ground truth).
    DynError(String),
}

impl Status {
    /// Stable kebab-case key for machine output.
    fn kind(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::KnownGap(_) => "known-gap",
            Status::NewGap => "new-gap",
            Status::ResolvedKnownGap(_) => "resolved-known-gap",
            Status::PrecisionGap => "precision-gap",
            Status::FrontendError(_) => "frontend-error",
            Status::KnownFrontendGap(_) => "known-frontend-gap",
            Status::ResolvedKnownFrontendGap(_) => "resolved-known-frontend-gap",
            Status::DynError(_) => "dyn-error",
        }
    }
    /// Human display, including finding id / error detail.
    fn display(&self) -> String {
        match self {
            Status::Ok => "OK".to_string(),
            Status::KnownGap(id) => format!("known-gap ({id})"),
            Status::NewGap => "NEW-GAP".to_string(),
            Status::ResolvedKnownGap(id) => format!("RESOLVED-KNOWN-GAP ({id})"),
            Status::PrecisionGap => "precision-gap".to_string(),
            Status::FrontendError(m) => format!("FRONTEND-ERROR ({m})"),
            Status::KnownFrontendGap(id) => format!("known-frontend-gap ({id})"),
            Status::ResolvedKnownFrontendGap(id) => format!("RESOLVED-KNOWN-FRONTEND-GAP ({id})"),
            Status::DynError(m) => format!("dyn-error ({m})"),
        }
    }
    /// Extra detail for the JSON `detail` field (finding id or error message).
    fn detail(&self) -> Option<String> {
        match self {
            Status::KnownGap(id)
            | Status::ResolvedKnownGap(id)
            | Status::KnownFrontendGap(id)
            | Status::ResolvedKnownFrontendGap(id) => Some(id.clone()),
            Status::FrontendError(m) | Status::DynError(m) => Some(m.clone()),
            _ => None,
        }
    }
}

struct Row {
    name: String,
    compiles: bool,
    oracle: bool,
    static_flow: Option<bool>,
    dynamic_flow: Option<bool>,
    status: Status,
    known_gap: Option<String>,
    known_frontend_gap: Option<String>,
    oracle_mismatch: bool,
}

#[derive(Serialize)]
struct CaseReport {
    name: String,
    /// manifest `expect_flow` (hand-authored).
    oracle: bool,
    /// CTADL static result (None on frontend error).
    static_flow: Option<bool>,
    /// DFSan runtime observation (None on dyn error) — the ground truth.
    dynamic_flow: Option<bool>,
    status: &'static str,
    known_gap: Option<String>,
    known_frontend_gap: Option<String>,
    oracle_mismatch: bool,
    detail: Option<String>,
}

#[derive(Default, Serialize)]
struct Summary {
    total: usize,
    ok: usize,
    known_gap: usize,
    new_gap: usize,
    resolved_known_gap: usize,
    precision_gap: usize,
    oracle_mismatch: usize,
    /// Un-allowlisted frontend ingestion failures (a finding).
    frontend_error: usize,
    known_frontend_gap: usize,
    resolved_known_frontend_gap: usize,
    dyn_error: usize,
}

#[derive(Serialize)]
struct Report {
    /// True iff nothing unexpected happened (no new gaps, no oracle mismatches).
    /// This is the primary signal for an automated loop.
    ok: bool,
    summary: Summary,
    cases: Vec<CaseReport>,
}

fn harness_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn main() {
    env_logger::init();
    let json_mode = std::env::args().any(|a| a == "--json");
    match run(json_mode) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harness error: {e:#}");
            std::process::exit(2);
        }
    }
}

fn run(json_mode: bool) -> Result<i32> {
    let root = harness_root();
    let model = root.join("markers.json");
    let static_markers = std::fs::read_to_string(root.join("shim/static_markers.c"))
        .context("reading shim/static_markers.c")?;
    let dfsan_shim = root.join("shim/dfsan_shim.c");
    let cases_dir = root.join("cases");

    let mut case_dirs: Vec<PathBuf> = std::fs::read_dir(&cases_dir)
        .with_context(|| format!("reading cases dir {}", cases_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("prog.c").is_file())
        .collect();
    case_dirs.sort();

    // CTADL's C frontend uses some panicking paths; silence our panic messages
    // so the table stays readable (we recover via catch_unwind below).
    std::panic::set_hook(Box::new(|_| {}));

    let mut rows = Vec::new();
    for case in &case_dirs {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let prog_path = case.join("prog.c");
        let prog = std::fs::read_to_string(&prog_path)
            .with_context(|| format!("reading {name}/prog.c"))?;
        let manifest: Manifest = serde_json::from_str(
            &std::fs::read_to_string(case.join("manifest.json"))
                .with_context(|| format!("reading {name}/manifest.json"))?,
        )
        .with_context(|| format!("parsing {name}/manifest.json"))?;

        let compiles = clang_compiles(&prog, &static_markers);

        // Static: concatenate the inert marker bodies so CTADL's model matches.
        let src = format!("{prog}\n{static_markers}\n");
        let static_flow = match catch_unwind(AssertUnwindSafe(|| analyze_c_flows(&src, &model))) {
            Err(_) => Err("panic in CTADL frontend".to_string()),
            Ok(Err(e)) => Err(short_err(&e.to_string())),
            Ok(Ok(flows)) => Ok(flows.iter().any(|f| f.label == manifest.label)),
        };

        // Dynamic: compile prog.c + instrumented shim under DFSan and run it.
        let dynamic_flow = run_dfsan(&prog_path, &dfsan_shim, &name);

        // Classify, applying the known_gap / known_frontend_gap allowlists.
        let status = match (&static_flow, &dynamic_flow) {
            // CTADL's frontend couldn't ingest the program.
            (Err(m), _) => match &manifest.known_frontend_gap {
                Some(id) => Status::KnownFrontendGap(id.clone()),
                None => Status::FrontendError(m.clone()),
            },
            (_, Err(m)) => Status::DynError(m.clone()),
            (Ok(s), Ok(d)) => {
                // If this case was allowlisted as a frontend gap but the frontend
                // ingested it this time, the parser improved — flag for re-triage.
                if let Some(id) = &manifest.known_frontend_gap {
                    Status::ResolvedKnownFrontendGap(id.clone())
                } else {
                    match (*d, *s) {
                        // dynamic observed a flow CTADL missed: a soundness gap.
                        (true, false) => match &manifest.known_gap {
                            Some(id) => Status::KnownGap(id.clone()),
                            None => Status::NewGap,
                        },
                        // CTADL reported a flow runtime never produced.
                        (false, true) => Status::PrecisionGap,
                        // agree: but if this was an allowlisted gap, it's now resolved.
                        _ => match &manifest.known_gap {
                            Some(id) => Status::ResolvedKnownGap(id.clone()),
                            None => Status::Ok,
                        },
                    }
                }
            }
        };

        // Cross-check: does observed runtime match the hand-authored oracle?
        let oracle_mismatch = matches!(&dynamic_flow, Ok(d) if *d != manifest.expect_flow);

        log::debug!("{name}: {} ({})", status.display(), manifest.description);
        rows.push(Row {
            name,
            compiles,
            oracle: manifest.expect_flow,
            static_flow: static_flow.ok(),
            dynamic_flow: dynamic_flow.ok(),
            status,
            known_gap: manifest.known_gap,
            known_frontend_gap: manifest.known_frontend_gap,
            oracle_mismatch,
        });
    }

    let summary = summarize(&rows);
    // The gate: anything a loop should treat as "needs attention now" — a new
    // soundness gap, an un-allowlisted frontend ingestion failure, or DFSan
    // disagreeing with the hand-authored oracle.
    let ok = summary.new_gap == 0 && summary.frontend_error == 0 && summary.oracle_mismatch == 0;

    let human = format!("{}\n{}", format_table(&rows), format_summary(&summary, ok));
    if json_mode {
        eprintln!("{human}");
        let report = Report {
            ok,
            summary,
            cases: rows.iter().map(case_report).collect(),
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{human}");
    }

    Ok(if ok { 0 } else { 1 })
}

fn summarize(rows: &[Row]) -> Summary {
    let mut s = Summary {
        total: rows.len(),
        ..Default::default()
    };
    for r in rows {
        match r.status {
            Status::Ok => s.ok += 1,
            Status::KnownGap(_) => s.known_gap += 1,
            Status::NewGap => s.new_gap += 1,
            Status::ResolvedKnownGap(_) => s.resolved_known_gap += 1,
            Status::PrecisionGap => s.precision_gap += 1,
            Status::FrontendError(_) => s.frontend_error += 1,
            Status::KnownFrontendGap(_) => s.known_frontend_gap += 1,
            Status::ResolvedKnownFrontendGap(_) => s.resolved_known_frontend_gap += 1,
            Status::DynError(_) => s.dyn_error += 1,
        }
        if r.oracle_mismatch {
            s.oracle_mismatch += 1;
        }
    }
    s
}

fn case_report(r: &Row) -> CaseReport {
    CaseReport {
        name: r.name.clone(),
        oracle: r.oracle,
        static_flow: r.static_flow,
        dynamic_flow: r.dynamic_flow,
        status: r.status.kind(),
        known_gap: r.known_gap.clone(),
        known_frontend_gap: r.known_frontend_gap.clone(),
        oracle_mismatch: r.oracle_mismatch,
        detail: r.status.detail(),
    }
}

fn format_table(rows: &[Row]) -> String {
    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let tri = |b: Option<bool>| match b {
        Some(true) => "flow",
        Some(false) => "none",
        None => "-",
    };
    let mut out = format!(
        "{:<nw$}  {:<8}  {:<6}  {:<6}  {:<7}  {}\n",
        "case", "compiles", "oracle", "static", "dynamic", "status",
        nw = name_w
    );
    out.push_str(&"-".repeat(name_w + 8 + 6 + 6 + 7 + 20));
    out.push('\n');
    for r in rows {
        let mismatch = if r.oracle_mismatch {
            "  (oracle mismatch!)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{:<nw$}  {:<8}  {:<6}  {:<6}  {:<7}  {}{}\n",
            r.name,
            if r.compiles { "yes" } else { "NO" },
            if r.oracle { "flow" } else { "none" },
            tri(r.static_flow),
            tri(r.dynamic_flow),
            r.status.display(),
            mismatch,
            nw = name_w
        ));
    }
    out
}

fn format_summary(s: &Summary, ok: bool) -> String {
    let mut lines = vec![
        format!(
            "{} cases: {} ok, {} known-gap, {} NEW-GAP, {} resolved-known-gap, {} precision-gap, \
             {} oracle-mismatch",
            s.total, s.ok, s.known_gap, s.new_gap, s.resolved_known_gap, s.precision_gap,
            s.oracle_mismatch,
        ),
        format!(
            "          frontend: {} known-frontend-gap, {} FRONTEND-ERROR, \
             {} resolved-known-frontend-gap, {} dyn-error",
            s.known_frontend_gap, s.frontend_error, s.resolved_known_frontend_gap, s.dyn_error,
        ),
    ];
    if s.resolved_known_gap > 0 {
        lines.push(
            "resolved-known-gap: a known soundness gap stopped failing — if intended, drop its \
             `known_gap` from the manifest (and the F-entry in KNOWN_FINDINGS.md)."
                .to_string(),
        );
    }
    if s.resolved_known_frontend_gap > 0 {
        lines.push(
            "resolved-known-frontend-gap: the frontend now ingests a previously-failing case — \
             drop its `known_frontend_gap` and re-triage (it may reveal a soundness result)."
                .to_string(),
        );
    }
    lines.push(format!(
        "result: {} (exit {})",
        if ok {
            "OK — nothing unexpected"
        } else {
            "ATTENTION — new soundness gap, frontend error, or oracle mismatch"
        },
        if ok { 0 } else { 1 },
    ));
    lines.join("\n")
}

/// Compile `prog.c` + inert static markers with plain clang (no DFSan) as a
/// well-formedness check. Returns true on success.
fn clang_compiles(prog: &str, static_markers: &str) -> bool {
    let dir = std::env::temp_dir();
    let stamp = std::process::id();
    let prog_path = dir.join(format!("ctadl_dyn_{stamp}_prog.c"));
    let mk_path = dir.join(format!("ctadl_dyn_{stamp}_markers.c"));
    let out_path = dir.join(format!("ctadl_dyn_{stamp}.out"));
    if std::fs::write(&prog_path, prog).is_err() || std::fs::write(&mk_path, static_markers).is_err()
    {
        return false;
    }
    let ok = Command::new("clang")
        .args(["-O0", "-w", "-o"])
        .arg(&out_path)
        .arg(&prog_path)
        .arg(&mk_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&prog_path);
    let _ = std::fs::remove_file(&mk_path);
    let _ = std::fs::remove_file(&out_path);
    ok
}

/// Compile `prog.c` + the instrumented DFSan shim and run it, returning whether
/// the sink observed the `Test` label at runtime. ASLR is disabled via
/// `setarch -R` because DFSan's shadow-memory layout assumes the binary loads in
/// a fixed low address range (otherwise it aborts with "out of application
/// range"). Falls back to a direct run if `setarch` is unavailable.
fn run_dfsan(prog_path: &Path, shim_path: &Path, case_name: &str) -> Result<bool, String> {
    let dir = std::env::temp_dir();
    let out = dir.join(format!("ctadl_dfsan_{}_{}", std::process::id(), case_name));

    let compiled = Command::new("clang")
        .args(["-fsanitize=dataflow", "-O0", "-w", "-o"])
        .arg(&out)
        .arg(prog_path)
        .arg(shim_path)
        .output();
    match compiled {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "dfsan compile failed: {}",
                short_err(&String::from_utf8_lossy(&o.stderr))
            ));
        }
        Err(e) => return Err(format!("clang not runnable: {e}")),
    }

    let run = Command::new("setarch")
        .arg("-R")
        .arg(&out)
        .output()
        .or_else(|_| Command::new(&out).output());
    let _ = std::fs::remove_file(&out);

    let output = run.map_err(|e| format!("run failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut saw_obs = false;
    let mut tainted = false;
    for line in stdout.lines() {
        if line.starts_with("CTADL_DYN_OBSERVE") {
            saw_obs = true;
            if line.contains("test=1") {
                tainted = true;
            }
        }
    }
    if !saw_obs {
        return Err(format!(
            "no sink observation (exit {:?})",
            output.status.code()
        ));
    }
    Ok(tainted)
}

fn short_err(s: &str) -> String {
    let first = s.lines().next().unwrap_or(s).trim();
    if first.len() > 60 {
        format!("{}…", &first[..60])
    } else {
        first.to_string()
    }
}
