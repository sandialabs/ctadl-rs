//! Structure-level benchmark for `index_engine::locals_trie`: exact heap bytes and time for the
//! `locals` store as a function of `(F,V)` **group size** — the one shape parameter the
//! module's design (a linear-probing small set vs. a promoted Swiss table) turns on.
//!
//! Run with:
//!     cargo bench -p ctadl-ascent --bench locals_trie
//!     cargo bench -p ctadl-ascent --bench locals_trie -- --tsv   # machine-readable
//!
//! Why a bench and not the end-to-end `ctadl index` measurement (see
//! `scripts/locals-bench.py`): process `phys_footprint` cannot attribute bytes to one
//! sub-structure, and at the scales a synthetic program reaches, the store is a small slice
//! of the process. Here a counting global allocator measures *only* what the store
//! allocates, so bytes/row is ground truth rather than an estimate — which also lets us
//! check `HeapReport`'s own estimator against it: the `est/real` column below is
//! `locals_trie::hb_bytes`'s whole-store prediction over the counting allocator's truth. (The
//! only per-table check left on `hb_bytes` is `hybrid_set`'s `bucket_counts_track_hashbrown`,
//! which covers 8 B elements in tables of 8 buckets or more. hashbrown's 4-bucket floor, which
//! `hb_buckets` models with its `.max(4)`, has no direct test; this whole-store ratio is what
//! stands in for one.)
//!
//! Leaf/key types are plain integers chosen to have the same sizes and hashing cost as the
//! production instantiation (`FunctionId`, `FlowVariable`, `Path`, `FormalIndex`, `Path`):
//! the leaf `(P,M,Fp)` is 24 B either way. Using the real interned types would add *their*
//! allocations to the counter, which is exactly what this bench is trying to isolate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ascent::internal::{RelFullIndexWrite, RelIndexMerge, ToRelIndex};
use ctadl_ascent::index_engine::hybrid_set::HybridSet;
use ctadl_ascent::index_engine::locals_trie::{LocalsIndCommon, ToFull};
use rustc_hash::FxHasher;

