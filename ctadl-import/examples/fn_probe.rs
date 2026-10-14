//! One function after the default index pipeline: loads and stores by base (frame kept in
//! memory, parameter, other), and calls by callee.
//!
//!   cargo run --release -p ctadl-import --example fn_probe -- <import-dir> <function>
use std::collections::BTreeMap;

use ctadl_import::SourceInfoMode;
use ctadl_import::load_import;
use ctadl_ir::mir::{CallEdges, CallStyle, StatementKind, Variable};
use ctadl_ir::ssa;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let import = ctadl_import::store::resolve_import(&args[1]).expect("import");
    let mut info = load_import(&import, SourceInfoMode::Skip).expect("load");
    ssa::run_pipeline(&mut info.program, ssa::Pipeline::index_default());
    let f = info.program.functions.iter().find(|f| f.name == args[2]).expect("function");
    let st = f.locals.iter_enumerated().find(|(_, d)| d.name == ssa::STACK_TOP).map(|(i, _)| i);
    let kind = |v: &ctadl_ir::VariableRef, _: &ctadl_ir::FunctionData| match v.variable.as_ref() {
        Variable::Local(l) if Some(*l) == st => "frame".to_string(),
        Variable::Local(_) => "local".to_string(),
        Variable::Param(p) => format!("param{p:?}"),
        Variable::GlobalHeap => "globals".to_string(),
    };
    let mut mem: BTreeMap<String, usize> = BTreeMap::new();
    let mut calls: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut stmts = 0;
    // kept frame slot offset -> (stores, loads)
    let mut slots: BTreeMap<i64, (usize, usize)> = BTreeMap::new();
    let off = |ap: &ctadl_ir::mir::AccessPath| -> i64 {
        ap.accesses.offsets.iter().map(|a| a.offset().0).sum()
    };
    for (_, data) in f.blocks.iter_enumerated() {
        for s in data.iter() {
            stmts += 1;
            match &s.kind {
                StatementKind::Load { source, .. } => {
                    if kind(&source.base, f) == "frame" {
                        slots.entry(off(source)).or_default().1 += 1;
                    }
                    *mem.entry(format!("load  {}", kind(&source.base, f))).or_default() += 1
                }
                StatementKind::Store { dest, .. } => {
                    if kind(&dest.base, f) == "frame" {
                        slots.entry(off(dest)).or_default().0 += 1;
                    }
                    *mem.entry(format!("store {}", kind(&dest.base, f))).or_default() += 1
                }
                StatementKind::CallAssign { style, args, .. } => {
                    let name = match style {
                        CallStyle::DirectCall { call_edges: CallEdges::Explicit(e) } => e.join("|"),
                        CallStyle::FuncPtrCall { .. } => "<indirect>".to_string(),
                        _ => "<other>".to_string(),
                    };
                    let e = calls.entry(name).or_default();
                    e.0 += 1;
                    e.1 += args.len();
                }
                _ => {}
            }
        }
    }
    println!("{}: {} blocks, {} statements, {} locals", f.name, f.blocks.len(), stmts, f.locals.iter_enumerated().count());
    println!("memory accesses by base:");
    for (k, n) in &mem {
        println!("  {n:>8}  {k}");
    }
    let mut sv: Vec<_> = slots.iter().collect();
    sv.sort_by_key(|(_, (st, ld))| std::cmp::Reverse(st * ld));
    println!("kept frame slots: {} (by stores x loads):", sv.len());
    for (k, (st, ld)) in sv.iter().take(15) {
        println!("  {k:>8}: {st:>6} stores {ld:>6} loads");
    }
    let mut v: Vec<_> = calls.into_iter().collect();
    v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    println!("calls ({} distinct callees, {} sites):", v.len(), v.iter().map(|(_, (n, _))| n).sum::<usize>());
    for (name, (n, a)) in v.iter().take(30) {
        println!("  {n:>6} sites {a:>6} args  {name}");
    }
}
