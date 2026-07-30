//! Standalone benchmark for `index_engine::hybrid_set`: exact heap bytes and per-operation time
//! for **one set**, swept across the size range a real `Map<K, Set<V>>` index holds, against the
//! representations it is competing with.
//!
//! This is the data structure on its own — no `locals` store, no Ascent, no views. The
//! store-level and end-to-end numbers are `cargo bench -p ctadl-ascent --bench locals_trie` and
//! `scripts/locals-bench.py`; see `locals-trie-benchmark.md`.
//!
//! Run with:
//!     cargo bench -p ctadl-ascent --bench hybrid_set
//!     cargo bench -p ctadl-ascent --bench hybrid_set -- --tsv   # machine-readable
//!
//! Four representations, all holding the production leaf `(Path, FormalIndex, Path)` = 24 B:
//!
//! | column   | representation                                                            |
//! |----------|---------------------------------------------------------------------------|
//! | `hybrid` | [`HybridSet`]: linear probing under `SMALL_THRESHOLD`, `HashTable` above  |
//! | `vec64`  | the shipped predecessor: sorted `Vec` under 64 elements, `HashSet` above   |
//! | `vec32`  | the same, thresholded at 32 — isolates representation from threshold       |
//! | `hash`   | `hashbrown::HashSet` at every size, i.e. no hybrid at all                  |
//!
//! Every number is measured over `TOTAL` elements spread across `TOTAL / n` separate sets, so
//! the per-element figures are comparable across the sweep and the per-set overhead (which is
//! the whole point at small `n`) is amortized exactly as a real index amortizes it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cmp::Ordering as CmpOrdering;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ctadl_ascent::index_engine::hybrid_set::{HybridSet, SMALL_THRESHOLD};
use rustc_hash::FxHasher;

// ---------------------------------------------------------------------------
// Counting allocator (same one the locals_trie bench uses).
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

// SAFETY: every method forwards to `System` with the caller's unmodified pointer and layout, so
// the underlying allocator's contract is upheld verbatim; the counters are plain atomics that do
// not touch the allocation itself.
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

fn mem_reset() -> usize {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    LIVE.load(Ordering::Relaxed)
}

fn live_since(base: usize) -> usize {
    LIVE.load(Ordering::Relaxed).saturating_sub(base)
}

fn allocs_since() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// The element and the contenders.
// ---------------------------------------------------------------------------

/// The production leaf: `(Path, FormalIndex, Path)`, 24 B, 8-byte aligned.
type Leaf = (u64, i16, u64);

type Set<T> = hashbrown::HashSet<T, BuildHasherDefault<FxHasher>>;

/// The set operations an index needs of its value collection (`locals-trie-hybrid-ds.md`).
trait Bag: Default {
    /// Displayed name.
    const NAME: &'static str;
    fn insert(&mut self, value: Leaf) -> bool;
    fn contains(&self, value: &Leaf) -> bool;
    /// Union `other` in; returns how many elements were newly added to `self`.
    fn merge(&mut self, other: Self) -> usize;
    fn len(&self) -> usize;
}

impl Bag for HybridSet<Leaf> {
    const NAME: &'static str = "hybrid";
    #[inline]
    fn insert(&mut self, value: Leaf) -> bool {
        HybridSet::insert(self, value)
    }
    #[inline]
    fn contains(&self, value: &Leaf) -> bool {
        HybridSet::contains(self, value)
    }
    #[inline]
    fn merge(&mut self, other: Self) -> usize {
        HybridSet::merge(self, other)
    }
    #[inline]
    fn len(&self) -> usize {
        HybridSet::len(self)
    }
}

impl Bag for Set<Leaf> {
    const NAME: &'static str = "hash";
    #[inline]
    fn insert(&mut self, value: Leaf) -> bool {
        hashbrown::HashSet::insert(self, value)
    }
    #[inline]
    fn contains(&self, value: &Leaf) -> bool {
        hashbrown::HashSet::contains(self, value)
    }
    #[inline]
    fn merge(&mut self, other: Self) -> usize {
        let before = self.len();
        self.extend(other);
        self.len() - before
    }
    #[inline]
    fn len(&self) -> usize {
        hashbrown::HashSet::len(self)
    }
}

// ---- the shipped predecessor: sorted Vec, promoted to a HashSet past THRESHOLD -------------
//
// Reproduced here (rather than measured from the old binary) so both representations are timed
// by the same harness, in the same process, over the same data. The algorithms are the ones
// `locals_trie` shipped: `binary_search` + `Vec::insert` to keep order, a linear two-way merge
// for the union, and a `merge_size` pre-pass so a union that will exceed the threshold is built
// straight into a `HashSet` instead of into a `Vec` the promotion would immediately free.

