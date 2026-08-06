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
//!
//! The third sweep runs the same shapes through `index_engine::c_locals_trie`, the parallel
//! store `ascent_par!` uses, inserted concurrently from a rayon pool. Its `B/row` columns are
//! what answer "what does `DashMap` cost in memory"; read its `secs` column with the caveat in
//! [`build_par`]'s docs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ascent::internal::{
    CRelFullIndexWrite, RelFullIndexWrite, RelIndexMerge, ToRelIndex, ToRelIndex0,
};
use ctadl_ascent::index_engine::c_locals_trie::{CLocalsIndCommon, CToFull};
use ctadl_ascent::index_engine::hybrid_set::HybridSet;
use ctadl_ascent::index_engine::locals_trie::{LocalsIndCommon, ToFull};
use rayon::prelude::*;
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
    // Read before writing. `fetch_max` is a read-modify-write on a line every thread touches, and
    // the parallel sweep below runs 20 threads through here; once the peak is established almost
    // no allocation raises it, so the plain load turns nearly all of that traffic into a shared
    // read. The counters are still a serialization point the serial sweep does not pay in the same
    // way -- see `build_par`'s note on reading the `secs` column.
    if live > PEAK.load(Ordering::Relaxed) {
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
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
type CStore = CLocalsIndCommon<F, V, P, M, Fp>;

/// Leaf `i` of a group: spread over `paths` distinct `P` values, with a distinct `(M, Fp)` per
/// leaf so every leaf is unique within its group.
#[inline]
fn leaf(i: usize, paths: usize) -> (P, M, Fp) {
    ((i % paths) as P, (i / paths) as M, i as Fp)
}

/// The `(F, V)` key of group `g`. 64 groups per function, so `fidx` holds a realistic fan-out.
#[inline]
fn key(g: usize) -> (F, V) {
    ((g / 64) as F, g as V)
}

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

    let per_round = group_size.div_ceil(rounds);
    let mut emitted = 0usize;
    for _ in 0..rounds {
        let hi = (emitted + per_round).min(group_size);
        {
            let mut w = ToRelIndex::to_rel_index_write(&mut writer, &mut new);
            for g in 0..groups {
                let (f, v) = key(g);
                for i in emitted..hi {
                    let (p, m, fp) = leaf(i, paths);
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

/// The same build against the **parallel** store, driven the way `ascent_par!` drives it: each
/// round's leaves are inserted into `new` concurrently from a rayon pool, through
/// `to_c_rel_index_write` and `CRelFullIndexWrite::insert_if_not_present(&self, ..)`; then the
/// single-threaded `merge_delta_to_total_new_to_delta` moves delta into total.
///
/// Reading this against `build`'s columns answers the two open questions about the parallel
/// store with ground truth rather than a guess: what `DashMap`'s shard tables cost in bytes per
/// row, and how much a hot shard costs in time. Groups are partitioned across threads by `(F,V)`,
/// which is the realistic shape — a rule derives many different subjects at once — while a group
/// with `group_size` leaves still has every one of those leaves inserted under a single shard
/// lock, so the contention regime the module docs warn about is present at large group sizes.
///
/// Read the `secs` column as an upper bound on cost, not as a scaling measurement. Two things
/// inflate it and only the parallel sweep pays them: the counting allocator's `LIVE` counter is a
/// contended atomic on every allocation and free, and at large group sizes there are fewer groups
/// than threads (`ROWS / group_size` items to fan out over), so the parallel loop runs out of work
/// to hand out long before it runs out of threads. What the column *does* show honestly is the
/// shape: cost grows with group size, which is the single-shard-lock regime.
fn build_par(groups: usize, group_size: usize, rounds: usize, paths: usize) -> Run {
    let base = mem_reset();
    let start = Instant::now();

    let mut total = CStore::default();
    let mut delta = CStore::default();
    let mut new = CStore::default();
    let writer = CToFull::<F, V, P, M, Fp>::default();

    let per_round = group_size.div_ceil(rounds);
    let mut emitted = 0usize;
    for _ in 0..rounds {
        let hi = (emitted + per_round).min(group_size);
        {
            let w = ToRelIndex0::to_c_rel_index_write(&writer, &new);
            (0..groups).into_par_iter().for_each(|g| {
                let (f, v) = key(g);
                for i in emitted..hi {
                    let (p, m, fp) = leaf(i, paths);
                    CRelFullIndexWrite::insert_if_not_present(&w, &(f, v, p, m, fp), ());
                }
            });
        }
        emitted = hi;
        CStore::merge_delta_to_total_new_to_delta(&mut new, &mut delta, &mut total);
    }
    CStore::merge_delta_to_total_new_to_delta(&mut new, &mut delta, &mut total);

    let secs = start.elapsed().as_secs_f64();
    let mem = mem_since(base);
    assert_eq!(total.len(), groups * group_size, "rows built");
    let report = total.heap_report();
    assert_eq!(report.max_group, group_size, "group size actually reached");
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

    // The parallel store, same shapes and same pessimal merge, inserted concurrently. Compare the
    // `B/row` columns against the first sweep for `DashMap`'s memory overhead, and `secs` for what
    // the shard locks buy (or cost) at each group size.
    //
    // Spin the pool up first: rayon allocates its worker stacks on first use, and that one-time
    // cost would otherwise land in the first row's `peak` and `allocs`.
    (0..rayon::current_num_threads())
        .into_par_iter()
        .for_each(|_| {});
    println!(
        "\n== parallel store, {ROWS} rows, pessimal merge, {} rayon threads ==",
        rayon::current_num_threads()
    );
    header();
    for lg in 0..=MAX_LG {
        let group_size = 1usize << lg;
        let groups = ROWS / group_size;
        let r = build_par(groups, group_size, group_size, 1.max(group_size / 4));
        line("par", group_size, groups, &r);
    }

    if tsv {
        for r in rows_out {
            println!("{r}");
        }
    }
}
