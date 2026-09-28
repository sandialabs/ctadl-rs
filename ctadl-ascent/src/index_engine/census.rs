//! A byte census of the serial index program: every relation's physical storage and every one
//! of its Ascent indices, sized from capacities, so a run (or a run stopped by
//! `CTADL_INDEX_TIMEOUT_SECS`) can say where its memory went, index by index.
//!
//! Stock Ascent 0.8 storage is priced from its concrete types (see `ascent::internal`):
//!   - a relation's rows: `Vec<tuple>`;
//!   - a full (existence) index `R_indices_0_1_..`: `RelFullIndexType<tuple, ()>`, a hashbrown
//!     set of whole tuples;
//!   - a probe index `R_indices_<key cols>`: `ToRelIndexType<K, V>`, a
//!     `HashMap<K, Vec<V>>` whose values are the *remaining columns inline*, one `Vec` per key
//!     allocated at capacity 4 at least;
//!   - a lattice index: `LatticeIndexType<K, usize>`, a `HashMap<K, HashSet<row>>`, one hash
//!     set per key.
//!
//! The BYODS stores (`locals_trie`, `assign_like_trie`) are priced by their own reports; their
//! views hold nothing and their physical relations hold no tuples.
//!
//! Every per-key allocation is rounded up to the 16-byte malloc quantum; hash tables are priced
//! by [`hb_bytes`], which includes load-factor slack. The census prices allocation sizes, so it
//! is directly comparable with the process footprint.

use std::collections::{HashMap as StdMap, HashSet as StdSet};
use std::fmt::Write as _;
use std::hash::{BuildHasher, Hash};

use crate::index_engine::assign_like_trie::AssignTrie;
use crate::index_engine::locals_trie::{LocalsIndCommon, hb_bytes};

const QUANTUM: usize = 16;

fn alloc(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else {
        bytes.next_multiple_of(QUANTUM)
    }
}

/// What one physical structure holds: outer keys (0 for a plain `Vec`), stored entries (rows,
/// or values across all keys), and heap bytes.
#[derive(Default, Clone, Copy)]
pub struct Stat {
    pub keys: usize,
    pub entries: usize,
    pub bytes: usize,
}

pub trait HeapBytes {
    fn heap_stat(&self) -> Stat;
}

impl<T> HeapBytes for Vec<T> {
    fn heap_stat(&self) -> Stat {
        Stat {
            keys: 0,
            entries: self.len(),
            bytes: self.capacity() * std::mem::size_of::<T>(),
        }
    }
}

impl<K, V> HeapBytes for ascent::rel::ToRelIndexType<K, V> {
    fn heap_stat(&self) -> Stat {
        let m = &self.0;
        let mut s = Stat {
            keys: m.len(),
            entries: 0,
            bytes: hb_bytes(m.capacity(), std::mem::size_of::<(K, Vec<V>)>()),
        };
        for v in m.values() {
            s.entries += v.len();
            s.bytes += alloc(v.capacity() * std::mem::size_of::<V>());
        }
        s
    }
}

impl<K, V> HeapBytes for ascent::internal::RelFullIndexType<K, V> {
    fn heap_stat(&self) -> Stat {
        Stat {
            keys: self.len(),
            entries: self.len(),
            bytes: hb_bytes(self.capacity(), std::mem::size_of::<(K, V)>()),
        }
    }
}

impl<K, V, S1, S2> HeapBytes for StdMap<K, StdSet<V, S2>, S1>
where
    S1: BuildHasher,
    S2: BuildHasher,
    K: Eq + Hash,
{
    fn heap_stat(&self) -> Stat {
        let mut s = Stat {
            keys: self.len(),
            entries: 0,
            bytes: hb_bytes(self.capacity(), std::mem::size_of::<(K, StdSet<V, S2>)>()),
        };
        for set in self.values() {
            s.entries += set.len();
            s.bytes += alloc(hb_bytes(set.capacity(), std::mem::size_of::<V>()));
        }
        s
    }
}

impl<F, V, P, M, Fp> HeapBytes for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn heap_stat(&self) -> Stat {
        let r = self.heap_report();
        Stat {
            keys: r.fv_groups,
            entries: r.rows,
            bytes: r.fwd_bytes + r.fidx_bytes,
        }
    }
}

impl<F, Vd, Pd, Vs, Ps> HeapBytes for AssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn heap_stat(&self) -> Stat {
        let (keys, entries, bytes) = self.heap_totals();
        Stat {
            keys,
            entries,
            bytes,
        }
    }
}

/// One line per physical structure: `(relation, structure, rows of the relation, stat)`.
#[derive(Default)]
pub struct Census {
    lines: Vec<(&'static str, &'static str, usize, Stat)>,
}

impl Census {
    pub fn add(&mut self, rel: &'static str, what: &'static str, rows: usize, s: Stat) {
        self.lines.push((rel, what, rows, s));
    }

    pub fn total_bytes(&self) -> usize {
        self.lines.iter().map(|l| l.3.bytes).sum()
    }

    /// Tab-separated, one structure per line, with a header.
    pub fn tsv(&self) -> String {
        let mut out = String::from("relation\tstructure\trel_rows\tkeys\tentries\tbytes\n");
        for (rel, what, rows, s) in &self.lines {
            let _ = writeln!(
                out,
                "{rel}\t{what}\t{rows}\t{}\t{}\t{}",
                s.keys, s.entries, s.bytes
            );
        }
        out
    }

    /// Relations ranked by bytes, each with its structures ranked under it.
    pub fn ranked(&self, footprint_bytes: usize) -> String {
        let gb = |b: usize| b as f64 / (1u64 << 30) as f64;
        let mut per_rel: Vec<(&str, usize, usize)> = Vec::new();
        for (rel, _, rows, s) in &self.lines {
            match per_rel.iter_mut().find(|(r, _, _)| r == rel) {
                Some(e) => e.2 += s.bytes,
                None => per_rel.push((rel, *rows, s.bytes)),
            }
        }
        per_rel.sort_by(|a, b| b.2.cmp(&a.2));
        let total = self.total_bytes();
        let mut out = format!(
            "index census: {:.2} GiB counted of {:.2} GiB footprint ({:.0}%)\n",
            gb(total),
            gb(footprint_bytes),
            100.0 * total as f64 / footprint_bytes.max(1) as f64
        );
        for (rel, rows, bytes) in per_rel.iter().filter(|r| r.2 > 0) {
            let _ = writeln!(
                out,
                "  {rel:<26} {rows:>13} rows {:>8.3} GiB {:>5.1}% {:>7.0} B/row",
                gb(*bytes),
                100.0 * *bytes as f64 / total.max(1) as f64,
                *bytes as f64 / (*rows).max(1) as f64
            );
            let mut parts: Vec<_> = self.lines.iter().filter(|l| l.0 == *rel && l.3.bytes > 0).collect();
            parts.sort_by(|a, b| b.3.bytes.cmp(&a.3.bytes));
            for (_, what, _, s) in parts {
                let _ = writeln!(
                    out,
                    "      {what:<40} keys {:>12} entries {:>12} {:>8.3} GiB",
                    s.keys,
                    s.entries,
                    gb(s.bytes)
                );
            }
        }
        out
    }
}