enum SortedThenHash<const THRESHOLD: usize> {
    Small(Vec<Leaf>),
    Large(Set<Leaf>),
}

impl<const THRESHOLD: usize> Default for SortedThenHash<THRESHOLD> {
    fn default() -> Self {
        Self::Small(Vec::new())
    }
}

/// Merge sorted, deduplicated `src` into sorted, deduplicated `dst`; returns how many elements
/// were newly added.
fn merge_sorted<T: Ord>(dst: &mut Vec<T>, src: Vec<T>) -> usize {
    if src.is_empty() {
        return 0;
    }
    if dst.is_empty() {
        let n = src.len();
        *dst = src;
        return n;
    }
    let old = std::mem::take(dst);
    let mut merged = Vec::with_capacity(old.len() + src.len());
    let mut oi = old.into_iter().peekable();
    let mut si = src.into_iter().peekable();
    let mut added = 0usize;
    loop {
        match (oi.peek(), si.peek()) {
            (Some(a), Some(b)) => match a.cmp(b) {
                CmpOrdering::Less => merged.push(oi.next().unwrap()),
                CmpOrdering::Greater => {
                    merged.push(si.next().unwrap());
                    added += 1;
                }
                CmpOrdering::Equal => {
                    merged.push(oi.next().unwrap());
                    si.next();
                }
            },
            (Some(_), None) => merged.push(oi.next().unwrap()),
            (None, Some(_)) => {
                merged.push(si.next().unwrap());
                added += 1;
            }
            (None, None) => break,
        }
    }
    *dst = merged;
    added
}

/// Size of the union of two sorted, deduplicated slices. O(m+n), allocation-free.
fn merge_size<T: Ord>(dst: &[T], src: &[T]) -> usize {
    let mut oi = dst.iter().peekable();
    let mut si = src.iter().peekable();
    let mut count = 0usize;
    loop {
        match (oi.peek(), si.peek()) {
            (Some(a), Some(b)) => {
                match a.cmp(b) {
                    CmpOrdering::Less => {
                        oi.next();
                    }
                    CmpOrdering::Greater => {
                        si.next();
                    }
                    CmpOrdering::Equal => {
                        oi.next();
                        si.next();
                    }
                }
                count += 1;
            }
            (Some(_), None) => {
                count += oi.count();
                break;
            }
            (None, Some(_)) => {
                count += si.count();
                break;
            }
            (None, None) => break,
        }
    }
    count
}

impl<const THRESHOLD: usize> SortedThenHash<THRESHOLD> {
    fn promote(&mut self) {
        if let Self::Small(v) = self {
            *self = Self::Large(std::mem::take(v).into_iter().collect());
        }
    }
}

impl Bag for SortedThenHash<64> {
    const NAME: &'static str = "vec64";
    #[inline]
    fn insert(&mut self, value: Leaf) -> bool {
        sth_insert::<64>(self, value)
    }
    #[inline]
    fn contains(&self, value: &Leaf) -> bool {
        sth_contains::<64>(self, value)
    }
    #[inline]
    fn merge(&mut self, other: Self) -> usize {
        sth_merge::<64>(self, other)
    }
    #[inline]
    fn len(&self) -> usize {
        sth_len::<64>(self)
    }
}

impl Bag for SortedThenHash<32> {
    const NAME: &'static str = "vec32";
    #[inline]
    fn insert(&mut self, value: Leaf) -> bool {
        sth_insert::<32>(self, value)
    }
    #[inline]
    fn contains(&self, value: &Leaf) -> bool {
        sth_contains::<32>(self, value)
    }
    #[inline]
    fn merge(&mut self, other: Self) -> usize {
        sth_merge::<32>(self, other)
    }
    #[inline]
    fn len(&self) -> usize {
        sth_len::<32>(self)
    }
}

fn sth_len<const T: usize>(s: &SortedThenHash<T>) -> usize {
    match s {
        SortedThenHash::Small(v) => v.len(),
        SortedThenHash::Large(h) => h.len(),
    }
}

fn sth_contains<const T: usize>(s: &SortedThenHash<T>, value: &Leaf) -> bool {
    match s {
        SortedThenHash::Small(v) => v.binary_search(value).is_ok(),
        SortedThenHash::Large(h) => h.contains(value),
    }
}

fn sth_insert<const T: usize>(s: &mut SortedThenHash<T>, value: Leaf) -> bool {
    match s {
        SortedThenHash::Small(v) => match v.binary_search(&value) {
            Ok(_) => false,
            Err(pos) => {
                v.insert(pos, value);
                if v.len() > T {
                    s.promote();
                }
                true
            }
        },
        SortedThenHash::Large(h) => h.insert(value),
    }
}

