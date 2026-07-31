//! A flat, CSR-style BYODS data structure for the `locals` relation.
//!
//! `locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is the dominant memory
//! consumer of the index phase. As a normal Ascent relation it is stored ~6× over: the
//! physical `Vec`, plus the indices `none`, `0_1`, `0_1_2`, the full existence index
//! `0_1_2_3_4`, and the inverse `0_3_4`. Every index stores its value columns *inline*, so
//! the full 5-column tuple is replicated many times.
//!
//! This module replaces all of that with a single shared store (the Ascent BYODS
//! "ind_common"). Every logical index becomes a lightweight *view* over that one store.
//!
//! ## Why flat, not a nested trie
//!
//! The earlier design stored `(F,V) -> P -> {(M,Fp)}` as `HashMap<(F,V), HashMap<P,
//! HashSet<(M,Fp)>>>`. Measured on a representative workload the groups are *tiny* — ~5
//! leaves per `(F,V)` group spread over ~2 distinct `P` — so the two inner hash levels are
//! nearly empty: each pays hashbrown's minimum **4**-bucket allocation, plus control bytes, to
//! hold ~2 elements. That structural slack, not the payload, held most of the store's bytes
//! (36k inner maps ≈ 53%, 70k leaf sets ≈ 38%, only ~13% real data + outer map). It does not
//! all come back, though: the flat form pays `P` inline on every leaf where the nested form
//! shared one `P` per leaf set, so measured against a faithful rebuild of the nested design
//! over identical data the flat design is 1.1–2.3× smaller, not ~10×
//! (`locals-trie-benchmark.md` §5).
//!
//! We collapse both inner levels into one **[`HybridSet`] of `(P,M,Fp)` per `(F,V)` group**:
//!   - one heap allocation per group instead of `1 + (#distinct P)` tiny hash tables;
//!   - leaves packed 24 B each with no per-element control bytes (the small representation's
//!     whole occupancy map is a single `u64` word beside the slots);
//!   - the group itself is two words — it is one structure that changes regime with its size,
//!     not an enum over two representations — so the outer map entry is 32 B, not 40;
//!   - existence is a probe, not a scan, at every group size.
//!
//! ## Large groups
//!
//! One representation cannot serve both ends of the group-size distribution. The *rare*
//! dense-aliasing function's `(F,V)` group grows to tens of thousands of leaves, and any
//! representation whose delta->total merge re-copies the whole accumulated group makes
//! building a group of size `G` cost O(G^2) — the dominant cost on such inputs (profiled at
//! ~55% of index time; see memory-investigation.md §7/§8). So a group that grows past
//! [`SMALL_THRESHOLD`] switches to Swiss probing (this crate's own table, see
//! [`super::hybrid_set::swiss`]), where the merge is O(delta) (insert only the new leaves).
//! Below the threshold it probes linearly over an allocation that is its element slots plus one
//! occupancy word. The switch is a change of regime *within* one structure, not a change of
//! type — see [`super::hybrid_set`] for why that is the right small representation, why it is not
//! an enum, and how the transition is made cheap.
//!
//! The forward map `(F,V) -> [ (P,M,Fp) ]` serves `none`, `0_1`, `0_1_2`, existence, and
//! iteration. The `0_3_4` view is *derived* by scanning (as before) rather than
//! materializing a full inverse; a small side-index `fidx: F -> {V}` narrows each `0_3_4`
//! probe to the flow-variables of the probed function.
//!
//! Correctness note (differs from `ascent_byods_rels::eqrel`): eqrel tolerates Ascent
//! merging the shared store twice per iteration (once via the ind_common, once via the
//! full-index write target) because union-find merge is idempotent. `locals` is a plain
//! relation, so a double merge would corrupt semi-naive evaluation. We therefore make the
//! *only* real merge live on the ind_common ([`LocalsIndCommon`]); every index write target
//! ([`FullWrite`], [`NoopWrite`]) has a no-op `RelIndexMerge`.

use std::hash::{BuildHasherDefault, Hash};
use std::marker::PhantomData;
use std::ops::Index;
use std::rc::Rc;

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use rustc_hash::FxHasher;

use super::hb_bytes;
use super::hybrid_set::{HybridSet, SMALL_THRESHOLD};

// The store keys are trusted, program-derived ids, so key on the fast,
// deterministic `FxHasher` rather than the std collections' DoS-resistant
// SipHash.
type Map<K, V> = hashbrown::HashMap<K, V, BuildHasherDefault<FxHasher>>;
type Set<T> = hashbrown::HashSet<T, BuildHasherDefault<FxHasher>>;