// ---------------------------------------------------------------------------
// Counting allocator: current / peak live bytes and allocation count.
// ---------------------------------------------------------------------------

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn record_alloc(size: usize) {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's unmodified pointer and layout,
// so the underlying allocator's contract is upheld verbatim; the counters are plain atomics
// that do not touch the allocation itself.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is the caller's, passed through unchanged.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            record_alloc(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` are the caller's matched pair, passed through unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`/`layout`/`new_size` are the caller's, passed through unchanged.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            record_alloc(new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Live/peak/count snapshot of the allocator, relative to a baseline.
#[derive(Clone, Copy)]
struct Mem {
    live: usize,
    peak: usize,
    allocs: usize,
}

/// One `build()` measurement.
struct Run {
    secs: f64,
    /// Peak live bytes during the build (total + delta + new + transients).
    peak: usize,
    /// Live bytes with all three semi-naive copies present, as Ascent holds them at fixpoint.
    live_all: usize,
    /// Live bytes of the `total` store alone (delta/new dropped) -- the steady-state size of
    /// the data structure, and the only thing `heap_report` claims to describe.
    live_total: usize,
    /// `heap_report()`'s own estimate for `total`, for accuracy checking.
    est_total: usize,
    allocs: usize,
}

fn mem_reset() -> usize {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    LIVE.load(Ordering::Relaxed)
}

fn mem_since(base: usize) -> Mem {
    Mem {
        live: LIVE.load(Ordering::Relaxed).saturating_sub(base),
        peak: PEAK.load(Ordering::Relaxed).saturating_sub(base),
        allocs: ALLOCS.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// The store under test, at production element sizes.
// ---------------------------------------------------------------------------

type F = u32; // FunctionId
type V = u64; // FlowVariable
type P = u64; // Path (interned handle)
type M = i16; // FormalIndex
type Fp = u64; // Path (interned handle)
type Store = LocalsIndCommon<F, V, P, M, Fp>;

/// Drive the store the way Ascent's semi-naive loop does: derive this round's leaves into
/// `new`, then `merge_delta_to_total_new_to_delta` (delta -> total, swap new/delta).
///
/// `rounds` is the knob that exposes the merge cost: `rounds == group_size` is the pessimal
/// shape the module docs describe (one new leaf per group per iteration, so every round pays
/// a merge into every group); `rounds == 1` is a single bulk build, i.e. structure cost with
/// the merge cost removed.
fn build(groups: usize, group_size: usize, rounds: usize, paths: usize) -> Run {
    let base = mem_reset();
    let start = Instant::now();

    let mut total = Store::default();
    let mut delta = Store::default();
    let mut new = Store::default();
    let mut writer = ToFull::<F, V, P, M, Fp>::default();

    // Leaf `i` of a group: spread over `paths` distinct `P` values, with a distinct
    // `(M, Fp)` per leaf so every leaf is unique within its group.
    let leaf = |i: usize| ((i % paths) as P, (i / paths) as M, i as Fp);

    let per_round = group_size.div_ceil(rounds);
    let mut emitted = 0usize;
    for _ in 0..rounds {
        let hi = (emitted + per_round).min(group_size);
        {
            let mut w = writer.to_rel_index_write(&mut new);
            for g in 0..groups {
                let (f, v) = ((g / 64) as F, g as V);
                for i in emitted..hi {
                    let (p, m, fp) = leaf(i);
                    w.insert_if_not_present(&(f, v, p, m, fp), ());
                }
            }
        }
        emitted = hi;
        Store::merge_delta_to_total_new_to_delta(&mut new, &mut delta, &mut total);
    }
    // Flush the last delta so `total` holds everything (Ascent does this on loop exit).
    Store::merge_delta_to_total_new_to_delta(&mut new, &mut delta, &mut total);

    let secs = start.elapsed().as_secs_f64();
    let mem = mem_since(base);
    assert_eq!(total.len(), groups * group_size, "rows built");
    let report = total.heap_report();
    assert_eq!(report.max_group, group_size, "group size actually reached");
    // `drain()` does not free a hashbrown table, so the emptied `delta`/`new` stores keep
    // whatever outer-map capacity their widest iteration reached, for the whole run. Drop
    // them to separate that retention from the steady-state size of `total`.
    drop((delta, new));
    let live_total = mem_since(base).live;
    drop(total);
    Run {
        secs,
        peak: mem.peak,
        live_all: mem.live,
        live_total,
        est_total: report.fwd_bytes + report.fidx_bytes,
        allocs: mem.allocs,
    }
}

fn main() {
    let tsv = std::env::args().any(|a| a == "--tsv");
    // Ignore criterion-style flags cargo passes through (`--bench`, `--save-baseline`, ...).

    // The leaf is the size that matters and it matches production (24 B); the outer key is
    // (F,V) plus the group.
    println!(
        "leaf (P,M,Fp) = {} B, key (F,V) = {} B, group HybridSet = {} B \
         (was: Vec {} B | HashSet {} B), outer entry = {} B",
        std::mem::size_of::<(P, M, Fp)>(),
        std::mem::size_of::<(F, V)>(),
        std::mem::size_of::<HybridSet<(P, M, Fp)>>(),
        std::mem::size_of::<Vec<(P, M, Fp)>>(),
        std::mem::size_of::<hashbrown::HashSet<(P, M, Fp), BuildHasherDefault<FxHasher>>>(),
        std::mem::size_of::<((F, V), HybridSet<(P, M, Fp)>)>(),
    );

    // Sweep group size at constant total rows, so time and bytes/row are comparable across
    // the sweep. `hybrid_set::SMALL_THRESHOLD` is where a group stops being a linear-probing
    // table and becomes a Swiss table.
    const ROWS: usize = 1 << 20;
    let mut rows_out: Vec<String> = Vec::new();
    let header = || {
        println!(
            "{:>7} {:>9} {:>10} {:>8} {:>10} {:>8} {:>10} {:>9} {:>8} {:>9}",
            "group",
            "groups",
            "total B",
            "B/row",
            "+delta B",
            "B/row",
            "peak B",
            "allocs",
            "secs",
            "est/real"
        );
    };
    let mut line = |tag: &str, group_size: usize, groups: usize, r: &Run| {
        let rows = groups * group_size;
        println!(
            "{:>7} {:>9} {:>10} {:>8.1} {:>10} {:>8.1} {:>10} {:>9} {:>8.3} {:>9.2}",
            group_size,
            groups,
            r.live_total,
            r.live_total as f64 / rows as f64,
            r.live_all,
            r.live_all as f64 / rows as f64,
            r.peak,
            r.allocs,
            r.secs,
            r.est_total as f64 / r.live_total as f64,
        );
        rows_out.push(format!(
            "DATA\t{tag}\t{group_size}\t{groups}\t{rows}\t{}\t{}\t{}\t{}\t{:.4}",
            r.live_total, r.live_all, r.peak, r.est_total, r.secs
        ));
    };

    const MAX_LG: u32 = 16;
    println!(
        "\n== group-size sweep, {ROWS} rows, one leaf per group per round (pessimal merge) =="
    );
    header();
    for lg in 0..=MAX_LG {
        let group_size = 1usize << lg;
        let groups = ROWS / group_size;
        let r = build(groups, group_size, group_size, 1.max(group_size / 4));
        line("sweep", group_size, groups, &r);
    }

    println!("\n== same sizes, single bulk round (structure cost, merge cost removed) ==");
    header();
    for lg in 0..=MAX_LG {
        let group_size = 1usize << lg;
        let groups = ROWS / group_size;
        let r = build(groups, group_size, 1, 1.max(group_size / 4));
        line("bulk", group_size, groups, &r);
    }

    if tsv {
        for r in rows_out {
            println!("{r}");
        }
    }
}