fn sth_merge<const T: usize>(s: &mut SortedThenHash<T>, other: SortedThenHash<T>) -> usize {
    match (
        std::mem::replace(s, SortedThenHash::Small(Vec::new())),
        other,
    ) {
        (SortedThenHash::Small(mut dst), SortedThenHash::Small(src)) => {
            if merge_size(&dst, &src) > T {
                let before = dst.len();
                let mut set: Set<Leaf> = dst.into_iter().collect();
                for leaf in src {
                    set.insert(leaf);
                }
                let added = set.len() - before;
                *s = SortedThenHash::Large(set);
                added
            } else {
                let added = merge_sorted(&mut dst, src);
                *s = SortedThenHash::Small(dst);
                added
            }
        }
        (this, other) => {
            let (mut set, before) = match this {
                SortedThenHash::Large(h) => {
                    let n = h.len();
                    (h, n)
                }
                SortedThenHash::Small(v) => {
                    let h: Set<Leaf> = v.into_iter().collect();
                    let n = h.len();
                    (h, n)
                }
            };
            match other {
                SortedThenHash::Small(v) => set.extend(v),
                SortedThenHash::Large(h) => set.extend(h),
            }
            let added = set.len() - before;
            *s = SortedThenHash::Large(set);
            added
        }
    }
}

// ---------------------------------------------------------------------------
// Measurements.
// ---------------------------------------------------------------------------

/// Elements touched per configuration, spread over `TOTAL / n` sets.
const TOTAL: usize = 1 << 20;

/// Leaf `i` of a set: spread over `paths` distinct `P`, distinct `(M, Fp)` per leaf, so every
/// leaf is unique within its set. Same generator as the `locals_trie` bench.
#[inline]
fn leaf(i: usize, paths: usize) -> Leaf {
    ((i % paths) as u64, (i / paths) as i16, i as u64)
}

/// A leaf that is *not* in a set built from `0..n`.
#[inline]
fn absent(i: usize, n: usize) -> Leaf {
    (u64::MAX, -1, (n + i) as u64)
}

struct Row {
    /// Live bytes for all `sets` sets built element by element, per element.
    bytes_per_elem: f64,
    /// Heap allocations per set over the same build.
    allocs_per_set: f64,
    /// Nanoseconds per `insert` while building.
    insert_ns: f64,
    /// Nanoseconds per `contains` that hits.
    hit_ns: f64,
    /// Nanoseconds per `contains` that misses.
    miss_ns: f64,
    /// Nanoseconds per element to build the same sets by merging `ROUNDS` deltas instead.
    merge_ns: f64,
    /// Live bytes per element of the merge-built sets (they can differ: growth history matters).
    merge_bytes_per_elem: f64,
}

/// Deltas merged in to build one set, mirroring the ~6 semi-naive iterations a real fixpoint
/// takes (`locals-trie-benchmark.md` §4). Held constant across `n` so the merge column measures
/// merge cost per element rather than a varying number of merges.
const ROUNDS: usize = 8;