// ---------------------------------------------------------------------------
// A single `(F,V)` group's leaves.
// ---------------------------------------------------------------------------

/// The leaves of one `(F,V)` group. A [`HybridSet`]: a two-word structure that probes linearly
/// over its bare element slots while the group holds at most [`SMALL_THRESHOLD`] leaves — which is
/// the overwhelmingly common case, since 67–100 % of the groups in a measured store hold exactly
/// *one* leaf (`locals-trie-benchmark.md` §6) — and switches to Swiss probing above that, so the
/// per-iteration delta->total merge stays O(delta) instead of re-copying the whole accumulated
/// group each round. Being two words rather than three is worth 8 B on *every* entry of the
/// forward map, promoted or not.
type Group<P, M, Fp> = HybridSet<(P, M, Fp)>;

// ---------------------------------------------------------------------------
// Physical `rel!` storage. Stores no tuples (all data lives in the shared
// ind_common), but tracks the row count so `prog.locals.len()` — the only
// post-run consumer of the physical relation — still reports the true size.
// `push` is invoked exactly once per newly-inserted row by the generated code.
// ---------------------------------------------------------------------------
pub struct CountingVec<T> {
    len: usize,
    _p: PhantomData<T>,
}
impl<T> Default for CountingVec<T> {
    fn default() -> Self {
        Self {
            len: 0,
            _p: PhantomData,
        }
    }
}
impl<T> CountingVec<T> {
    #[inline(always)]
    pub fn push(&mut self, _v: T) {
        self.len += 1;
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline(always)]
    pub fn iter(&self) -> std::iter::Empty<&T> {
        std::iter::empty()
    }
}
impl<T> Index<usize> for CountingVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("locals_trie::CountingVec stores no tuples")
    }
}

// ---------------------------------------------------------------------------
// Clone-able boxed iterator (a local copy of byods' private IteratorFromDyn).
// ---------------------------------------------------------------------------
pub struct DynIter<'a, T> {
    iter: Box<dyn Iterator<Item = T> + 'a>,
    producer: Rc<dyn Fn() -> Box<dyn Iterator<Item = T> + 'a> + 'a>,
}
impl<'a, T> DynIter<'a, T> {
    pub fn new<F, I>(producer: F) -> Self
    where
        F: Fn() -> I + 'a,
        I: Iterator<Item = T> + 'a,
    {
        let producer = Rc::new(move || Box::new(producer()) as Box<dyn Iterator<Item = T> + 'a>);
        let iter = producer();
        Self { iter, producer }
    }
}
impl<T> Iterator for DynIter<'_, T> {
    type Item = T;
    #[inline(always)]
    fn next(&mut self) -> Option<T> {
        self.iter.next()
    }
}
impl<T> Clone for DynIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            iter: (self.producer)(),
            producer: self.producer.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The shared store (Ascent's `ind_common`).
// ---------------------------------------------------------------------------
pub struct LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    /// forward: (F,V) -> sorted `Vec` of `(P, M, Fp)` (ascending, deduplicated). Serves
    /// none / 0_1 / 0_1_2 / existence, and the `0_3_4` view by *scanning*. The vec is kept
    /// sorted so existence is a `binary_search` and `0_1_2` is a `partition_point` range over
    /// the contiguous `P`-run. One heap allocation per group replaces the old nested
    /// `HashMap<P, HashSet<(M,Fp)>>` (mostly hashbrown slack on tiny groups).
    fwd: Map<(F, V), Group<P, M, Fp>>,
    /// side-index: F -> set of V present for that function. Lets a `0_3_4` probe restrict its
    /// scan to the flow-variables of the probed function instead of walking every `(F,V)` group
    /// in `fwd`. Maintained in lockstep with `fwd`'s outer keys (exactly one V per `(F,V)`
    /// group), so it is cheap: one V (8 B + hashbrown slack) per group vs. the multi-GB store.
    fidx: Map<F, Set<V>>,
    len: usize,
}

