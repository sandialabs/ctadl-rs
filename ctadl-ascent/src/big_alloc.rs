//! `CTADL_BIG_ALLOC_MB=<n>`: a probe for where the peak goes (P20 in things-to-improve.md).
//!
//! The process's peak runs 0.5-0.7 GB above what the fixpoint holds when it returns, on every app
//! measured, and the allocations that fail under a memory cap are single requests of about 100 MB.
//! This counts live heap bytes and, for every allocation or reallocation of at least `n` MB, prints
//! one stderr line with its size (and, for a reallocation, the old size), the live and peak heap
//! at that moment, and the frames that asked for it:
//!
//! ```text
//! [big-alloc] realloc 256.0 -> 512.0 MB  live 2811.4 MB  peak 2811.4 MB (new)
//! [big-alloc]   hashbrown::raw::RawTableInner::reserve_rehash
//! [big-alloc]   ctadl_ascent::index_engine::...
//! ```
//!
//! Off, the allocator costs one relaxed load per call. Backtraces need symbols: build with
//! `--profile profiling`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

/// The reporting threshold in bytes; 0 is off. Set once, by [`init`].
static THRESHOLD: AtomicUsize = AtomicUsize::new(0);
// Signed: a block allocated before `init` turned the probe on is not counted, but its free is.
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

thread_local! {
    /// Set while this thread is reporting, so the report's own allocations (the backtrace, the
    /// formatting) are counted but not reported.
    static REPORTING: Cell<bool> = const { Cell::new(false) };
}

/// Reads `CTADL_BIG_ALLOC_MB`. Call first thing in `main`.
pub fn init() {
    if let Some(mb) = std::env::var("CTADL_BIG_ALLOC_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&mb| mb > 0)
    {
        THRESHOLD.store(mb << 20, Ordering::Relaxed);
    }
}

pub struct Probe;

#[global_allocator]
static ALLOC: Probe = Probe;

fn mb(bytes: impl TryInto<i64>) -> f64 {
    bytes.try_into().unwrap_or(i64::MAX) as f64 / (1u64 << 20) as f64
}

/// Counts `size` new bytes (`freed` given back by a reallocation) and reports when it is big.
fn record(size: usize, freed: usize, old: Option<usize>) {
    let threshold = THRESHOLD.load(Ordering::Relaxed);
    if threshold == 0 {
        return;
    }
    let delta = size as i64 - freed as i64;
    let live = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    let prev_peak = PEAK.fetch_max(live, Ordering::Relaxed);
    if size < threshold || REPORTING.with(Cell::get) {
        return;
    }
    REPORTING.with(|r| r.set(true));
    let what = match old {
        Some(old) => format!("realloc {:.1} -> {:.1} MB", mb(old), mb(size)),
        None => format!("alloc {:.1} MB", mb(size)),
    };
    let mut out = format!(
        "[big-alloc] {what}  live {:.1} MB  peak {:.1} MB{}\n",
        mb(live),
        mb(live.max(prev_peak)),
        if live > prev_peak { " (new)" } else { "" }
    );
    for frame in frames() {
        out.push_str("[big-alloc]   ");
        out.push_str(&frame);
        out.push('\n');
    }
    eprint!("{out}");
    REPORTING.with(|r| r.set(false));
}

/// The frames worth reading: the first one outside the allocator and the standard library (the
/// container that grew), then the first few of this project's own.
fn frames() -> Vec<String> {
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    let symbols = bt.lines().filter_map(|l| {
        let (n, sym) = l.trim_start().split_once(": ")?;
        n.parse::<usize>().ok()?;
        Some(sym.trim().to_string())
    });
    let ours = |s: &str| s.starts_with("ctadl") || s.contains("::index_engine::");
    let internal = |s: &str| {
        s.contains("big_alloc")
            || s.starts_with("std::")
            || s.starts_with("alloc::")
            || s.starts_with("core::")
            || s.starts_with("__rust")
            || s.contains("backtrace")
            || s.starts_with("<unknown>")
    };
    let mut out = Vec::new();
    let mut have_container = false;
    for s in symbols {
        if internal(&s) {
            continue;
        }
        if ours(&s) {
            out.push(s);
            if out.len() >= 5 {
                break;
            }
        } else if !have_container && out.is_empty() {
            out.push(s);
            have_container = true;
        }
    }
    out
}

// SAFETY: every method forwards to `System` with the caller's unmodified pointer and layout, so
// the underlying allocator's contract is upheld verbatim; the counters are plain atomics that do
// not touch the allocation itself.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is the caller's, passed through unchanged.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            record(layout.size(), 0, None);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is the caller's, passed through unchanged.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            record(layout.size(), 0, None);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if THRESHOLD.load(Ordering::Relaxed) != 0 {
            LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: `ptr`/`layout` are the caller's matched pair, passed through unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`/`layout`/`new_size` are the caller's, passed through unchanged.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            record(new_size, layout.size(), Some(layout.size()));
        }
        p
    }
}
