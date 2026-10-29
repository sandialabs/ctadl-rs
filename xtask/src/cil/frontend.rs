//! `cil:frontend`: the CIL frontend (`frontends/ctadl-cil`) checked against what it does not
//! control. Two parts:
//!
//! - **sweep**: every corpus assembly (Cecil's and the compiled fixtures) imported on its own,
//!   which runs `Program::verify`, and then put through the SSA pipeline `ctadl index` runs
//!   (`Pipeline::index_default`). Neither may panic, and an assembly cil-reader reads must
//!   import. `ssa::transform` asserts `FunctionData::verify` itself, so this also checks every
//!   function the frontend makes is well formed.
//! - **goldens**: the IR of the frontend fixtures against `xtask/cil-goldens/mir`, reviewed
//!   when blessed with `cargo xtask cil bless-mir`.
//!
//! Per-assembly counts (functions, stubs, unresolved address writes, the flow layer and the
//! frontend disagreeing on an address) go to `<work>/cil-frontend/summary.txt`. A
//! disagreement fails the check: the frontend's address analysis is meant to trace
//! everything the layer's does.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctadl_ir::ssa::{self, Pipeline};

use super::cecil_tests::install_panic_hook;
use super::parity::Input;
use super::{parallel_map, to_outcome};
use crate::regression::Outcome;

/// The fixtures whose IR has a golden.
pub const MIR_FIXTURES: &[&str] = &["exec.cs.dll", "exec.il.dll", "flow.il.dll"];

/// Where the IR goldens live under the goldens directory.
pub fn goldens_dir(goldens: &Path) -> PathBuf {
    goldens.join("mir")
}

fn golden_path(goldens: &Path, fixture: &str) -> PathBuf {
    goldens_dir(goldens).join(format!("{fixture}.mir"))
}

/// What one assembly's import came to.
#[derive(Default)]
struct Swept {
    functions: usize,
    stats: Option<ctadl_cil::Stats>,
}

pub fn run(inputs: &[Input], fixtures: &Path, goldens: &Path, work: &Path) -> Result<Outcome> {
    install_panic_hook();
    let dir = work.join("cil-frontend");
    std::fs::create_dir_all(&dir)?;
    let opts = ctadl_cil::ImportOptions::default();
    // Largest first. Mono.Android.dll takes longer to import than the rest of the corpus put
    // together, so the sweep takes as long as it does; started first, it overlaps the others.
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by_key(|&i| {
        std::cmp::Reverse(std::fs::metadata(&inputs[i].path).map_or(0, |m| m.len()))
    });
    let mut swept = parallel_map(&order, |&i| (i, sweep(&inputs[i].path, &opts)));
    swept.sort_by_key(|(i, _)| *i);
    let sweeps = swept.into_iter().map(|(_, result)| result);
    let mut failures = Vec::new();
    let mut summary = String::new();
    let mut total = Totals::default();
    for (input, result) in inputs.iter().zip(sweeps) {
        match result {
            Ok(swept) => {
                let line = total.add(&swept);
                let _ = writeln!(summary, "{} {line}", input.id());
                if let Some(s) = &swept.stats {
                    if s.address_disagreements > 0 {
                        failures.push(format!(
                            "sweep {}: {} input(s) the flow layer traces and the frontend does \
                             not",
                            input.id(),
                            s.address_disagreements
                        ));
                    }
                }
            }
            Err(e) => {
                let _ = writeln!(summary, "{} ERROR {e}", input.id());
                failures.push(format!("sweep {}: {e}", input.id()));
            }
        }
    }
    let _ = writeln!(summary, "TOTAL {}", total.describe());
    std::fs::write(dir.join("summary.txt"), &summary)?;
    if total.assemblies == 0 {
        failures.push("sweep: no assembly imported".to_string());
    }

    for fixture in MIR_FIXTURES {
        let path = golden_path(goldens, fixture);
        let golden = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read {} (bless with `cargo xtask cil bless-mir`)",
                path.display()
            )
        })?;
        match mir_dump(&fixtures.join(fixture)) {
            Ok(dump) if dump == golden => {}
            Ok(dump) => {
                let actual = dir.join(format!("{fixture}.mir"));
                std::fs::write(&actual, &dump)?;
                let (line, lines) = super::parity::first_divergence(&golden, &dump);
                let mut msg = format!("golden {fixture}: differs at line {line}");
                if let Some((g, a)) = lines {
                    let _ = write!(msg, "\n  golden: {g}\n  actual: {a}");
                }
                let _ = write!(msg, "\n  actual IR in {}", actual.display());
                failures.push(msg);
            }
            Err(e) => failures.push(format!("golden {fixture}: {e}")),
        }
    }
    Ok(to_outcome(failures, "frontend check(s)"))
}