impl<F, V, P, M, Fp> LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of distinct `(F, V)` groups — i.e. distinct variables reached by some formal,
    /// since `fwd`'s outer key is exactly the subject of a `locals` row. O(1): the trie
    /// already keys on the prefix a scan of the rows would have to rediscover.
    #[inline]
    pub fn num_reached_variables(&self) -> usize {
        self.fwd.len()
    }

    /// Phase-0 instrumentation: estimate the heap bytes held by the forward store vs. the
    /// `fidx` side-index, so we can see *which* structure dominates before optimizing (external
    /// `phys_footprint` can't attribute bytes to a sub-structure). Hash-table bytes include
    /// load-factor slack ([`hb_bytes`], exact for these element types) and group bytes come from
    /// [`HybridSet::heap_bytes`], so the whole report is an allocation-size accounting rather
    /// than a payload count. O(rows), one pass.
    pub fn heap_report(&self) -> HeapReport {
        let sz_outer = std::mem::size_of::<((F, V), Group<P, M, Fp>)>();
        let sz_leaf = std::mem::size_of::<(P, M, Fp)>();
        let sz_fidx_outer = std::mem::size_of::<(F, Set<V>)>();
        let sz_fidx_val = std::mem::size_of::<V>();

        let mut r = HeapReport {
            rows: self.len,
            fv_groups: self.fwd.len(),
            p_entries: 0,
            leaf_elems: 0,
            fwd_bytes: hb_bytes(self.fwd.capacity(), sz_outer),
            fidx_funcs: self.fidx.len(),
            fidx_vs: 0,
            fidx_bytes: hb_bytes(self.fidx.capacity(), sz_fidx_outer),
            elem_sizes: (sz_outer, 0, sz_leaf, sz_fidx_outer, sz_fidx_val),
            max_group: 0,
            large_groups: 0,
            group_hist: Vec::new(),
        };
        // Groups are unordered, so distinct `P` needs a scratch set rather than a run count.
        // One set, cleared per group, keeps the report to a single extra allocation.
        let mut ps: Set<&P> = Set::default();
        for group in self.fwd.values() {
            r.leaf_elems += group.len();
            r.max_group = r.max_group.max(group.len());
            // Coarse power-of-two histogram of group sizes: bucket i counts groups whose leaf
            // count is in `[2^i, 2^(i+1))`. This is what tells a benchmark (or a profile of a
            // real target) whether the store is the many-tiny-groups regime the small
            // representation is tuned for or the few-huge-groups regime that promotes.
            let bucket = usize::BITS as usize - 1 - group.len().max(1).leading_zeros() as usize;
            if r.group_hist.len() <= bucket {
                r.group_hist.resize(bucket + 1, 0);
            }
            r.group_hist[bucket] += 1;
            if group.is_large() {
                r.large_groups += 1;
            }
            r.fwd_bytes += group.heap_bytes();
            ps.clear();
            for (p, _, _) in group.iter() {
                ps.insert(p);
            }
            r.p_entries += ps.len();
        }
        for vs in self.fidx.values() {
            r.fidx_vs += vs.len();
            r.fidx_bytes += hb_bytes(vs.capacity(), sz_fidx_val);
        }
        r
    }

    #[inline]
    fn contains(&self, f: &F, v: &V, p: &P, m: &M, fp: &Fp) -> bool {
        // (P,M,Fp) are cheap to clone (8-byte handles + i16); avoids a borrow-key helper.
        self.fwd
            .get(&(f.clone(), v.clone()))
            .is_some_and(|group| group.contains(&(p.clone(), m.clone(), fp.clone())))
    }

    /// Insert a full tuple; returns true if newly added to *this* store.
    fn insert(&mut self, key: &(F, V, P, M, Fp)) -> bool {
        use hashbrown::hash_map::Entry;
        let (f, v, p, m, fp) = key;
        // Fetch the (F,V) group, recording V in the side-index the first time the group appears.
        let group = match self.fwd.entry((f.clone(), v.clone())) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                self.fidx.entry(f.clone()).or_default().insert(v.clone());
                e.insert(Group::new())
            }
        };
        if group.insert((p.clone(), m.clone(), fp.clone())) {
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Merge `from` into `self` (union). Used by the delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        use hashbrown::hash_map::Entry;
        for ((f, v), fromgroup) in from.fwd.drain() {
            match self.fwd.entry((f.clone(), v.clone())) {
                Entry::Occupied(e) => {
                    self.len += e.into_mut().merge(fromgroup);
                }
                Entry::Vacant(e) => {
                    self.fidx.entry(f).or_default().insert(v);
                    self.len += fromgroup.len();
                    e.insert(fromgroup);
                }
            }
        }
        from.len = 0;
        from.fidx.clear();
    }
}

