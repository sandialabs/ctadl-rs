//! Case runner for the DFSan-dynamic vs. CTADL-static taint comparison harness.
//!
//! For each case under `cases/`, this:
//!   1. runs CTADL's static taint analysis ([`analyze_c_flows`]) on `prog.c` +
//!      the inert static markers,
//!   2. compiles `prog.c` + the **instrumented** DFSan shim with
//!      `clang -fsanitize=dataflow`, runs it (ASLR disabled via `setarch -R`),
//!      and reads the observed runtime taint,
//!   3. classifies static vs. dynamic, with the hand-authored manifest oracle as
//!      a cross-check.
//!
//! DFSan observation is the ground truth: `dynamic=flow & static=none` is a
//! soundness gap (CTADL missed a flow that genuinely happens at runtime).
//!
//! Run with: `cargo run -p ctadl-dynamic` (add `RUST_LOG=debug` for detail).
//! Requires `clang` with the dataflow sanitizer runtime and `setarch`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use ctadl_ascent::taint_compare::analyze_c_flows;
use serde::Deserialize;

/// Per-case ground truth, authored by hand alongside `prog.c`.
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    description: String,
    #[serde(default = "default_label")]
    label: String,
    /// Whether taint truly flows from source to sink in this program.
    expect_flow: bool,
}

fn default_label() -> String {
    "Test".to_string()
}

#[derive(Debug)]
enum Verdict {
    /// Static and dynamic agree.
    Ok,
    /// Dynamic observed a flow that CTADL missed — a soundness gap.
    SoundnessGap,
    /// CTADL reported a flow that did not occur at runtime — imprecision.
    PrecisionGap,
    /// CTADL failed to parse/index the program (frontend gap, not a taint bug).
    FrontendError(String),
    /// DFSan could not compile/run the case (no dynamic ground truth).
    DynError(String),
}

impl Verdict {
    fn tag(&self) -> &'static str {
        match self {
            Verdict::Ok => "OK",
            Verdict::SoundnessGap => "SOUNDNESS-GAP",
            Verdict::PrecisionGap => "precision-gap",
            Verdict::FrontendError(_) => "frontend-error",
            Verdict::DynError(_) => "dyn-error",
        }
    }
    fn note(&self) -> Option<&str> {
        match self {
            Verdict::FrontendError(m) | Verdict::DynError(m) => Some(m),
            _ => None,
        }
    }
}

fn harness_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Row {
    name: String,
    compiles: bool,
    oracle: bool,
    static_flow: Option<bool>,
    dynamic_flow: Option<bool>,
    verdict: Verdict,
    oracle_mismatch: bool,
}

fn main() -> Result<()> {
    env_logger::init();
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
    let mut soundness_gaps = 0usize;
    let mut oracle_mismatches = 0usize;
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

        // Verdict: dynamic is ground truth for the soundness/precision call.
        let verdict = match (&static_flow, &dynamic_flow) {
            (Err(m), _) => Verdict::FrontendError(m.clone()),
            (_, Err(m)) => Verdict::DynError(m.clone()),
            (Ok(s), Ok(d)) => match (*d, *s) {
                (true, false) => {
                    soundness_gaps += 1;
                    Verdict::SoundnessGap
                }
                (false, true) => Verdict::PrecisionGap,
                _ => Verdict::Ok,
            },
        };

        // Cross-check: does observed runtime match the hand-authored oracle?
        let oracle_mismatch = matches!(&dynamic_flow, Ok(d) if *d != manifest.expect_flow);
        if oracle_mismatch {
            oracle_mismatches += 1;
        }

        log::debug!("{name}: {} ({})", verdict.tag(), manifest.description);
        rows.push(Row {
            name,
            compiles,
            oracle: manifest.expect_flow,
            static_flow: static_flow.ok(),
            dynamic_flow: dynamic_flow.ok(),
            verdict,
            oracle_mismatch,
        });
    }

    print_table(&rows);
    println!(
        "\n{} cases — {} soundness gap(s), {} oracle mismatch(es). \
         Ground truth = DFSan dynamic observation.",
        rows.len(),
        soundness_gaps,
        oracle_mismatches
    );
    if soundness_gaps > 0 {
        println!(
            "Note: soundness gaps in this corpus may be intentionally-preserved known \
             findings — see ctadl-dynamic/KNOWN_FINDINGS.md."
        );
    }
    Ok(())
}

fn print_table(rows: &[Row]) {
    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let tri = |b: Option<bool>| match b {
        Some(true) => "flow",
        Some(false) => "none",
        None => "-",
    };
    println!(
        "{:<nw$}  {:<8}  {:<6}  {:<6}  {:<7}  {}",
        "case", "compiles", "oracle", "static", "dynamic", "verdict",
        nw = name_w
    );
    println!("{}", "-".repeat(name_w + 8 + 6 + 6 + 7 + 18));
    for r in rows {
        let note = match (r.verdict.note(), r.oracle_mismatch) {
            (Some(m), _) => format!("  ({m})"),
            (None, true) => "  (oracle mismatch!)".to_string(),
            (None, false) => String::new(),
        };
        println!(
            "{:<nw$}  {:<8}  {:<6}  {:<6}  {:<7}  {}{}",
            r.name,
            if r.compiles { "yes" } else { "NO" },
            if r.oracle { "flow" } else { "none" },
            tri(r.static_flow),
            tri(r.dynamic_flow),
            r.verdict.tag(),
            note,
            nw = name_w
        );
    }
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