fn measure<B: Bag>(n: usize, paths: usize) -> Row {
    let sets = (TOTAL / n).max(1);

    // ---- build by insert: bytes, allocations, time -------------------------
    let base = mem_reset();
    let start = Instant::now();
    let mut bags: Vec<B> = Vec::with_capacity(sets);
    for _ in 0..sets {
        let mut bag = B::default();
        for i in 0..n {
            bag.insert(leaf(i, paths));
        }
        bags.push(bag);
    }
    let insert_secs = start.elapsed().as_secs_f64();
    // The `Vec` holding the sets is part of the harness, not the structure: subtract it.
    let harness = bags.capacity() * std::mem::size_of::<B>();
    let live = live_since(base).saturating_sub(harness);
    let allocs = allocs_since();
    assert!(bags.iter().all(|b| b.len() == n), "built {n} elements");

    // ---- lookups ----------------------------------------------------------
    let mut found = 0usize;
    let start = Instant::now();
    for bag in &bags {
        for i in 0..n {
            found += bag.contains(&leaf(i, paths)) as usize;
        }
    }
    let hit_secs = start.elapsed().as_secs_f64();
    assert_eq!(found, sets * n, "every element must be found");

    let start = Instant::now();
    for bag in &bags {
        for i in 0..n {
            found += bag.contains(&absent(i, n)) as usize;
        }
    }
    let miss_secs = start.elapsed().as_secs_f64();
    assert_eq!(found, sets * n, "no absent element may be found");
    drop(bags);

    // ---- build by merging deltas -----------------------------------------
    // What a semi-naive fixpoint actually does to the value collection: accumulate into one
    // set by unioning a fresh delta into it once per iteration. This is the operation that made
    // the sorted `Vec` quadratic on large sets.
    let rounds = ROUNDS.min(n);
    let per_round = n.div_ceil(rounds);
    let base = mem_reset();
    let start = Instant::now();
    let mut bags: Vec<B> = Vec::with_capacity(sets);
    for _ in 0..sets {
        let mut bag = B::default();
        let mut emitted = 0;
        while emitted < n {
            let hi = (emitted + per_round).min(n);
            let mut delta = B::default();
            for i in emitted..hi {
                delta.insert(leaf(i, paths));
            }
            let added = bag.merge(delta);
            assert_eq!(added, hi - emitted, "merge must report the new elements");
            emitted = hi;
        }
        bags.push(bag);
    }
    let merge_secs = start.elapsed().as_secs_f64();
    let harness = bags.capacity() * std::mem::size_of::<B>();
    let merge_live = live_since(base).saturating_sub(harness);
    assert!(bags.iter().all(|b| b.len() == n), "merged {n} elements");
    drop(bags);

    let elems = (sets * n) as f64;
    Row {
        bytes_per_elem: live as f64 / elems,
        allocs_per_set: allocs as f64 / sets as f64,
        insert_ns: insert_secs * 1e9 / elems,
        hit_ns: hit_secs * 1e9 / elems,
        miss_ns: miss_secs * 1e9 / elems,
        merge_ns: merge_secs * 1e9 / elems,
        merge_bytes_per_elem: merge_live as f64 / elems,
    }
}

fn main() {
    let tsv = std::env::args().any(|a| a == "--tsv");
    let mut data: Vec<String> = Vec::new();

    println!(
        "element = {} B, SMALL_THRESHOLD = {}",
        std::mem::size_of::<Leaf>(),
        SMALL_THRESHOLD
    );
    println!(
        "struct sizes: HybridSet {} B | sorted-Vec-then-HashSet {} B | HashSet {} B | Vec {} B",
        std::mem::size_of::<HybridSet<Leaf>>(),
        std::mem::size_of::<SortedThenHash<64>>(),
        std::mem::size_of::<Set<Leaf>>(),
        std::mem::size_of::<Vec<Leaf>>(),
    );
    println!(
        "\n{TOTAL} elements per row, spread over TOTAL/n sets; `merge` builds the same sets from \
         {ROUNDS} deltas instead of by insert."
    );

    let header = || {
        println!(
            "\n{:>6} {:>8} {:>8} {:>8} {:>9} {:>8} {:>8} {:>9} {:>9} {:>9}",
            "n",
            "impl",
            "sets",
            "B/elem",
            "allocs/set",
            "ins ns",
            "hit ns",
            "miss ns",
            "merge ns",
            "mrg B/el",
        );
    };

    let sizes: [usize; 16] = [1, 2, 3, 5, 8, 16, 24, 31, 32, 33, 40, 48, 64, 65, 128, 1024];

    header();
    for (row_no, n) in sizes.into_iter().enumerate() {
        let paths = 1.max(n / 4);
        let mut line = |name: &str, r: Row| {
            println!(
                "{:>6} {:>8} {:>8} {:>8.1} {:>9.2} {:>8.1} {:>8.1} {:>9.1} {:>9.1} {:>9.1}",
                n,
                name,
                (TOTAL / n).max(1),
                r.bytes_per_elem,
                r.allocs_per_set,
                r.insert_ns,
                r.hit_ns,
                r.miss_ns,
                r.merge_ns,
                r.merge_bytes_per_elem,
            );
            data.push(format!(
                "DATA\t{name}\t{n}\t{:.2}\t{:.3}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}",
                r.bytes_per_elem,
                r.allocs_per_set,
                r.insert_ns,
                r.hit_ns,
                r.miss_ns,
                r.merge_ns,
                r.merge_bytes_per_elem
            ));
        };
        line(
            <HybridSet<Leaf> as Bag>::NAME,
            measure::<HybridSet<Leaf>>(n, paths),
        );
        line(
            <SortedThenHash<64> as Bag>::NAME,
            measure::<SortedThenHash<64>>(n, paths),
        );
        line(
            <SortedThenHash<32> as Bag>::NAME,
            measure::<SortedThenHash<32>>(n, paths),
        );
        line(<Set<Leaf> as Bag>::NAME, measure::<Set<Leaf>>(n, paths));
        if row_no + 1 < sizes.len() {
            println!();
        }
    }

    if tsv {
        println!();
        for d in data {
            println!("{d}");
        }
    }
}