/// Imports one assembly and runs the index's SSA pipeline over it.
fn sweep(path: &Path, opts: &ctadl_cil::ImportOptions) -> Result<Swept, String> {
    // An image cil-reader does not read is `cil:read`'s to report, not this check's.
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if cil_reader::Module::read(&data).is_err() {
        return Ok(Swept::default());
    }
    let imported = std::panic::catch_unwind(|| {
        ctadl_cil::import_files_with_stats(&[path.to_path_buf()], opts)
    })
    .map_err(|p| format!("the frontend panicked: {}", panic_message(&p)))?;
    let (mut info, stats) = imported.map_err(|e| format!("import failed: {e:#}"))?;
    let functions = info.program.functions.len();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ssa::run_pipeline(&mut info.program, Pipeline::index_default())
    }))
    .map_err(|p| format!("the SSA pipeline panicked: {}", panic_message(&p)))?;
    Ok(Swept {
        functions,
        stats: Some(stats),
    })
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "(no message)".to_string())
}

/// The IR of an assembly as the goldens hold it: the method table, then every function.
pub fn mir_dump(path: &Path) -> Result<String> {
    let opts = ctadl_cil::ImportOptions::default();
    let info = ctadl_cil::import_files(&[path.to_path_buf()], &opts)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .with_context(|| format!("importing {}", path.display()))?;
    let mut out = String::new();
    let _ = writeln!(out, "{}", info.vmt);
    for func in info.program.functions.iter() {
        let _ = writeln!(out, "{}", ctadl_ir::mir::WithLocalNames(func));
    }
    Ok(out)
}

/// `cargo xtask cil bless-mir`: regenerate the IR goldens from the fixtures.
pub fn bless(goldens: &Path, fixtures: &Path) -> Result<bool> {
    let dir = goldens_dir(goldens);
    std::fs::create_dir_all(&dir)?;
    for fixture in MIR_FIXTURES {
        let dump = mir_dump(&fixtures.join(fixture))?;
        std::fs::write(golden_path(goldens, fixture), dump)?;
    }
    println!(
        "bless-mir: wrote {} golden(s) to {}",
        MIR_FIXTURES.len(),
        dir.display()
    );
    Ok(true)
}

#[derive(Default)]
struct Totals {
    assemblies: usize,
    functions: usize,
    bodies: usize,
    stubs: usize,
    bridges: usize,
    shared_key_bridges: usize,
    opaque: usize,
    unresolved_writes: usize,
    missing_metadata: usize,
}

impl Totals {
    fn add(&mut self, s: &Swept) -> String {
        let Some(stats) = &s.stats else {
            return "skipped (cil-reader does not read it)".to_string();
        };
        self.assemblies += 1;
        self.functions += s.functions;
        self.bodies += stats.bodies;
        self.stubs += stats.stubs;
        self.bridges += stats.bridges;
        self.shared_key_bridges += stats.shared_key_bridges;
        self.opaque += stats.opaque_effects;
        self.unresolved_writes += stats.unresolved_address_writes;
        self.missing_metadata += stats.missing_metadata.len();
        format!(
            "functions={} bodies={} stubs={} bridges={} shared-key-bridges={} opaque={} \
             unresolved-address-writes={} missing-metadata={}",
            s.functions,
            stats.bodies,
            stats.stubs,
            stats.bridges,
            stats.shared_key_bridges,
            stats.opaque_effects,
            stats.unresolved_address_writes,
            stats.missing_metadata.len()
        )
    }

    fn describe(&self) -> String {
        format!(
            "assemblies={} functions={} bodies={} stubs={} bridges={} shared-key-bridges={} \
             opaque={} unresolved-address-writes={} missing-metadata={}",
            self.assemblies,
            self.functions,
            self.bodies,
            self.stubs,
            self.bridges,
            self.shared_key_bridges,
            self.opaque,
            self.unresolved_writes,
            self.missing_metadata
        )
    }
}
