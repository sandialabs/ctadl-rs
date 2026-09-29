//! Per-function census of stack-frame accesses in an import, to size a mem2reg pass: how many
//! loads and stores go through `__stack_top.[k].deref`, and where the frame's address escapes
//! (copied, stored, passed to a call, returned), which is what blocks promoting a slot.
//!
//!   cargo run --release -p ctadl-import --example stack_census -- <import-dir> [top-n] [detail-fn]
//!
//! Runs dead-temps and coalescing first (no SSA), since that is where mem2reg would run.
use std::collections::{BTreeMap, BTreeSet};

use ctadl_import::SourceInfoMode;
use ctadl_import::load_import;
use ctadl_ir::mir::{AccessPath, CallStyle, Exp, StatementKind, TerminatorKind, Variable};
use ctadl_ir::{FunctionData, LocalIdx, ssa};

#[derive(Default, Clone)]
struct Row {
    loads: usize,
    stores: usize,
    load_slots: BTreeMap<i64, usize>,
    store_slots: BTreeMap<i64, usize>,
    /// kind -> offsets whose address escaped that way
    escapes: BTreeMap<&'static str, BTreeMap<i64, usize>>,
    /// statements that define `__stack_top` itself
    redefs: usize,
    /// src occurrences of `__stack_top` this census did not classify
    unclassified: usize,
}

fn off(ap: &AccessPath) -> i64 {
    ap.accesses.offsets.iter().map(|a| a.offset().0).sum()
}

fn exp_off(e: &Exp, st: LocalIdx) -> Option<i64> {
    match e {
        Exp::Variable(v) if *v.variable == Variable::Local(st) => Some(0),
        Exp::AccessPath(ap) if *ap.base.variable == Variable::Local(st) => Some(off(ap)),
        _ => None,
    }
}

fn census(f: &FunctionData, st: LocalIdx) -> Row {
    let mut r = Row::default();
    let is_st = |v: &ctadl_ir::VariableRef| *v.variable == Variable::Local(st);
    let mut esc = |r: &mut Row, kind: &'static str, k: i64| {
        *r.escapes.entry(kind).or_default().entry(k).or_default() += 1;
    };
    for (_, data) in f.blocks.iter_enumerated() {
        for s in data.iter() {
            let total = s.iter_src_var().filter(|v| is_st(v)).count();
            let mut seen = 0;
            if s.iter_dst_var().any(|v| is_st(v)) {
                r.redefs += 1;
            }
            match &s.kind {
                StatementKind::Load { source, field, .. } if is_st(&source.base) => {
                    seen += 1;
                    if &*field.field == "deref" {
                        r.loads += 1;
                        *r.load_slots.entry(off(source)).or_default() += 1;
                    } else {
                        esc(&mut r, "load-nonderef", off(source));
                    }
                }
                StatementKind::Store { dest, value, .. } => {
                    if is_st(&dest.base) {
                        seen += 1;
                        r.stores += 1;
                        *r.store_slots.entry(off(dest)).or_default() += 1;
                    }
                    if let Some(k) = exp_off(value, st) {
                        seen += 1;
                        esc(&mut r, "stored", k);
                    }
                }
                StatementKind::Assign { sources, .. } => {
                    for e in sources {
                        if let Some(k) = exp_off(e, st) {
                            seen += 1;
                            esc(&mut r, "assigned", k);
                        }
                    }
                }
                StatementKind::CallAssign { style, args, .. } => {
                    for e in args {
                        if let Some(k) = exp_off(e, st) {
                            seen += 1;
                            esc(&mut r, "call-arg", k);
                        }
                    }
                    if let CallStyle::FuncPtrCall { callee, .. } = style
                        && is_st(&callee.base)
                    {
                        seen += 1;
                        esc(&mut r, "callee", off(callee));
                    }
                }
                _ => {}
            }
            r.unclassified += total.saturating_sub(seen);
        }
        if let Some(t) = &data.terminator
            && let TerminatorKind::Return { args } = &t.kind
        {
            for e in args {
                if let Some(k) = exp_off(e, st) {
                    esc(&mut r, "returned", k);
                }
            }
        }
    }
    r
}

/// Loads and stores on slots that no escape touches: exactly-escaped offsets only, and every
/// offset at or above the lowest escaped offset (an escaped address may reach anything above it).
fn promotable(r: &Row) -> (usize, usize, usize, usize) {
    let escaped: BTreeSet<i64> = r.escapes.values().flat_map(|m| m.keys().copied()).collect();
    let min_esc = escaped.iter().next().copied();
    let exact = |k: &i64| !escaped.contains(k);
    let below = |k: &i64| min_esc.is_none_or(|m| *k < m);
    let sum = |m: &BTreeMap<i64, usize>, p: &dyn Fn(&i64) -> bool| -> usize {
        m.iter().filter(|(k, _)| p(k)).map(|(_, n)| n).sum()
    };
    (
        sum(&r.load_slots, &exact),
        sum(&r.store_slots, &exact),
        sum(&r.load_slots, &below),
        sum(&r.store_slots, &below),
    )
}