/// Per-structure byte estimate for the `locals` store (see [`LocalsIndCommon::heap_report`]).
#[derive(Debug, Clone)]
pub struct HeapReport {
    /// Logical row count (== `locals` size).
    pub rows: usize,
    /// Number of distinct `(F,V)` groups (outer forward keys).
    pub fv_groups: usize,
    /// Number of distinct `(F,V,P)` entries across all groups.
    pub p_entries: usize,
    /// Number of `(P,M,Fp)` leaf elements across all groups (== rows).
    pub leaf_elems: usize,
    /// Estimated bytes for the whole forward store (outer map + per-group leaf vecs).
    pub fwd_bytes: usize,
    /// Number of distinct functions in the `fidx` side-index (outer keys).
    pub fidx_funcs: usize,
    /// Number of `V` entries across all `fidx` sets (== distinct `(F,V)` groups == `fv_groups`).
    pub fidx_vs: usize,
    /// Estimated bytes for the `fidx` side-index (outer map + per-function V sets).
    pub fidx_bytes: usize,
    /// Element sizes `(outer, _unused, leaf, fidx_outer, fidx_val)` for reference.
    pub elem_sizes: (usize, usize, usize, usize, usize),
    /// Largest single `(F,V)` group, in leaves. The knob that decides whether the `Small` `Vec`
    /// representation (and its O(group) insert / O(G^2) accumulate) is the right one.
    pub max_group: usize,
    /// How many groups exceeded [`SMALL_THRESHOLD`] and switched to Swiss probing.
    pub large_groups: usize,
    /// Power-of-two histogram of group sizes: `group_hist[i]` counts groups with
    /// `2^i <= leaves < 2^(i+1)`.
    pub group_hist: Vec<usize>,
}

impl std::fmt::Display for HeapReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        let total = self.fwd_bytes + self.fidx_bytes;
        let pct = |b: usize| {
            if total == 0 {
                0.0
            } else {
                100.0 * b as f64 / total as f64
            }
        };
        let (o, _i, l, fo, fv) = self.elem_sizes;
        // "0:12 1:3 5:1" == 12 groups of 1 leaf, 3 of 2-3, 1 of 32-63.
        let hist = self
            .group_hist
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(i, n)| format!("{i}:{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        write!(
            f,
            "locals store estimate: total {:.1} MB over {} rows ({:.0} B/row) | \
             fwd {:.1} MB ({:.0}%): {} (F,V) groups, {} (F,V,P) entries, {} leaves | \
             fidx {:.1} MB ({:.0}%): {} funcs, {} V entries | \
             groups: max {}, large {}, mean {:.2}, log2hist [{}] | \
             elem sizes o={} l={} fo={} fv={} B | group set threshold {}",
            mb(total),
            self.rows,
            if self.rows == 0 {
                0.0
            } else {
                total as f64 / self.rows as f64
            },
            mb(self.fwd_bytes),
            pct(self.fwd_bytes),
            self.fv_groups,
            self.p_entries,
            self.leaf_elems,
            mb(self.fidx_bytes),
            pct(self.fidx_bytes),
            self.fidx_funcs,
            self.fidx_vs,
            self.max_group,
            self.large_groups,
            if self.fv_groups == 0 {
                0.0
            } else {
                self.leaf_elems as f64 / self.fv_groups as f64
            },
            hist,
            o,
            l,
            fo,
            fv,
            SMALL_THRESHOLD,
        )
    }
}

impl<F, V, P, M, Fp> Default for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            fwd: Map::default(),
            fidx: Map::default(),
            len: 0,
        }
    }
}

impl<F, V, P, M, Fp> Clone for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        Self {
            fwd: self.fwd.clone(),
            fidx: self.fidx.clone(),
            len: self.len,
        }
    }
}

// The ONE real merge lives here (on the ind_common).
impl<F, V, P, M, Fp> RelIndexMerge for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.absorb(from);
    }
    // default merge_delta_to_total_new_to_delta: move delta->total, swap new<->delta.
}

// ---------------------------------------------------------------------------
// Write targets.
// ---------------------------------------------------------------------------

