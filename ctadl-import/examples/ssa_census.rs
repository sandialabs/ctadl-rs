//! Per-function SSA census of an import: statements before SSA, and after the `ctadl index`
//! pipeline, the phis it placed, their operands, and how many phis are live (reached from a
//! non-phi use through phi operands) -- i.e. what pruned SSA would have kept.
//!
//!   cargo run --release -p ctadl-import --example ssa_census -- <import-dir> [top-n]
use std::collections::{HashMap, HashSet};

use ctadl_import::load_import;
use ctadl_import::SourceInfoMode;
use ctadl_ir::mir::StatementKind;
use ctadl_ir::{FunctionData, VariableRef, ssa};

#[derive(Default, Clone, Copy)]
struct Row {
    blocks: usize,
    stmts_before: usize,
    stmts_after: usize,
    phis: usize,
    phi_operands: usize,
    live_phis: usize,
    live_phi_operands: usize,
}

fn census(f: &FunctionData) -> (usize, usize, usize, usize, usize) {
    let mut stmts = 0;
    let mut phi_ops: HashMap<&VariableRef, Vec<&VariableRef>> = HashMap::new();
    let mut roots: Vec<&VariableRef> = Vec::new();
    for (_, data) in f.blocks.iter_enumerated() {
        for s in data.iter() {
            stmts += 1;
            if let StatementKind::Phi { dest, operands } = &s.kind {
                phi_ops.insert(dest, operands.iter().map(|(_, v)| v).collect());
            } else {
                roots.extend(s.iter_src_var());
            }
        }
        if let Some(t) = &data.terminator {
            roots.extend(t.iter_src_var());
        }
    }
    let phis = phi_ops.len();
    let phi_operands: usize = phi_ops.values().map(|v| v.len()).sum();
    let mut live: HashSet<&VariableRef> = HashSet::new();
    while let Some(v) = roots.pop() {
        if let Some(ops) = phi_ops.get(v) {
            if live.insert(v) {
                roots.extend(ops.iter().copied());
            }
        }
    }
    let live_ops: usize = live.iter().map(|v| phi_ops[v].len()).sum();
    (stmts, phis, phi_operands, live.len(), live_ops)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let top: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
    let import = ctadl_import::store::resolve_import(dir).expect("import");
    let mut info = load_import(&import, SourceInfoMode::Skip).expect("load");
    let before: Vec<(usize, usize)> = info
        .program
        .functions
        .iter()
        .map(|f| (f.blocks.iter_enumerated().count(), f.blocks.iter_enumerated().map(|(_, d)| d.iter().count()).sum()))
        .collect();
    ssa::run_pipeline(&mut info.program, ssa::Pipeline::index_default());
    let mut rows: Vec<(String, Row)> = Vec::new();
    for (i, f) in info.program.functions.iter().enumerate() {
        let (stmts_after, phis, phi_operands, live_phis, live_phi_operands) = census(f);
        rows.push((
            f.name.clone(),
            Row {
                blocks: before[i].0,
                stmts_before: before[i].1,
                stmts_after,
                phis,
                phi_operands,
                live_phis,
                live_phi_operands,
            },
        ));
    }
    let mut tot = Row::default();
    for (_, r) in &rows {
        tot.blocks += r.blocks;
        tot.stmts_before += r.stmts_before;
        tot.stmts_after += r.stmts_after;
        tot.phis += r.phis;
        tot.phi_operands += r.phi_operands;
        tot.live_phis += r.live_phis;
        tot.live_phi_operands += r.live_phi_operands;
    }
    rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.phi_operands));
    println!(
        "{:<28} {:>7} {:>10} {:>10} {:>9} {:>10} {:>9} {:>10}",
        "function", "blocks", "stmts_pre", "stmts_ssa", "phis", "phi_ops", "live_phi", "live_ops"
    );
    let p = |n: &str, r: &Row| {
        println!(
            "{:<28} {:>7} {:>10} {:>10} {:>9} {:>10} {:>9} {:>10}",
            n, r.blocks, r.stmts_before, r.stmts_after, r.phis, r.phi_operands, r.live_phis, r.live_phi_operands
        )
    };
    for (n, r) in rows.iter().take(top) {
        p(n, r);
    }
    p("TOTAL", &tot);
}