/// Loads plus stores on slots outside every escaped region, where an escape at `k` covers
/// `[k, end)` and `end` is the next escaped offset (`window = None`) or `k + window`.
fn promotable_region(r: &Row, window: Option<i64>) -> usize {
    let escaped: Vec<i64> = r
        .escapes
        .values()
        .flat_map(|m| m.keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let covered = |k: i64| {
        escaped.iter().enumerate().any(|(i, &e)| {
            let end = match window {
                Some(w) => e + w,
                None => escaped.get(i + 1).copied().unwrap_or(i64::MAX),
            };
            e <= k && k < end
        })
    };
    r.load_slots
        .iter()
        .chain(r.store_slots.iter())
        .filter(|(k, _)| !covered(**k))
        .map(|(_, n)| n)
        .sum()
}

fn pct(a: usize, b: usize) -> String {
    if b == 0 { "-".into() } else { format!("{:.0}%", 100.0 * a as f64 / b as f64) }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let top: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let detail = args.get(3);
    let import = ctadl_import::store::resolve_import(dir).expect("import");
    let mut info = load_import(&import, SourceInfoMode::Skip).expect("load");
    ssa::run_pipeline(
        &mut info.program,
        ssa::Pipeline { dead_temps: true, coalesce: true, ..ssa::Pipeline::none() },
    );
    let mut rows: Vec<(String, Row)> = Vec::new();
    for f in info.program.functions.iter() {
        let Some((st, _)) = f.locals.iter_enumerated().find(|(_, d)| d.name == "__stack_top") else {
            continue;
        };
        let r = census(f, st);
        if detail == Some(&f.name) {
            println!("== {} ==", f.name);
            println!("redefs={} unclassified={}", r.redefs, r.unclassified);
            for (kind, m) in &r.escapes {
                println!("escape {kind}: {} sites over {} offsets: {:?}", m.values().sum::<usize>(), m.len(), m);
            }
            println!("load slots: {:?}", r.load_slots.keys().collect::<Vec<_>>());
            println!("store slots: {:?}", r.store_slots.keys().collect::<Vec<_>>());
        }
        rows.push((f.name.clone(), r));
    }
    rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.loads + r.stores));
    println!(
        "{:<24} {:>7} {:>7} {:>6} {:>6} {:>6} {:>6} {:>6} {:>9} {:>9}",
        "function", "loads", "stores", "slots", "escs", "escofs", "redef", "uncls", "prom-ex", "prom-abv"
    );
    let (mut tl, mut ts, mut te, mut tpl, mut tps, mut tal, mut tas, mut noesc) = (0, 0, 0, 0, 0, 0, 0, 0);
    for (i, (n, r)) in rows.iter().enumerate() {
        let (pl, ps, al, as_) = promotable(r);
        let escs: usize = r.escapes.values().flat_map(|m| m.values()).sum();
        let escofs: BTreeSet<i64> = r.escapes.values().flat_map(|m| m.keys().copied()).collect();
        let slots: BTreeSet<i64> = r.load_slots.keys().chain(r.store_slots.keys()).copied().collect();
        tl += r.loads;
        ts += r.stores;
        te += escs;
        tpl += pl;
        tps += ps;
        tal += al;
        tas += as_;
        if escs == 0 {
            noesc += 1;
        }
        if i < top {
            println!(
                "{:<24} {:>7} {:>7} {:>6} {:>6} {:>6} {:>6} {:>6} {:>9} {:>9}",
                n, r.loads, r.stores, slots.len(), escs, escofs.len(), r.redefs, r.unclassified,
                pct(pl + ps, r.loads + r.stores), pct(al + as_, r.loads + r.stores)
            );
        }
    }
    for w in [None, Some(8), Some(16), Some(32), Some(64), Some(128)] {
        let all: usize = rows.iter().map(|(_, r)| promotable_region(r, w)).sum();
        let top1 = rows.iter().find(|(n, _)| Some(n) == detail).map(|(_, r)| {
            pct(promotable_region(r, w), r.loads + r.stores)
        });
        println!("region model {:?}: all {} ({}) detail-fn {:?}", w, all, pct(all, tl + ts), top1);
    }
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, r) in &rows {
        for (k, m) in &r.escapes {
            *kinds.entry(k).or_default() += m.values().sum::<usize>();
        }
    }
    println!(
        "TOTAL functions={} no-escape={} loads={tl} stores={ts} escapes={te} {:?}\n  promotable (exact-offset escape): {} ({})\n  promotable (escape taints all offsets >= it): {} ({})",
        rows.len(), noesc, kinds, tpl + tps, pct(tpl + tps, tl + ts), tal + tas, pct(tal + tas, tl + ts)
    );
}