/// No-op write target for the partial (view) indices: data enters the store only through
/// the full index, and merging is done only by the ind_common.
pub struct NoopWrite<K, V>(PhantomData<(K, V)>);
impl<K, V> Default for NoopWrite<K, V> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<K, V> RelIndexWrite for NoopWrite<K, V> {
    type Key = K;
    type Value = V;
    #[inline(always)]
    fn index_insert(&mut self, _key: K, _value: V) {}
}
impl<K, V> RelIndexMerge for NoopWrite<K, V> {
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

/// Write target of the full existence index: performs the real inserts into the store, but
/// has a NO-OP merge (the real merge is the ind_common's, see module docs).
pub struct FullWrite<'a, F, V, P, M, Fp>(&'a mut LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;

impl<'a, F, V, P, M, Fp> RelFullIndexWrite for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P, M, Fp);
    type Value = ();
    #[inline]
    fn insert_if_not_present(&mut self, key: &Self::Key, _v: ()) -> bool {
        self.0.insert(key)
    }
}
impl<'a, F, V, P, M, Fp> RelIndexWrite for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P, M, Fp);
    type Value = ();
    #[inline]
    fn index_insert(&mut self, key: Self::Key, _value: ()) {
        self.0.insert(&key);
    }
}
impl<'a, F, V, P, M, Fp> RelIndexMerge for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

// ---------------------------------------------------------------------------
// Read views + their ToRelIndex markers.
//
// Each `To*` marker is a zero-sized field in the generated struct; `to_rel_index` borrows
// the shared store to produce a read view, `to_rel_index_write` produces a write target.
// ---------------------------------------------------------------------------

/// Defines a zero-sized `ToRelIndex` marker over the concrete `LocalsIndCommon` store.
/// `$wrty`/`$wrbody` give the write target (real for the full index, no-op for the views).
macro_rules! marker {
    ($to:ident, $view:ident, $wrty:ty, $rel:ident => $wrbody:expr) => {
        pub struct $to<F, V, P, M, Fp>(PhantomData<(F, V, P, M, Fp)>);
        impl<F, V, P, M, Fp> Default for $to<F, V, P, M, Fp> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<F, V, P, M, Fp> ToRelIndex<LocalsIndCommon<F, V, P, M, Fp>> for $to<F, V, P, M, Fp>
        where
            F: Clone + Eq + Hash,
            V: Clone + Eq + Hash,
            P: Clone + Eq + Hash,
            M: Clone + Eq + Hash,
            Fp: Clone + Eq + Hash,
        {
            type RelIndex<'a>
                = $view<'a, F, V, P, M, Fp>
            where
                Self: 'a,
                LocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a LocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndex<'a> {
                $view(rel)
            }
            type RelIndexWrite<'a>
                = $wrty
            where
                Self: 'a,
                LocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                $rel: &'a mut LocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndexWrite<'a> {
                $wrbody
            }
        }
    };
}

