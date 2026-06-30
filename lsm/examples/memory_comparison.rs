//! Heap-residency comparison: a fully heap-resident index vs. the mmap-backed
//! LSM, as the data grows past what you'd want to keep in RAM.
//!
//! Criterion measures time, not memory, so this is a standalone example with a
//! counting global allocator that tracks live (and peak) heap bytes. The
//! `BTreeMap<u64, Vec<u64>>` stands in for the default Ascent provider's
//! heap-resident storage (`Vec`s + hash indices): every fact lives on the heap,
//! so its footprint grows linearly. The `MmapLsmMultiMap` keeps only a bounded
//! memtable plus small per-run metadata on the heap; the facts themselves live
//! in memory-mapped run files (page cache, evictable — not heap).
//!
//! Run with: `cargo run --release --example memory_comparison`

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsm::MmapLsmMultiMap;

/// A `System`-wrapping allocator that tracks currently-live and peak heap bytes.
struct CountingAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    // `GlobalAlloc::realloc`'s default impl routes through `alloc`/`dealloc`
    // above, so `Vec` growth is accounted for without overriding it.
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// Resets the peak watermark to the current live total before a measurement.
fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Eight values per key, so the maps are genuine multi-maps.
const VALUES_PER_KEY: u64 = 8;
/// Bounded write buffer: the LSM's heap stays ~flat regardless of total facts.
const MEMTABLE_LIMIT: usize = 1024;

fn pairs(n: usize) -> impl Iterator<Item = (u64, u64)> {
    (0..n as u64).map(|i| (i / VALUES_PER_KEY, i))
}

/// Builds a fully heap-resident index and returns (live, peak) heap bytes while
/// it is alive.
fn measure_in_memory(n: usize) -> (usize, usize) {
    let base = live();
    reset_peak();
    let mut map: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (key, value) in pairs(n) {
        map.entry(key).or_default().push(value);
    }
    let result = (live() - base, peak().saturating_sub(base));
    std::hint::black_box(&map);
    result
}

/// Builds the mmap LSM (flushing everything to disk) and returns (live, peak)
/// heap bytes while it is alive.
fn measure_mmap(n: usize) -> (usize, usize) {
    let base = live();
    reset_peak();
    let mut lsm = MmapLsmMultiMap::<u64, u64>::with_memtable_limit(MEMTABLE_LIMIT);
    for (key, value) in pairs(n) {
        lsm.insert(key, value);
    }
    lsm.flush().expect("flush failed");
    let result = (live() - base, peak().saturating_sub(base));
    std::hint::black_box(&lsm);
    result
}

fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

fn main() {
    println!(
        "{:>9}  {:>14}  {:>14}  {:>12}  {:>14}",
        "facts", "in-mem live", "mmap live", "mmap/in-mem", "mmap peak"
    );
    println!("{}", "-".repeat(72));
    for &n in &[10_000usize, 50_000, 100_000, 200_000, 400_000] {
        let (mem_live, _mem_peak) = measure_in_memory(n);
        let (mmap_live, mmap_peak) = measure_mmap(n);
        println!(
            "{:>9}  {:>14}  {:>14}  {:>11.1}%  {:>14}",
            n,
            kib(mem_live),
            kib(mmap_live),
            100.0 * mmap_live as f64 / mem_live as f64,
            kib(mmap_peak),
        );
    }
    println!(
        "\nin-mem live heap grows linearly with facts; mmap live heap stays flat\n\
         (bounded by the {MEMTABLE_LIMIT}-value memtable + run metadata — the facts\n\
         themselves live in mmap'd files). mmap peak also stays flat: the hybrid\n\
         run writer assembles small runs in memory but spills larger ones to\n\
         scratch files past its threshold, so the k-way compaction merge never\n\
         materializes a whole run on the heap."
    );
}
