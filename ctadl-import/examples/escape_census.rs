//! Where escaped stack addresses go when they escape into a call: a function defined in the
//! import (internal), one that is not (external), or a function pointer (indirect). Decides
//! whether a callee access-extent summary could bound how far an escaped address reaches.
//!
//!   cargo run --release -p ctadl-import --example escape_census -- <import-dir> [top-n]
use std::collections::{BTreeMap, HashSet};

use ctadl_import::SourceInfoMode;
use ctadl_import::load_import;
use ctadl_ir::mir::{CallEdges, CallStyle, Exp, StatementKind, Variable};
use ctadl_ir::ssa;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let top: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(25);
    let import = ctadl_import::store::resolve_import(dir).expect("import");
    let mut info = load_import(&import, SourceInfoMode::Skip).expect("load");
    ssa::run_pipeline(
        &mut info.program,
        ssa::Pipeline { dead_temps: true, coalesce: true, ..ssa::Pipeline::none() },
    );
    let defined: HashSet<&str> = info.program.functions.iter().map(|f| f.name.as_str()).collect();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut externals: BTreeMap<String, usize> = BTreeMap::new();
    let mut internals: BTreeMap<String, usize> = BTreeMap::new();
    for f in info.program.functions.iter() {
        let Some((st, _)) = f.locals.iter_enumerated().find(|(_, d)| d.name == ssa::STACK_TOP)
        else {
            continue;
        };
        let is_st = |e: &Exp| e.base_variable().is_some_and(|v| *v.variable == Variable::Local(st));
        for (_, data) in f.blocks.iter_enumerated() {
            for s in data.iter() {
                let StatementKind::CallAssign { style, args, .. } = &s.kind else { continue };
                let n = args.iter().filter(|e| is_st(e)).count();
                if n == 0 {
                    continue;
                }
                match style {
                    CallStyle::DirectCall { call_edges: CallEdges::Explicit(edges) } => {
                        if edges.is_empty() {
                            *kinds.entry("direct-no-edge").or_default() += n;
                        } else if edges.iter().any(|e| defined.contains(e.as_str())) {
                            *kinds.entry("internal").or_default() += n;
                            *internals.entry(edges.join("|")).or_default() += n;
                        } else {
                            *kinds.entry("external").or_default() += n;
                            *externals.entry(edges.join("|")).or_default() += n;
                        }
                    }
                    CallStyle::FuncPtrCall { .. } => *kinds.entry("indirect").or_default() += n,
                    _ => *kinds.entry("other").or_default() += n,
                }
            }
        }
    }
    println!("call-arg escapes by callee kind: {kinds:?}");
    for (title, m) in [("external", &externals), ("internal", &internals)] {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("top {title} callees ({} distinct):", v.len());
        for (name, n) in v.into_iter().take(top) {
            println!("  {n:>6}  {name}");
        }
    }
}