marker!(ToNone, ViewNone, NoopWrite<(), (F, V, P, M, Fp)>, _rel => NoopWrite::default());
marker!(To01, View01, NoopWrite<(F, V), (P, M, Fp)>, _rel => NoopWrite::default());
marker!(To012, View012, NoopWrite<(F, V, P), (M, Fp)>, _rel => NoopWrite::default());
marker!(To034, View034, NoopWrite<(F, M, Fp), (V, P)>, _rel => NoopWrite::default());
marker!(ToFull, ViewFull, FullWrite<'a, F, V, P, M, Fp>, rel => FullWrite(rel));

pub struct ViewNone<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View01<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View012<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View034<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct ViewFull<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;

// ---- none: () -> (F,V,P,M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for ViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
        let c = self.0;
        Some(DynIter::new(move || {
            c.fwd
                .iter()
                .flat_map(|((f, v), group)| group.iter().map(move |(p, m, fp)| (f, v, p, m, fp)))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        1
    }
    #[inline]
    fn is_empty(&'a self) -> bool {
        self.0.is_empty()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for ViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = std::iter::Once<((), Self::ValueIteratorType)>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        std::iter::once(((), RelIndexRead::index_get(self, &()).unwrap()))
    }
}

// ---- 0_1: (F,V) -> (P,M,Fp) -----------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V);
    type Value = (&'a P, &'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, V)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.get(key)?;
        Some(DynIter::new(move || {
            group.iter().map(|(p, m, fp)| (p, m, fp))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V);
    type Value = (&'a P, &'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.iter().map(|((f, v), group)| {
            let it = DynIter::new(move || group.iter().map(|(p, m, fp)| (p, m, fp)));
            ((f, v), it)
        }))
    }
}

// ---- 0_1_2: (F,V,P) -> (M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P);
    type Value = (&'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, V, P)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.get(&(key.0.clone(), key.1.clone()))?;
        let p = key.2.clone();
        // A group is a set, not a sorted run, so the leaves carrying this `P` are scattered and
        // this view filters instead of slicing a range. The one up-front scan keeps `None` — and
        // with it the caller's whole join — off the table for a `P` the group does not hold;
        // without it a miss would hand the planner a `Some` it has to drive to exhaustion. The
        // scan is O(group), it short-circuits on the first match, and the median group is a
        // single leaf (`locals-trie-benchmark.md` §6).
        if !group.iter().any(|(pp, _, _)| *pp == p) {
            return None;
        }
        Some(DynIter::new(move || {
            let p = p.clone();
            group
                .iter()
                .filter_map(move |(pp, m, fp)| (*pp == p).then_some((m, fp)))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V, &'a P);
    type Value = (&'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        // Emit one key per distinct (F,V,P), each with all its (M,Fp) leaves. Groups are
        // unordered, so the leaves of one `P` have to be bucketed rather than sliced off as a
        // run. Not on a hot path: `0_1_2` is point-probed by the rules, and only a planner that
        // chose it as a join driver would land here.
        Box::new(self.0.fwd.iter().flat_map(|((f, v), group)| {
            let mut byp: Map<&'a P, Vec<(&'a M, &'a Fp)>> = Map::default();
            for (p, m, fp) in group.iter() {
                byp.entry(p).or_default().push((m, fp));
            }
            byp.into_iter().map(move |(p, mfs)| {
                let it = DynIter::new(move || mfs.clone().into_iter());
                ((f, v, p), it)
            })
        }))
    }
}

// ---- 0_3_4: (F,M,Fp) -> (V,P) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, M, Fp);
    type Value = (&'a V, &'a P);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, M, Fp)) -> Option<Self::IteratorType> {
        // Derived (no materialized inverse): for the probed function `f`, visit only its
        // flow-variables via the `fidx` side-index, then yield every `(v, p)` whose group vec
        // contains a leaf with this `(m, fp)`. This touches one function's `(F,V)` groups
        // instead of scanning all of `fwd`. Cold path (rule 2.2, ~tens of probes). Returns
        // `Some` of a possibly-empty iterator; an empty result is join-equivalent to `None`.
        let c = self.0;
        let (f, m, fp) = key.clone();
        Some(DynIter::new(move || {
            let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
            // `fidx[f]` and `fwd` are maintained in lockstep, so every V here has an `fwd` group.
            c.fidx.get(&f).into_iter().flat_map(move |vs| {
                let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
                vs.iter().flat_map(move |v| {
                    let (m, fp) = (m.clone(), fp.clone());
                    c.fwd
                        .get(&(f.clone(), v.clone()))
                        .into_iter()
                        .flat_map(move |group| {
                            let (m, fp) = (m.clone(), fp.clone());
                            group.iter().filter_map(move |(p, mm, ffp)| {
                                if *mm == m && *ffp == fp {
                                    Some((v, p))
                                } else {
                                    None
                                }
                            })
                        })
                })
            })
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        // No materialized inverse to size. `0_3_4` is only ever probed (never a join driver),
        // so a large estimate is safe — it just discourages the planner from choosing it as a
        // driver, which is exactly what we want. Use the row count as a conservative upper bound.
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a M, &'a Fp);
    type Value = (&'a V, &'a P);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    fn iter_all(&'a self) -> Self::AllIteratorType {
        // Not on any hot path: `0_3_4` is only ever point-probed (rule 2.2), never iterated as a
        // driver. Provide a correct fallback by transiently inverting `fwd` on demand. If this
        // ever shows up in a profile, it means some rule started iterating `0_3_4` and a
        // materialized inverse should be reconsidered.
        let mut groups: Map<(&'a F, &'a M, &'a Fp), Vec<(&'a V, &'a P)>> = Map::default();
        for ((f, v), group) in self.0.fwd.iter() {
            for (p, m, fp) in group.iter() {
                groups.entry((f, m, fp)).or_default().push((v, p));
            }
        }
        Box::new(groups.into_iter().map(|(key, vps)| {
            let it = DynIter::new(move || vps.clone().into_iter());
            (key, it)
        }))
    }
}

// ---- full 0_1_2_3_4: existence ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelFullIndexRead<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P, M, Fp);
    #[inline]
    fn contains_key(&'a self, key: &Self::Key) -> bool {
        self.0.contains(&key.0, &key.1, &key.2, &key.3, &key.4)
    }
}
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P, M, Fp);
    type Value = &'a ();
    type IteratorType = std::iter::Once<&'a ()>;
    #[inline]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        if self.0.contains(&key.0, &key.1, &key.2, &key.3, &key.4) {
            Some(std::iter::once(&()))
        } else {
            None
        }
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type Value = &'a ();
    type ValueIteratorType = std::iter::Once<&'a ()>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.iter().flat_map(|((f, v), group)| {
            group
                .iter()
                .map(move |(p, m, fp)| ((f, v, p, m, fp), std::iter::once(&())))
        }))
    }
}

