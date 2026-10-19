//! `cil:flow`: the acceptance gate of cil-reader's flow layer. Three parts:
//!
//! - **property**: the invariants `cil_reader::flow::check` states, over every
//!   method body in the corpus (Cecil's assemblies and the compiled fixtures).
//!   Layer 1 via `layer1_violations` on every instruction, plus coverage of
//!   every instruction byte; Layer 2 via `layer2_violations` on
//!   `MethodFlow::build` of every body.
//! - **fixtures**: targeted tests on `flow.il` (in
//!   `readers/cil-reader/fixtures`), for what the Cecil corpus barely
//!   exercises; see `flow/fixtures.rs`.
//! - **goldens**: `cil-dump --flow` of the flow fixtures against
//!   `xtask/cil-goldens/flow`, reviewed when blessed with
//!   `cargo xtask cil bless-flow`.
//!
//! A summary of the property sweep (methods, instructions, Opaque effects)
//! is written to `<work>/cil-flow/summary.txt`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result};
use cil_reader::cil::Ins;
use cil_reader::flow::check::{layer1_violations, layer2_violations};
use cil_reader::flow::{FlowCx, InsFlow, MethodFlow};
use cil_reader::model::{MethodBody, MethodDef};
use cil_reader::raw::Table;
use cil_reader::Module;

use super::cecil_tests::{install_panic_hook, run_one};
use super::parity::Input;
use super::{parallel_map, to_outcome};
use crate::regression::Outcome;

mod client;
mod fixtures;

/// The fixtures whose `cil-dump --flow` has a golden.
pub const FLOW_FIXTURES: &[&str] = &["flow.il.dll"];

/// Where the flow goldens live under the goldens directory.
pub fn goldens_dir(goldens: &Path) -> PathBuf {
    goldens.join("flow")
}

fn golden_path(goldens: &Path, fixture: &str) -> PathBuf {
    goldens_dir(goldens).join(format!("{fixture}.flow"))
}

/// The compiled fixtures, for the fixture tests.
static FIXTURES: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Contents of a compiled fixture, for the fixture tests.
pub fn read_fixture(name: &str) -> Vec<u8> {
    let dir = FIXTURES
        .read()
        .unwrap()
        .clone()
        .expect("fixtures set by `run`");
    let path = dir.join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Run the three parts as one check.
pub fn run(inputs: &[Input], fixtures: &Path, goldens: &Path, work: &Path) -> Result<Outcome> {
    *FIXTURES.write().unwrap() = Some(fixtures.to_path_buf());
    let mut failures = Vec::new();

    // property
    let sweeps = parallel_map(inputs, |input| sweep_input(&input.path));
    let mut totals = Stats::default();
    let mut summary = String::new();
    for (input, sweep) in inputs.iter().zip(&sweeps) {
        match sweep {
            Ok((stats, violations)) => {
                totals.add(stats);
                let _ = writeln!(summary, "{} {}", input.id(), stats.describe());
                failures.extend(
                    violations
                        .iter()
                        .map(|v| format!("property {}: {v}", input.id())),
                );
            }
            Err(e) => failures.push(format!("property {}: {e}", input.id())),
        }
    }
    let _ = writeln!(summary, "TOTAL {}", totals.describe());
    let dir = work.join("cil-flow");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("summary.txt"), &summary)?;
    if totals.methods == 0 {
        failures.push("property: no method bodies swept".to_string());
    }

    // fixtures
    install_panic_hook();
    let results = parallel_map(fixtures::TESTS, |(_, test)| run_one(*test));
    for ((name, _), result) in fixtures::TESTS.iter().zip(results) {
        if let Err(e) = result {
            failures.push(format!("fixtures::{name}: {e}"));
        }
    }

    // goldens
    for fixture in FLOW_FIXTURES {
        let path = golden_path(goldens, fixture);
        let golden = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read {} (bless with `cargo xtask cil bless-flow`)",
                path.display()
            )
        })?;
        match flow_dump(&fixtures.join(fixture)) {
            Ok(dump) if dump == golden => {}
            Ok(dump) => {
                let actual = dir.join(format!("{fixture}.flow"));
                std::fs::write(&actual, &dump)?;
                let (line, lines) = super::parity::first_divergence(&golden, &dump);
                let mut msg = format!("golden {fixture}: differs at line {line}");
                if let Some((g, a)) = lines {
                    let _ = write!(msg, "\n  golden: {g}\n  rust: {a}");
                }
                let _ = write!(msg, "\n  dump in {}", actual.display());
                failures.push(msg);
            }
            Err(e) => failures.push(format!("golden {fixture}: {e}")),
        }
    }

    Ok(to_outcome(failures, "flow check(s)"))
}

/// `cil-dump --flow` of the assembly at `path`.
pub fn flow_dump(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let module = Module::read(&data).map_err(|e| e.to_string())?;
    cil_dump::flow::dump_flow(&module).map_err(|e| e.to_string())
}

/// `cargo xtask cil bless-flow`: rewrite the flow goldens from the compiled
/// fixtures. Review the diff before committing it.
pub fn bless(goldens: &Path, fixtures: &Path) -> Result<bool> {
    std::fs::create_dir_all(goldens_dir(goldens))?;
    for fixture in FLOW_FIXTURES {
        let dump = flow_dump(&fixtures.join(fixture)).map_err(anyhow::Error::msg)?;
        let path = golden_path(goldens, fixture);
        std::fs::write(&path, dump)?;
        println!("bless-flow: wrote {}", path.display());
    }
    Ok(true)
}

