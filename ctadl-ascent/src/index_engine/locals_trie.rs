//! A prefix-sharing (trie-like) BYODS data structure for the `locals` relation.
//!
//! `locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is the dominant memory
//! consumer of the index phase. As a normal Ascent relation it is stored ~6× over: the
//! physical `Vec`, plus the indices `none`, `0_1`, `0_1_2`, the full existence index
//! `0_1_2_3_4`, and the inverse `0_3_4`. Every index stores its value columns *inline*, so
//! the full 5-column tuple is replicated many times.
//!
//! This module replaces all of that with a single shared store (the Ascent BYODS
//! "ind_common"). Every logical index becomes a lightweight *view* over that one store:
//!   - a forward map `(F,V) -> P -> {(M,Fp)}` serves `none`, `0_1`, `0_1_2`, existence,
//!     and iteration — the `(F,V)` and `P` prefixes are stored once and shared.
//!   - the `0_3_4` view is *derived* by scanning the forward store rather than materializing
//!     a full inverse `(F,M,Fp) -> [(V,P)]` copy (which measured ~53% of the store). A small
//!     side-index `fidx: F -> {V}` narrows each `0_3_4` probe to the flow-variables of the
//!     probed function, so the scan touches one function's groups instead of all of `fwd`.
//!
//! Correctness note (differs from `ascent_byods_rels::eqrel`): eqrel tolerates Ascent
//! merging the shared store twice per iteration (once via the ind_common, once via the
//! full-index write target) because union-find merge is idempotent. `locals` is a plain
//! relation, so a double merge would corrupt semi-naive evaluation. We therefore make the
//! *only* real merge live on the ind_common ([`LocalsIndCommon`]); every index write target
//! ([`FullWrite`], [`NoopWrite`]) has a no-op `RelIndexMerge`.

use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::Index;
use std::rc::Rc;

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll, RelIndexWrite, ToRelIndex,
};