// ---------------------------------------------------------------------------
// The BYODS provider macros. Codegen calls these as
//   locals_trie::rel!(Name, (cols), [inds], ser, (args))
//   locals_trie::rel_ind_common!(Name, (cols), [inds], ser, (args))
//   locals_trie::rel_full_ind!(Name, (cols), [inds], ser, (args), Key, Val)
//   locals_trie::rel_ind!(Name, (cols), [inds], ser, (args), [subset], Key, Val)
//   locals_trie::rel_codegen!(Name, (cols), [inds], ser, (args))
// ---------------------------------------------------------------------------

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_codegen {
    ($($tt:tt)*) => {};
}
pub use locals_trie_rel_codegen as rel_codegen;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::locals_trie::CountingVec<($f, $v, $p, $m, $fp)>
    };
}
pub use locals_trie_rel as rel;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_ind_common {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::locals_trie::LocalsIndCommon<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_ind_common as rel_ind_common;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_full_ind {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::ToFull<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_full_ind as rel_full_ind;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_ind {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::ToNone<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 1], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To01<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 1, 2], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To012<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 3, 4], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To034<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_ind as rel_ind;

#[cfg(test)]
mod tests {
    use ascent::internal::{RelFullIndexRead, RelIndexRead, RelIndexReadAll};

    use super::*;

    type Store = LocalsIndCommon<u32, u64, u64, i16, u64>;

    /// Every read view over a store built from `rows`, checked against the rows themselves.
    /// The views are the part of this module that changed when groups stopped being sorted:
    /// `0_1_2` filters where it used to slice a run, and `iter_all` buckets where it used to
    /// split one. `groups` is exercised on both sides of `SMALL_THRESHOLD`.
    fn check_views(rows: &[(u32, u64, u64, i16, u64)]) {
        let mut store = Store::default();
        for row in rows {
            store.insert(row);
        }
        let mut want: Vec<_> = rows.to_vec();
        want.sort_unstable();
        want.dedup();
        assert_eq!(store.len(), want.len(), "row count");

        // Existence, both present and absent.
        for &(f, v, p, m, fp) in &want {
            assert!(
                store.contains(&f, &v, &p, &m, &fp),
                "missing {:?}",
                (f, v, p, m, fp)
            );
        }
        assert!(!store.contains(&999, &0, &0, &0, &0));
        for &(f, v, p, m, fp) in &want {
            assert!(!store.contains(&f, &v, &p, &m, &(fp + 1_000_000)));
        }

        // none: () -> every row.
        let view = ViewNone(&store);
        let mut got: Vec<_> = RelIndexRead::index_get(&view, &())
            .unwrap()
            .map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`none` view");

        // 0_1: (F,V) -> (P,M,Fp).
        let view = View01(&store);
        for &(f, v, ..) in &want {
            let mut got: Vec<_> = view
                .index_get(&(f, v))
                .unwrap()
                .map(|(p, m, fp)| (f, v, *p, *m, *fp))
                .collect();
            got.sort_unstable();
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.1 == v)
                .collect();
            assert_eq!(got, expect, "`0_1` view at {:?}", (f, v));
        }
        assert!(view.index_get(&(999, 0)).is_none());