/// What the property sweep saw in one input.
#[derive(Default, Clone)]
struct Stats {
    methods: usize,
    instructions: usize,
    opaque: usize,
    /// Bodies whose signature or code did not decode: the reader's errors,
    /// which the flow layer reports rather than causes.
    decode_errors: usize,
    /// Opaque effects by reason, for the summary.
    opaque_reasons: BTreeMap<String, usize>,
    /// Layer 2 warnings by kind, for the summary.
    warnings: BTreeMap<String, usize>,
    blocks: usize,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.methods += other.methods;
        self.instructions += other.instructions;
        self.opaque += other.opaque;
        self.decode_errors += other.decode_errors;
        for (k, v) in &other.opaque_reasons {
            *self.opaque_reasons.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.warnings {
            *self.warnings.entry(k.clone()).or_default() += v;
        }
        self.blocks += other.blocks;
    }

    fn describe(&self) -> String {
        format!(
            "methods={} instructions={} blocks={} opaque={} decode-errors={} {:?} warnings={:?}",
            self.methods,
            self.instructions,
            self.blocks,
            self.opaque,
            self.decode_errors,
            self.opaque_reasons,
            self.warnings
        )
    }
}

/// Violations listed per input before the rest are counted.
const MAX_PER_INPUT: usize = 5;

/// The property sweep over one assembly. A panic is an error.
fn sweep_input(path: &Path) -> Result<(Stats, Vec<String>), String> {
    let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    std::panic::catch_unwind(|| {
        let module = Module::read(&data).map_err(|e| e.to_string())?;
        let mut stats = Stats::default();
        let mut violations = Vec::new();
        let mut more = 0usize;
        for rid in 1..=module.metadata().row_count(Table::Method) {
            let Some(method) = module.method_def(rid) else {
                continue;
            };
            if !method.has_body().unwrap_or(false) || method.rva().unwrap_or(0) == 0 {
                continue;
            }
            let Ok(body) = method.body() else {
                stats.decode_errors += 1;
                continue;
            };
            stats.methods += 1;
            let mut found = check_body(&method, &body, &mut stats);
            found.extend(check_method_flow(&body, &mut stats));
            for v in found {
                if violations.len() < MAX_PER_INPUT {
                    violations.push(format!("{:08x}: {v}", method.token().raw()));
                } else {
                    more += 1;
                }
            }
        }
        if more > 0 {
            violations.push(format!("... and {more} more"));
        }
        Ok((stats, violations))
    })
    .map_err(|_| "panicked".to_string())?
}

/// The Layer 2 invariants over one body.
fn check_method_flow(body: &MethodBody<'_>, stats: &mut Stats) -> Vec<String> {
    let mf = MethodFlow::build(body);
    stats.blocks += mf.blocks.len();
    for w in &mf.warnings {
        let kind = format!("{w:?}");
        let kind = kind.split([' ', '(', '{']).next().unwrap_or("").to_string();
        *stats.warnings.entry(kind).or_default() += 1;
    }
    layer2_violations(&mf)
}

/// The Layer 1 invariants over one body, plus coverage: the records tile the
/// decoded code, one per instruction that is not a prefix.
fn check_body(method: &MethodDef<'_>, body: &MethodBody<'_>, stats: &mut Stats) -> Vec<String> {
    let mut v = Vec::new();
    let Ok(sig) = method.signature() else {
        stats.decode_errors += 1;
        return v;
    };
    if body.decode_error.is_some() {
        stats.decode_errors += 1;
    }
    let flows: Vec<InsFlow> = body.flows().filter_map(|f| f.ok()).collect();
    let cx = FlowCx::new(body, &sig);
    let by_end: BTreeMap<u32, &Ins<'_>> = body
        .instructions
        .iter()
        .map(|i| (i.end_offset(), i))
        .collect();

    let mut expected_offset = 0;
    for flow in &flows {
        stats.instructions += 1;
        if flow.offset != expected_offset {
            v.push(format!(
                "IL_{:04x}: record starts there, not at IL_{expected_offset:04x} where the last ended",
                flow.offset
            ));
        }
        expected_offset = flow.end_offset();
        for e in &flow.effects {
            if let cil_reader::flow::Effect::Opaque { reason, .. } = e {
                stats.opaque += 1;
                let key = match reason {
                    cil_reader::flow::OpaqueReason::UnresolvedToken(_) => "unresolved",
                    cil_reader::flow::OpaqueReason::UndefinedOpcode => "undefined-opcode",
                };
                *stats.opaque_reasons.entry(key.to_string()).or_default() += 1;
            }
        }
        let Some(ins) = by_end.get(&flow.end_offset()) else {
            v.push(format!(
                "IL_{:04x}: no instruction ends where it does",
                flow.offset
            ));
            continue;
        };
        for violation in layer1_violations(ins, flow, &cx) {
            v.push(format!(
                "IL_{:04x} {}: {violation}",
                flow.offset, ins.opcode
            ));
        }
    }
    let decoded_end = body.instructions.last().map_or(0, |i| i.end_offset());
    if expected_offset != decoded_end {
        v.push(format!(
            "records end at IL_{expected_offset:04x}, the code at IL_{decoded_end:04x}"
        ));
    }
    let non_prefix = body
        .instructions
        .iter()
        .filter(|i| !cil_reader::flow::is_prefix(i.opcode.code))
        .count();
    let trailing = flows
        .last()
        .is_some_and(|f| cil_reader::flow::is_prefix(f.opcode.code));
    if flows.len() != non_prefix + trailing as usize {
        v.push(format!(
            "{} records for {non_prefix} instructions that are not prefixes",
            flows.len()
        ));
    }
    v
}