type Map<K, V> = hashbrown::HashMap<K, V>;
type Set<T> = hashbrown::HashSet<T>;

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
        Self { len: 0, _p: PhantomData }
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
        Self { iter: (self.producer)(), producer: self.producer.clone() }
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
    /// forward: (F,V) -> P -> set of (M, Fp). Serves none / 0_1 / 0_1_2 / existence, and
    /// the `0_3_4` view by *scanning* rather than a materialized inverse: `0_3_4` is probed at
    /// exactly one cold site (rule 2.2, mod.rs), driven by the tiny `resolvent` relation
    /// (measured 2-38 tuples), so deriving those few probes by scanning `fwd` trades a ~53%
    /// store-size inverse copy for a handful of scans. See `View034`.
    fwd: Map<(F, V), Map<P, Set<(M, Fp)>>>,
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

    #[inline]
    fn contains(&self, f: &F, v: &V, p: &P, m: &M, fp: &Fp) -> bool {
        // (M,Fp) is cheap to clone (i16 + 8-byte handle); avoids a borrow-key helper.
        self.fwd
            .get(&(f.clone(), v.clone()))
            .and_then(|pm| pm.get(p))
            .is_some_and(|s| s.contains(&(m.clone(), fp.clone())))
    }

    /// Insert a full tuple; returns true if newly added to *this* store.
    fn insert(&mut self, key: &(F, V, P, M, Fp)) -> bool {
        use hashbrown::hash_map::Entry;
        let (f, v, p, m, fp) = key;
        // Fetch the (F,V) group, recording V in the side-index the first time the group appears.
        let pm = match self.fwd.entry((f.clone(), v.clone())) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                self.fidx.entry(f.clone()).or_default().insert(v.clone());
                e.insert(Map::default())
            }
        };
        let leaves = pm.entry(p.clone()).or_default();
        if leaves.insert((m.clone(), fp.clone())) {
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Phase-0 instrumentation: estimate the heap bytes held by the forward trie vs. the
    /// `fidx` side-index, so we can see *which* structure dominates before optimizing (external
    /// `phys_footprint` can't attribute bytes to a sub-structure). Estimates are
    /// allocation-size approximations that include hashbrown load-factor slack; they are for
    /// *relative* comparison (fwd vs fidx), not exact accounting. O(groups+leaves), one pass.
    pub fn heap_report(&self) -> HeapReport {
        // hashbrown allocates `buckets` slots (a power of two sized so that 7/8*buckets >=
        // capacity), each `size_of::<T>()` bytes, plus one control byte per bucket (+ a
        // group-width mirror). Approximate that from the map's reported `capacity()`.
        fn hb_bytes(capacity: usize, elem: usize) -> usize {
            if capacity == 0 {
                return 0;
            }
            let buckets = ((capacity * 8 + 6) / 7).next_power_of_two().max(8);
            buckets * (elem + 1) + 16
        }

        let sz_outer = std::mem::size_of::<((F, V), Map<P, Set<(M, Fp)>>)>();
        let sz_inner = std::mem::size_of::<(P, Set<(M, Fp)>)>();
        let sz_leaf = std::mem::size_of::<(M, Fp)>();
        // inverse map removed (option-1): 0_3_4 is a derived scan over `fwd`. The only auxiliary
        // structure now is the `fidx` side-index (F -> {V}) that narrows the scan (option-2).
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
            elem_sizes: (sz_outer, sz_inner, sz_leaf, sz_fidx_outer, sz_fidx_val),
        };
        for pm in self.fwd.values() {
            r.p_entries += pm.len();
            r.fwd_bytes += hb_bytes(pm.capacity(), sz_inner);
            for set in pm.values() {
                r.leaf_elems += set.len();
                r.fwd_bytes += hb_bytes(set.capacity(), sz_leaf);
            }
        }
        for vs in self.fidx.values() {
            r.fidx_vs += vs.len();
            r.fidx_bytes += hb_bytes(vs.capacity(), sz_fidx_val);
        }
        r
    }

    /// Merge `from` into `self` (union). Used by the delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        use hashbrown::hash_map::Entry;
        for ((f, v), pm) in from.fwd.drain() {
            let dst = match self.fwd.entry((f.clone(), v.clone())) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    self.fidx.entry(f).or_default().insert(v);
                    e.insert(Map::default())
                }
            };
            for (p, set) in pm {
                let dst_set = dst.entry(p).or_default();
                for mf in set {
                    if dst_set.insert(mf) {
                        self.len += 1;
                    }
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
    /// Number of `(F,V,P)` inner entries across all groups.
    pub p_entries: usize,
    /// Number of `(M,Fp)` leaf elements across all leaves (== rows).
    pub leaf_elems: usize,
    /// Estimated bytes for the whole forward trie (outer map + inner maps + leaf sets).
    pub fwd_bytes: usize,
    /// Number of distinct functions in the `fidx` side-index (outer keys).
    pub fidx_funcs: usize,
    /// Number of `V` entries across all `fidx` sets (== distinct `(F,V)` groups == `fv_groups`).
    pub fidx_vs: usize,
    /// Estimated bytes for the `fidx` side-index (outer map + per-function V sets).
    pub fidx_bytes: usize,
    /// Element sizes `(outer, inner, leaf, fidx_outer, fidx_val)` for reference.
    pub elem_sizes: (usize, usize, usize, usize, usize),
}

impl std::fmt::Display for HeapReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        let total = self.fwd_bytes + self.fidx_bytes;
        let pct = |b: usize| if total == 0 { 0.0 } else { 100.0 * b as f64 / total as f64 };
        let (o, i, l, fo, fv) = self.elem_sizes;
        write!(
            f,
            "locals store estimate: total {:.1} MB over {} rows ({:.0} B/row) | \
             fwd {:.1} MB ({:.0}%): {} (F,V) groups, {} (F,V,P) entries, {} leaves | \
             fidx {:.1} MB ({:.0}%): {} funcs, {} V entries | \
             elem sizes o={} i={} l={} fo={} fv={} B",
            mb(total),
            self.rows,
            if self.rows == 0 { 0.0 } else { total as f64 / self.rows as f64 },
            mb(self.fwd_bytes),
            pct(self.fwd_bytes),
            self.fv_groups,
            self.p_entries,
            self.leaf_elems,
            mb(self.fidx_bytes),
            pct(self.fidx_bytes),
            self.fidx_funcs,
            self.fidx_vs,
            o,
            i,
            l,
            fo,
            fv,
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
        Self { fwd: Map::default(), fidx: Map::default(), len: 0 }
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
        Self { fwd: self.fwd.clone(), fidx: self.fidx.clone(), len: self.len }
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
            fn to_rel_index<'a>(&'a self, rel: &'a LocalsIndCommon<F, V, P, M, Fp>) -> Self::RelIndex<'a> {
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
            c.fwd.iter().flat_map(|((f, v), pm)| {
                pm.iter().flat_map(move |(p, set)| set.iter().map(move |(m, fp)| (f, v, p, m, fp)))
            })
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
        let pm = self.0.fwd.get(key)?;
        Some(DynIter::new(move || {
            pm.iter().flat_map(|(p, set)| set.iter().map(move |(m, fp)| (p, m, fp)))
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
        Box::new(self.0.fwd.iter().map(|((f, v), pm)| {
            let it = DynIter::new(move || pm.iter().flat_map(|(p, set)| set.iter().map(move |(m, fp)| (p, m, fp))));
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
        let set = self.0.fwd.get(&(key.0.clone(), key.1.clone()))?.get(&key.2)?;
        Some(DynIter::new(move || set.iter().map(|(m, fp)| (m, fp))))
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
        Box::new(self.0.fwd.iter().flat_map(|((f, v), pm)| {
            pm.iter().map(move |(p, set)| {
                let it = DynIter::new(move || set.iter().map(|(m, fp)| (m, fp)));
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
        // flow-variables via the `fidx` side-index, then yield every `(v, p)` whose leaf
        // contains `(m, fp)`. This touches one function's `(F,V)` groups instead of scanning all
        // of `fwd`. Cold path (rule 2.2, ~tens of probes). Returns `Some` of a possibly-empty
        // iterator; an empty result is join-equivalent to `None`, keeping this a single pass.
        let c = self.0;
        let (f, m, fp) = key.clone();
        Some(DynIter::new(move || {
            let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
            // `fidx[f]` and `fwd` are maintained in lockstep, so every V here has an `fwd` group.
            c.fidx.get(&f).into_iter().flat_map(move |vs| {
                let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
                vs.iter().flat_map(move |v| {
                    let (m, fp) = (m.clone(), fp.clone());
                    c.fwd.get(&(f.clone(), v.clone())).into_iter().flat_map(move |pm| {
                        let (m, fp) = (m.clone(), fp.clone());
                        pm.iter().filter_map(move |(p, set)| {
                            if set.contains(&(m.clone(), fp.clone())) {
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
        // ever shows up in a profile, it means some rule started iterating `0_3_4` and the
        // inverse should be reconsidered.
        let mut groups: Map<(&'a F, &'a M, &'a Fp), Vec<(&'a V, &'a P)>> = Map::default();
        for ((f, v), pm) in self.0.fwd.iter() {
            for (p, set) in pm.iter() {
                for (m, fp) in set.iter() {
                    groups.entry((f, m, fp)).or_default().push((v, p));
                }
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
        Box::new(self.0.fwd.iter().flat_map(|((f, v), pm)| {
            pm.iter().flat_map(move |(p, set)| set.iter().map(move |(m, fp)| ((f, v, p, m, fp), std::iter::once(&()))))
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