        // 0_1_2: (F,V,P) -> (M,Fp). A miss must be `None`, not an empty iterator.
        let view = View012(&store);
        for &(f, v, p, ..) in &want {
            let mut got: Vec<_> = view
                .index_get(&(f, v, p))
                .unwrap()
                .map(|(m, fp)| (f, v, p, *m, *fp))
                .collect();
            got.sort_unstable();
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.1 == v && r.2 == p)
                .collect();
            assert_eq!(got, expect, "`0_1_2` view at {:?}", (f, v, p));
            assert!(
                view.index_get(&(f, v, p + 1_000_000)).is_none(),
                "`0_1_2` must report a missing P as None"
            );
        }
        let mut got: Vec<_> = view
            .iter_all()
            .flat_map(|((f, v, p), vals)| vals.map(move |(m, fp)| (*f, *v, *p, *m, *fp)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_1_2` iter_all");

        // 0_3_4: (F,M,Fp) -> (V,P), derived by scanning via `fidx`.
        let view = View034(&store);
        for &(f, _, _, m, fp) in &want {
            let mut got: Vec<_> = view
                .index_get(&(f, m, fp))
                .unwrap()
                .map(|(v, p)| (f, *v, *p, m, fp))
                .collect();
            got.sort_unstable();
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.3 == m && r.4 == fp)
                .collect();
            assert_eq!(got, expect, "`0_3_4` view at {:?}", (f, m, fp));
        }
        let mut got: Vec<_> = view
            .iter_all()
            .flat_map(|((f, m, fp), vals)| vals.map(move |(v, p)| (*f, *v, *p, *m, *fp)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_3_4` iter_all");

        // full existence index.
        let view = ViewFull(&store);
        for &(f, v, p, m, fp) in &want {
            assert!(view.contains_key(&(f, v, p, m, fp)));
        }
        let mut got: Vec<_> = RelIndexReadAll::iter_all(&view)
            .map(|((f, v, p, m, fp), _)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "full-index iter_all");
    }

    /// `group` leaves per `(F,V)`, spread over `paths` distinct `P`, over two functions.
    fn rows(groups: usize, group: usize, paths: usize) -> Vec<(u32, u64, u64, i16, u64)> {
        (0..groups)
            .flat_map(|g| {
                (0..group).map(move |i| {
                    (
                        (g % 2) as u32,
                        g as u64,
                        (i % paths) as u64,
                        (i / paths) as i16,
                        i as u64,
                    )
                })
            })
            .collect()
    }

    #[test]
    fn views_agree_with_their_rows() {
        check_views(&[]);
        check_views(&rows(1, 1, 1));
        check_views(&rows(3, 5, 2));
        // Duplicate rows must be idempotent.
        let mut dup = rows(2, 4, 2);
        dup.extend_from_slice(&dup.clone());
        check_views(&dup);
        // Straddle the small -> hashbrown transition inside a group.
        for group in [
            SMALL_THRESHOLD - 1,
            SMALL_THRESHOLD,
            SMALL_THRESHOLD + 1,
            SMALL_THRESHOLD * 3,
        ] {
            check_views(&rows(2, group, 3));
        }
    }

    /// `absorb` is the delta->total merge, and it has to keep `len`, `fidx` and the groups
    /// consistent whichever representation the two sides are in.
    #[test]
    fn absorb_unions_the_stores() {
        for (a_group, b_group) in [
            (2usize, 3usize),
            (SMALL_THRESHOLD, 2),
            (2, SMALL_THRESHOLD + 1),
            (SMALL_THRESHOLD + 1, SMALL_THRESHOLD + 1),
        ] {
            let a_rows = rows(4, a_group, 2);
            // Overlapping keys with different leaves, plus one key `a` does not have.
            let b_rows: Vec<_> = rows(6, b_group, 2)
                .into_iter()
                .map(|(f, v, p, m, fp)| (f, v, p, m, fp + 7))
                .collect();

            let mut total = Store::default();
            for row in &a_rows {
                total.insert(row);
            }
            let mut delta = Store::default();
            for row in &b_rows {
                delta.insert(row);
            }
            total.absorb(&mut delta);

            assert_eq!(delta.len(), 0, "absorbed delta must be empty");
            assert_eq!(delta.fidx.len(), 0, "absorbed delta must drop its fidx");

            let mut want: Vec<_> = a_rows.iter().chain(b_rows.iter()).copied().collect();
            want.sort_unstable();
            want.dedup();
            assert_eq!(total.len(), want.len(), "union row count");
            for &(f, v, p, m, fp) in &want {
                assert!(total.contains(&f, &v, &p, &m, &fp));
            }
            // `fidx` must list exactly the V of every group, so `0_3_4` can still find them.
            for ((f, v), _) in total.fwd.iter() {
                assert!(
                    total.fidx[f].contains(v),
                    "fidx missing {v} of function {f}"
                );
            }
            assert_eq!(
                total.fidx.values().map(|vs| vs.len()).sum::<usize>(),
                total.fwd.len(),
                "fidx must hold exactly one V per group"
            );
            let report = total.heap_report();
            assert_eq!(report.rows, total.len());
            assert_eq!(report.leaf_elems, total.len(), "heap_report leaf count");
        }
    }
}
