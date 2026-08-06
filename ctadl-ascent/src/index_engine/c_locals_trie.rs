//! The parallel twin of [`super::locals_trie`], for `ascent_par!`.
//!
//! [`super::locals_trie`] holds `locals` in one shared store: a forward map `(F,V) -> {(P,M,Fp)}`
//! whose groups are [`HybridSet`]s, plus a side-index `fidx: F -> {V}` that narrows the derived
//! `0_3_4` probe. Every logical index is a lightweight *view* over that store. This module keeps
//! all of that and changes exactly one thing: the two outer maps become `DashMap`s, so many rayon
//! threads can insert at once.
//!
//! The groups stay [`HybridSet`]s, so the whole reason `locals_trie` exists survives: two-word
//! groups, 24 B packed leaves, one allocation per group, and Swiss promotion above
//! [`SMALL_THRESHOLD`] that keeps the delta->total merge at O(delta). `DashMap`'s own overhead is
//! per *shard* (`shards_count()` ~ 4x cores), which is nothing against a store measured in
//! gigabytes.
//!
//! ## What parallel Ascent asks for
//!
//! Ascent's parallel codegen runs each fixpoint iteration in three phases:
//!
//! 1. **Freeze.** `total` and `delta` (the ind_common *and* every index marker field) are
//!    `freeze()`d. A frozen store is a `dashmap::ReadOnlyView`, which hands out plain `&V`
//!    references, so reads need no locks at all.
//! 2. **Evaluate.** Rules run on rayon threads. They read frozen `total`/`delta` and write only
//!    to `new`, through `CRelFullIndexWrite::insert_if_not_present(&self, ..)` — a *shared*
//!    reference. The physical relation is pushed through `&self` too, returning the row index.
//! 3. **Unfreeze and merge.** Everything is `unfreeze()`d and `RelIndexMerge` runs
//!    single-threaded on `&mut`.
//!
//! The simplification that makes this store easy: during evaluation **`new` is only written and
//! `total`/`delta` are only read**. No store is concurrently read and written. So the only
//! concurrency to get right is "many threads inserting into `new`".
//!
//! ## Reads are frozen; writes take one shard lock
//!
//! A concurrent insert takes the `DashMap` entry for `(f, v)` — one shard write lock — and
//! inserts the leaf into that group's [`HybridSet`] under it. The lock makes the winner unique,
//! so exactly one thread sees `true`, pushes the physical row, and sets `changed`. That
//! uniqueness is what keeps semi-naive evaluation and the physical row count correct.
//!
//! On a vacant group we also record `v` in `fidx[f]`, after releasing the `fwd` shard. The two
//! maps are therefore briefly out of step, which no reader can observe: readers only ever see
//! frozen snapshots, and freezing happens between iterations, when no thread is inserting.
//!
//! Correctness note, same as the serial module: the *only* real `RelIndexMerge` lives on the
//! ind_common ([`CLocalsIndCommon`]). Every index write target has a no-op merge, so nothing is
//! merged twice per iteration.
//!
//! ## Parallel iterators
//!
//! Rules parallelize over their first clauses, which read through `CRelIndexRead::c_index_get`
//! and `CRelIndexReadAll::c_iter_all`. Those must return `ParallelIterator`s. Whole-store
//! iteration ([`CAllRowsParIter`] and friends) splits over `DashMap`'s shards via ascent's
//! `DashMapViewParIter`, which is where the real scaling has to come from. A *keyed* probe
//! (`0_1`, `0_1_2`, `0_3_4`) instead collects its matches into a [`CollectedParIter`]: the median
//! group holds one leaf, `0_3_4` is cold, and the allocation only happens when a keyed clause
//! drives a parallel loop. A native `HybridSet` par-iterator (splitting the slot range, which
//! both representations store as a flat array) would remove those collects; do that only if a
//! profile asks for it.

use std::hash::{BuildHasherDefault, Hash};
use std::marker::PhantomData;
use std::ops::Index;
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::dashmap::{DashMap, ReadOnlyView};
use ascent::internal::{
    CRelFullIndexWrite, CRelIndexRead, CRelIndexReadAll, CRelIndexWrite, DashMapViewParIter,
    Freezable, RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex, shards_count,
};
use ascent::rayon::iter::plumbing::UnindexedConsumer;
use ascent::rayon::iter::{IntoParallelIterator, ParallelIterator};
use ascent_base::util::update;
use rustc_hash::FxHasher;

use super::hybrid_set::HybridSet;
use super::locals_trie::{DynIter, HeapReport, hb_bytes};

/// The store keys are trusted ids derived from the program, so we hash them with the fast,
/// deterministic `FxHasher` rather than the DoS-resistant SipHash the std collections use. This is
/// the same hasher ascent's own concurrent indices use, so shard counts line up.
pub type Hasher = BuildHasherDefault<FxHasher>;
type Set<T> = hashbrown::HashSet<T, Hasher>;
/// The leaves of one `(F,V)` group. See [`super::locals_trie`] for why this is a [`HybridSet`].
type Group<P, M, Fp> = HybridSet<(P, M, Fp)>;

// ---------------------------------------------------------------------------
// A freezable concurrent map.
// ---------------------------------------------------------------------------

/// A `DashMap` that can be turned into a lock-free read-only snapshot and back.
///
/// This is ascent's `CRelFullIndex` pattern, factored out so both concurrent stores in this crate
/// can use it. Frozen is the read state: `ReadOnlyView` hands out `&V` with no guard, which is
/// what lets a rule hold a reference into a group while iterating it. Unfrozen is the write state.
/// Both conversions are O(shards), not O(entries).
pub enum CMap<K, V> {
    Unfrozen(DashMap<K, V, Hasher>),
    Frozen(ReadOnlyView<K, V, Hasher>),
}

impl<K: Clone + Eq + Hash, V> Default for CMap<K, V> {
    fn default() -> Self {
        // Match ascent's shard count so that our stores shard exactly like its own concurrent
        // indices, and so the count does not depend on how many threads happen to be alive.
        Self::Unfrozen(DashMap::with_hasher_and_shard_amount(
            Hasher::default(),
            shards_count(),
        ))
    }
}

impl<K: Clone + Eq + Hash, V: Clone> Clone for CMap<K, V> {
    fn clone(&self) -> Self {
        match self {
            CMap::Unfrozen(dm) => CMap::Unfrozen(dm.clone()),
            CMap::Frozen(v) => CMap::Frozen(v.clone()),
        }
    }
}

/// Total entry count extrapolated from the first few of `shards` shards, where `shard_len(i)`
/// gives shard `i`'s entry count. Shards are hashed independently and uniformly, so a small
/// sample tracks the whole map closely enough for a join-ordering decision.
#[inline]
fn extrapolate(shards: usize, shard_len: impl Fn(usize) -> usize) -> usize {
    const SAMPLE: usize = 4;
    let n = shards.min(SAMPLE);
    if n == 0 {
        return 0;
    }
    (0..n).map(shard_len).sum::<usize>() * shards / n
}

impl<K: Clone + Eq + Hash, V> CMap<K, V> {
    #[inline]
    pub fn freeze(&mut self) {
        update(self, |this| match this {
            CMap::Unfrozen(dm) => CMap::Frozen(dm.into_read_only()),
            CMap::Frozen(_) => this,
        })
    }

    #[inline]
    pub fn unfreeze(&mut self) {
        update(self, |this| match this {
            CMap::Frozen(v) => CMap::Unfrozen(v.into_inner()),
            CMap::Unfrozen(_) => this,
        })
    }

    #[inline]
    pub fn frozen(&self) -> &ReadOnlyView<K, V, Hasher> {
        match self {
            CMap::Frozen(v) => v,
            CMap::Unfrozen(_) => panic!("CMap::frozen() on an unfrozen map"),
        }
    }

    #[inline]
    pub fn unfrozen(&self) -> &DashMap<K, V, Hasher> {
        match self {
            CMap::Unfrozen(dm) => dm,
            CMap::Frozen(_) => panic!("CMap::unfrozen() on a frozen map"),
        }
    }

    #[inline]
    pub fn unfrozen_mut(&mut self) -> &mut DashMap<K, V, Hasher> {
        match self {
            CMap::Unfrozen(dm) => dm,
            CMap::Frozen(_) => panic!("CMap::unfrozen_mut() on a frozen map"),
        }
    }

    /// Number of entries, in either state.
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            CMap::Unfrozen(dm) => dm.len(),
            CMap::Frozen(v) => v.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate entry count, from a sample of the shards.
    ///
    /// This is what the views report as `len_estimate`, which Ascent calls to order the two sides
    /// of a reorderable join — and calls from inside the outer loop, so it must not cost
    /// `shards_count()` lock acquisitions each time. Ascent's own `CRelIndex` samples four shards
    /// for the same reason.
    #[inline]
    pub fn len_estimate(&self) -> usize {
        match self {
            CMap::Frozen(v) => {
                let shards = v.shards();
                extrapolate(shards.len(), |i| shards[i].read().len())
            }
            CMap::Unfrozen(dm) => {
                let shards = dm.shards();
                extrapolate(shards.len(), |i| shards[i].read().len())
            }
        }
    }

    /// Estimated heap bytes of the shard hash tables, for an entry `elem` bytes wide.
    ///
    /// A `DashMap` is an array of independent hashbrown tables, and each one rounds its bucket
    /// count up on its own, so the whole map's bytes are the sum over shards rather than
    /// [`hb_bytes`] of a single capacity. Reading a shard's capacity takes its lock; this runs
    /// after the fixpoint, single-threaded.
    pub fn table_bytes(&self, elem: usize) -> usize {
        match self {
            CMap::Unfrozen(dm) => dm
                .shards()
                .iter()
                .map(|s| hb_bytes(s.read().capacity(), elem))
                .sum(),
            CMap::Frozen(v) => v
                .shards()
                .iter()
                .map(|s| hb_bytes(s.read().capacity(), elem))
                .sum(),
        }
    }
}

// ---------------------------------------------------------------------------
// Physical `rel!` storage.
// ---------------------------------------------------------------------------

/// The parallel [`super::locals_trie::CountingVec`]: it stores no tuples, only the row count.
///
/// Parallel codegen pushes through `&self` and uses the returned index as the synthetic row
/// index, so the counter is an `AtomicUsize` and `push` is one `fetch_add`. All the data lives in
/// the shared store; `prog.locals.len()` is the only thing that reads this after a run.
pub struct CCountingVec<T> {
    len: AtomicUsize,
    _p: PhantomData<T>,
}
impl<T> Default for CCountingVec<T> {
    fn default() -> Self {
        Self {
            len: AtomicUsize::new(0),
            _p: PhantomData,
        }
    }
}
impl<T> CCountingVec<T> {
    /// Count one row and return its index. Ascent calls this exactly once per newly inserted row.
    #[inline(always)]
    pub fn push(&self, _v: T) -> usize {
        self.len.fetch_add(1, Ordering::Relaxed)
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[inline(always)]
    pub fn iter(&self) -> std::iter::Empty<&T> {
        std::iter::empty()
    }
}
impl<T> Index<usize> for CCountingVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("c_locals_trie::CCountingVec stores no tuples")
    }
}

// ---------------------------------------------------------------------------
// The shared store, which is Ascent's `ind_common`.
// ---------------------------------------------------------------------------

/// The concurrent `locals` store. See the module docs.
pub struct CLocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    /// Forward map: `(F,V)` -> the set of `(P, M, Fp)` leaves for that group. It serves the
    /// `none`, `0_1`, `0_1_2`, and existence views directly, and the `0_3_4` view by *scanning*.
    fwd: CMap<(F, V), Group<P, M, Fp>>,
    /// Side-index: `F` -> the set of `V` present for that function, kept in lockstep with `fwd`'s
    /// outer keys. It lets a `0_3_4` probe visit one function's groups instead of all of `fwd`.
    fidx: CMap<F, Set<V>>,
    /// Row count. Bumped once per winning insert, so it stays exact under concurrency.
    len: AtomicUsize,
}

impl<F, V, P, M, Fp> Default for CLocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            fwd: CMap::default(),
            fidx: CMap::default(),
            len: AtomicUsize::new(0),
        }
    }
}

impl<F, V, P, M, Fp> Clone for CLocalsIndCommon<F, V, P, M, Fp>
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
            len: AtomicUsize::new(self.len()),
        }
    }
}

impl<F, V, P, M, Fp> Freezable for CLocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn freeze(&mut self) {
        self.fwd.freeze();
        self.fidx.freeze();
    }
    fn unfreeze(&mut self) {
        self.fwd.unfreeze();
        self.fidx.unfreeze();
    }
}

impl<F, V, P, M, Fp> CLocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of distinct `(F, V)` groups, that is, of variables reached by some formal. O(shards).
    /// Works in either freeze state, because a run can leave the store frozen: the last SCC to
    /// mention `locals` uses it body-only, and that path freezes without unfreezing.
    #[inline]
    pub fn num_reached_variables(&self) -> usize {
        self.fwd.len()
    }

    /// Existence probe against the **frozen** store. Rules only ever probe `total`/`delta`, which
    /// are frozen for the whole evaluation phase.
    #[inline]
    fn contains(&self, f: &F, v: &V, p: &P, m: &M, fp: &Fp) -> bool {
        // `(P,M,Fp)` are cheap to clone: 8-byte handles plus an i16.
        self.fwd
            .frozen()
            .get(&(f.clone(), v.clone()))
            .is_some_and(|group| group.contains(&(p.clone(), m.clone(), fp.clone())))
    }

    /// Insert a full tuple through a **shared** reference. Returns true if it was new to *this*
    /// store, and true for exactly one caller when several race on the same tuple.
    ///
    /// The `(f, v)` entry is one shard write lock, held across the group insert (including a
    /// possible Swiss promotion). That lock is the store's contention point: the pessimal shape is
    /// many threads hitting one huge group. `fidx` is touched only when the group is new, and
    /// strictly after the `fwd` shard lock is released, so the two maps are never held at once.
    fn c_insert(&self, key: &(F, V, P, M, Fp)) -> bool {
        use ascent::dashmap::mapref::entry::Entry;
        let (f, v, p, m, fp) = key;
        let leaf = (p.clone(), m.clone(), fp.clone());

        let mut new_group = false;
        let added = match self.fwd.unfrozen().entry((f.clone(), v.clone())) {
            Entry::Occupied(mut occ) => occ.get_mut().insert(leaf),
            Entry::Vacant(vac) => {
                let mut group = Group::new();
                group.insert(leaf);
                // The `RefMut` this returns is dropped at the semicolon, releasing the shard.
                vac.insert(group);
                new_group = true;
                true
            }
        };
        if new_group {
            self.fidx
                .unfrozen()
                .entry(f.clone())
                .or_default()
                .insert(v.clone());
        }
        if added {
            self.len.fetch_add(1, Ordering::Relaxed);
        }
        added
    }

    /// Merge `from` into `self`, taking the union. This is the delta->total move, and Ascent runs
    /// it single-threaded on `&mut` with both sides unfrozen.
    ///
    /// Reusing [`HybridSet::merge`] keeps the merge O(delta) rather than O(accumulated total),
    /// which is the whole point of promoting large groups.
    fn absorb(&mut self, from: &mut Self) {
        use ascent::dashmap::mapref::entry::Entry;
        let mut added = 0usize;
        {
            // Single-threaded here, so `entry()` on `&DashMap` cannot deadlock against anyone.
            let fwd = self.fwd.unfrozen();
            let fidx = self.fidx.unfrozen();
            // Drain `from`'s shard tables in place. Draining rather than replacing the whole map
            // matters: `from` becomes the next `new` store, and a fresh `DashMap` would allocate a
            // whole shard array (`shards_count()` locked tables) on *every* fixpoint iteration.
            // Groups move out of the drain, so no group is ever cloned.
            for shard in from.fwd.unfrozen_mut().shards_mut() {
                for ((f, v), group) in shard.get_mut().drain() {
                    let group = group.into_inner();
                    match fwd.entry((f.clone(), v.clone())) {
                        Entry::Occupied(mut occ) => added += occ.get_mut().merge(group),
                        Entry::Vacant(vac) => {
                            added += group.len();
                            vac.insert(group);
                            fidx.entry(f).or_default().insert(v);
                        }
                    }
                }
            }
        }
        *self.len.get_mut() += added;
        *from.len.get_mut() = 0;
        from.fidx.unfrozen().clear();
    }

    /// Estimated heap bytes, broken down per structure. Same report as the serial store, with the
    /// outer-map figures summed over `DashMap`'s shard tables (see [`CMap::table_bytes`]).
    ///
    /// Runs in either freeze state, and takes one pass, O(rows).
    pub fn heap_report(&self) -> HeapReport {
        let sz_outer = std::mem::size_of::<((F, V), Group<P, M, Fp>)>();
        let sz_leaf = std::mem::size_of::<(P, M, Fp)>();
        let sz_fidx_outer = std::mem::size_of::<(F, Set<V>)>();
        let sz_fidx_val = std::mem::size_of::<V>();

        let mut r = HeapReport {
            rows: self.len(),
            fv_groups: self.fwd.len(),
            p_entries: 0,
            leaf_elems: 0,
            fwd_bytes: self.fwd.table_bytes(sz_outer),
            fidx_funcs: self.fidx.len(),
            fidx_vs: 0,
            fidx_bytes: self.fidx.table_bytes(sz_fidx_outer),
            elem_sizes: (sz_outer, 0, sz_leaf, sz_fidx_outer, sz_fidx_val),
            max_group: 0,
            large_groups: 0,
            group_hist: Vec::new(),
        };
        // Counting distinct `P` per group needs a scratch set, because groups are unordered. One
        // set, cleared per group, keeps the report to a single extra allocation. It holds `P` by
        // value rather than by reference: the unfrozen `DashMap` hands out guarded values whose
        // lifetime is the loop iteration, and `P` is a cheap handle to clone.
        let mut ps: Set<P> = Set::default();
        // Each `visit_*` closure borrows `r` mutably, so it is scoped to end the borrow before
        // the next one starts.
        {
            let mut visit_group = |group: &Group<P, M, Fp>| {
                r.leaf_elems += group.len();
                r.max_group = r.max_group.max(group.len());
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
                    ps.insert(p.clone());
                }
                r.p_entries += ps.len();
            };
            match &self.fwd {
                CMap::Frozen(v) => v.iter().for_each(|(_, group)| visit_group(group)),
                CMap::Unfrozen(dm) => dm.iter().for_each(|e| visit_group(e.value())),
            }
        }
        {
            let mut visit_vs = |vs: &Set<V>| {
                r.fidx_vs += vs.len();
                r.fidx_bytes += hb_bytes(vs.capacity(), sz_fidx_val);
            };
            match &self.fidx {
                CMap::Frozen(v) => v.iter().for_each(|(_, vs)| visit_vs(vs)),
                CMap::Unfrozen(dm) => dm.iter().for_each(|e| visit_vs(e.value())),
            }
        }
        r
    }
}

// The one real merge lives here, on the ind_common.
impl<F, V, P, M, Fp> RelIndexMerge for CLocalsIndCommon<F, V, P, M, Fp>
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
    // We take the default `merge_delta_to_total_new_to_delta`: move delta into total, then swap
    // new and delta.
}

// ---------------------------------------------------------------------------
// Parallel iterator adaptors.
// ---------------------------------------------------------------------------

/// A `Vec` of already-collected items, exposed as a `ParallelIterator`.
///
/// This is how the *keyed* probes return their matches. A group is a `HybridSet`, which has no
/// native parallel iterator, and rayon needs a nameable `ParallelIterator + Clone` type. Since the
/// median group holds one leaf, collecting is cheap; and it only ever happens when a keyed clause
/// drives a parallel loop, which is rare.
pub struct CollectedParIter<T>(Vec<T>);

impl<T: Clone> Clone for CollectedParIter<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Send> ParallelIterator for CollectedParIter<T> {
    type Item = T;
    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        self.0.into_par_iter().drive_unindexed(consumer)
    }
    fn opt_len(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

/// Every row of the store, as `(&F, &V, &P, &M, &Fp)`.
///
/// Splitting happens over `DashMap`'s shards (ascent's `DashMapViewParIter`); within a shard entry
/// the group is walked serially with `flat_map_iter`, which does not require the inner iterator to
/// be `Send`. That is the granularity that matters: there are many more groups than threads.
pub struct CAllRowsParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, V), Group<P, M, Fp>, Hasher>,
}

impl<F, V, P, M, Fp> Clone for CAllRowsParIter<'_, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        Self { fwd: self.fwd }
    }
}

impl<'a, F, V, P, M, Fp> ParallelIterator for CAllRowsParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Item = (&'a F, &'a V, &'a P, &'a M, &'a Fp);

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .flat_map_iter(|((f, v), group)| group.iter().map(move |(p, m, fp)| (f, v, p, m, fp)))
            .drive_unindexed(consumer)
    }
}

/// `iter_all` over the full existence index: every row keyed by itself.
pub struct CFullAllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, V), Group<P, M, Fp>, Hasher>,
}

impl<'a, F, V, P, M, Fp> ParallelIterator for CFullAllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Item = (
        (&'a F, &'a V, &'a P, &'a M, &'a Fp),
        ascent::rayon::iter::Once<&'a ()>,
    );

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .flat_map_iter(|((f, v), group)| {
                group
                    .iter()
                    .map(move |(p, m, fp)| ((f, v, p, m, fp), ascent::rayon::iter::once(&())))
            })
            .drive_unindexed(consumer)
    }
}

/// `iter_all` over `0_1`: one key per `(F,V)` group, with that group's leaves.
pub struct CView01AllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, V), Group<P, M, Fp>, Hasher>,
}

impl<'a, F, V, P, M, Fp> ParallelIterator for CView01AllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Item = ((&'a F, &'a V), CollectedParIter<(&'a P, &'a M, &'a Fp)>);

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .map(|((f, v), group)| {
                (
                    (f, v),
                    CollectedParIter(group.iter().map(|(p, m, fp)| (p, m, fp)).collect()),
                )
            })
            .drive_unindexed(consumer)
    }
}

/// `iter_all` over `0_1_2`: one key per distinct `(F,V,P)`.
///
/// Groups are unordered, so the leaves carrying one `P` have to be bucketed rather than sliced off
/// as a run. Not a hot path: the rules point-probe `0_1_2`.
pub struct CView012AllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, V), Group<P, M, Fp>, Hasher>,
}

impl<'a, F, V, P, M, Fp> ParallelIterator for CView012AllParIter<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Item = ((&'a F, &'a V, &'a P), CollectedParIter<(&'a M, &'a Fp)>);

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .flat_map_iter(|((f, v), group)| {
                let mut byp: hashbrown::HashMap<&P, Vec<(&M, &Fp)>, Hasher> =
                    hashbrown::HashMap::default();
                for (p, m, fp) in group.iter() {
                    byp.entry(p).or_default().push((m, fp));
                }
                byp.into_iter()
                    .map(move |(p, mfs)| ((f, v, p), CollectedParIter(mfs)))
            })
            .drive_unindexed(consumer)
    }
}

// ---------------------------------------------------------------------------
// Read views and their `ToRelIndex` markers.
//
// Under the blanket `ToRelIndex0` impl, `to_c_rel_index_write` hands back whatever `to_rel_index`
// returns. So the concurrent write traits live on the *views*: `CViewFull` is the one real writer,
// and the rest are no-ops. Every marker also needs `Freezable`, because codegen freezes the marker
// fields alongside the store.
// ---------------------------------------------------------------------------

/// Defines a zero-sized `ToRelIndex` marker over the concrete `CLocalsIndCommon` store. The read
/// view doubles as the write target, as it does in `ascent-byods-rels`' own parallel eqrel.
macro_rules! marker {
    ($to:ident, $view:ident) => {
        pub struct $to<F, V, P, M, Fp>(PhantomData<(F, V, P, M, Fp)>);
        impl<F, V, P, M, Fp> Default for $to<F, V, P, M, Fp> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<F, V, P, M, Fp> Freezable for $to<F, V, P, M, Fp> {}
        impl<F, V, P, M, Fp> ToRelIndex<CLocalsIndCommon<F, V, P, M, Fp>> for $to<F, V, P, M, Fp>
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
                CLocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a CLocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndex<'a> {
                $view(rel)
            }
            type RelIndexWrite<'a>
                = $view<'a, F, V, P, M, Fp>
            where
                Self: 'a,
                CLocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                rel: &'a mut CLocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndexWrite<'a> {
                $view(rel)
            }
        }

        pub struct $view<'a, F, V, P, M, Fp>(&'a CLocalsIndCommon<F, V, P, M, Fp>)
        where
            F: Clone + Eq + Hash,
            V: Clone + Eq + Hash,
            P: Clone + Eq + Hash,
            M: Clone + Eq + Hash,
            Fp: Clone + Eq + Hash;

        // The real merge lives on the ind_common; a write target must never merge again, or
        // semi-naive evaluation sees each row twice. See the module docs.
        impl<F, V, P, M, Fp> RelIndexMerge for $view<'_, F, V, P, M, Fp>
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
            fn merge_delta_to_total_new_to_delta(
                _new: &mut Self,
                _delta: &mut Self,
                _total: &mut Self,
            ) {
            }
        }
    };
}

/// A view index that holds no data of its own: the row is already in the shared store, put there
/// by the full index. Both write traits are no-ops.
macro_rules! noop_writes {
    ($view:ident, $key:ty, $val:ty) => {
        impl<F, V, P, M, Fp> RelIndexWrite for $view<'_, F, V, P, M, Fp>
        where
            F: Clone + Eq + Hash,
            V: Clone + Eq + Hash,
            P: Clone + Eq + Hash,
            M: Clone + Eq + Hash,
            Fp: Clone + Eq + Hash,
        {
            type Key = $key;
            type Value = $val;
            #[inline(always)]
            fn index_insert(&mut self, _key: Self::Key, _value: Self::Value) {}
        }
        impl<F, V, P, M, Fp> CRelIndexWrite for $view<'_, F, V, P, M, Fp>
        where
            F: Clone + Eq + Hash,
            V: Clone + Eq + Hash,
            P: Clone + Eq + Hash,
            M: Clone + Eq + Hash,
            Fp: Clone + Eq + Hash,
        {
            type Key = $key;
            type Value = $val;
            #[inline(always)]
            fn index_insert(&self, _key: Self::Key, _value: Self::Value) {}
        }
    };
}

marker!(CToNone, CViewNone);
marker!(CTo01, CView01);
marker!(CTo012, CView012);
marker!(CTo034, CView034);
marker!(CToFull, CViewFull);

noop_writes!(CViewNone, (), (F, V, P, M, Fp));
noop_writes!(CView01, (F, V), (P, M, Fp));
noop_writes!(CView012, (F, V, P), (M, Fp));
noop_writes!(CView034, (F, M, Fp), (V, P));

// ---- none: () -> (F,V,P,M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for CViewNone<'a, F, V, P, M, Fp>
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
        let fwd = self.0.fwd.frozen();
        Some(DynIter::new(move || {
            fwd.iter()
                .flat_map(|((f, v), group)| group.iter().map(move |(p, m, fp)| (f, v, p, m, fp)))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        1
    }
    #[inline]
    fn is_empty(&'a self) -> bool {
        self.0.fwd.is_empty()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for CViewNone<'a, F, V, P, M, Fp>
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
impl<'a, F, V, P, M, Fp> CRelIndexRead<'a> for CViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type IteratorType = CAllRowsParIter<'a, F, V, P, M, Fp>;
    #[inline]
    fn c_index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
        Some(CAllRowsParIter {
            fwd: self.0.fwd.frozen(),
        })
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexReadAll<'a> for CViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type ValueIteratorType = CAllRowsParIter<'a, F, V, P, M, Fp>;
    type AllIteratorType = ascent::rayon::iter::Once<((), Self::ValueIteratorType)>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        ascent::rayon::iter::once((
            (),
            CAllRowsParIter {
                fwd: self.0.fwd.frozen(),
            },
        ))
    }
}

// ---- 0_1: (F,V) -> (P,M,Fp) -----------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for CView01<'a, F, V, P, M, Fp>
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
        let group = self.0.fwd.frozen().get(key)?;
        Some(DynIter::new(move || {
            group.iter().map(|(p, m, fp)| (p, m, fp))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len_estimate()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for CView01<'a, F, V, P, M, Fp>
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
        Box::new(self.0.fwd.frozen().iter().map(|((f, v), group)| {
            let it = DynIter::new(move || group.iter().map(|(p, m, fp)| (p, m, fp)));
            ((f, v), it)
        }))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexRead<'a> for CView01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (F, V);
    type Value = (&'a P, &'a M, &'a Fp);
    type IteratorType = CollectedParIter<Self::Value>;
    #[inline]
    fn c_index_get(&'a self, key: &(F, V)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.frozen().get(key)?;
        Some(CollectedParIter(
            group.iter().map(|(p, m, fp)| (p, m, fp)).collect(),
        ))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexReadAll<'a> for CView01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a V);
    type Value = (&'a P, &'a M, &'a Fp);
    type ValueIteratorType = CollectedParIter<Self::Value>;
    type AllIteratorType = CView01AllParIter<'a, F, V, P, M, Fp>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CView01AllParIter {
            fwd: self.0.fwd.frozen(),
        }
    }
}

// ---- 0_1_2: (F,V,P) -> (M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for CView012<'a, F, V, P, M, Fp>
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
        let group = self.0.fwd.frozen().get(&(key.0.clone(), key.1.clone()))?;
        let p = key.2.clone();
        // A group is a set, not a sorted run, so this filters rather than slicing a range. The
        // scan up front is what lets us return `None` for a `P` the group does not hold, which
        // cuts the caller's whole join; it stops at the first match, and the median group holds a
        // single leaf.
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
        self.0.fwd.len_estimate()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for CView012<'a, F, V, P, M, Fp>
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
        Box::new(self.0.fwd.frozen().iter().flat_map(|((f, v), group)| {
            let mut byp: hashbrown::HashMap<&'a P, Vec<(&'a M, &'a Fp)>, Hasher> =
                hashbrown::HashMap::default();
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
impl<'a, F, V, P, M, Fp> CRelIndexRead<'a> for CView012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (F, V, P);
    type Value = (&'a M, &'a Fp);
    type IteratorType = CollectedParIter<Self::Value>;
    #[inline]
    fn c_index_get(&'a self, key: &(F, V, P)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.frozen().get(&(key.0.clone(), key.1.clone()))?;
        let p = &key.2;
        let matches: Vec<_> = group
            .iter()
            .filter_map(|(pp, m, fp)| (pp == p).then_some((m, fp)))
            .collect();
        // A miss must be `None`, not an empty iterator: `None` cuts the caller's join.
        if matches.is_empty() {
            return None;
        }
        Some(CollectedParIter(matches))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexReadAll<'a> for CView012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a V, &'a P);
    type Value = (&'a M, &'a Fp);
    type ValueIteratorType = CollectedParIter<Self::Value>;
    type AllIteratorType = CView012AllParIter<'a, F, V, P, M, Fp>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CView012AllParIter {
            fwd: self.0.fwd.frozen(),
        }
    }
}

// ---- 0_3_4: (F,M,Fp) -> (V,P) ---------------------------------------------
//
// Derived, not materialized. For the probed function `f` we visit only its flow-variables, found
// through `fidx`, and yield every `(v, p)` whose group holds a leaf with this `(m, fp)`. A cold
// path: rule 2.2 makes on the order of tens of probes.
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for CView034<'a, F, V, P, M, Fp>
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
        let fwd = self.0.fwd.frozen();
        let fidx = self.0.fidx.frozen();
        let (f, m, fp) = key.clone();
        Some(DynIter::new(move || {
            let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
            // `fidx[f]` and `fwd` are in lockstep, so every V here has an `fwd` group.
            fidx.get(&f).into_iter().flat_map(move |vs| {
                let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
                vs.iter().flat_map(move |v| {
                    let (m, fp) = (m.clone(), fp.clone());
                    fwd.get(&(f.clone(), v.clone()))
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
        // There is no materialized inverse to size. `0_3_4` is only probed, never used as a join
        // driver, so a large estimate is safe: it discourages the planner from choosing it as one.
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for CView034<'a, F, V, P, M, Fp>
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
        // Correct fallback: invert `fwd` on demand and throw the result away. Nothing iterates
        // `0_3_4` as a driver; if this shows up in a profile, some rule has started to.
        Box::new(invert_034(self.0).into_iter().map(|(key, vps)| {
            let it = DynIter::new(move || vps.clone().into_iter());
            (key, it)
        }))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexRead<'a> for CView034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (F, M, Fp);
    type Value = (&'a V, &'a P);
    type IteratorType = CollectedParIter<Self::Value>;
    #[inline]
    fn c_index_get(&'a self, key: &(F, M, Fp)) -> Option<Self::IteratorType> {
        Some(CollectedParIter(
            RelIndexRead::index_get(self, key)?.collect(),
        ))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexReadAll<'a> for CView034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a M, &'a Fp);
    type Value = (&'a V, &'a P);
    type ValueIteratorType = CollectedParIter<Self::Value>;
    type AllIteratorType = CollectedParIter<(Self::Key, Self::ValueIteratorType)>;
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CollectedParIter(
            invert_034(self.0)
                .into_iter()
                .map(|(key, vps)| (key, CollectedParIter(vps)))
                .collect(),
        )
    }
}

/// Builds the whole `(F,M,Fp) -> [(V,P)]` inverse of the frozen store. Only the two `0_3_4`
/// whole-relation iterators use it, and neither is on a hot path.
#[allow(clippy::type_complexity)]
fn invert_034<'a, F, V, P, M, Fp>(
    store: &'a CLocalsIndCommon<F, V, P, M, Fp>,
) -> hashbrown::HashMap<(&'a F, &'a M, &'a Fp), Vec<(&'a V, &'a P)>, Hasher>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    let mut groups: hashbrown::HashMap<(&'a F, &'a M, &'a Fp), Vec<(&'a V, &'a P)>, Hasher> =
        hashbrown::HashMap::default();
    for ((f, v), group) in store.fwd.frozen().iter() {
        for (p, m, fp) in group.iter() {
            groups.entry((f, m, fp)).or_default().push((v, p));
        }
    }
    groups
}

// ---- full 0_1_2_3_4: existence and the one real writer ---------------------
impl<'a, F, V, P, M, Fp> RelFullIndexRead<'a> for CViewFull<'a, F, V, P, M, Fp>
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
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for CViewFull<'a, F, V, P, M, Fp>
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
        self.0
            .contains(&key.0, &key.1, &key.2, &key.3, &key.4)
            .then(|| std::iter::once(&()))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for CViewFull<'a, F, V, P, M, Fp>
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
        Box::new(self.0.fwd.frozen().iter().flat_map(|((f, v), group)| {
            group
                .iter()
                .map(move |(p, m, fp)| ((f, v, p, m, fp), std::iter::once(&())))
        }))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexRead<'a> for CViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P, M, Fp);
    type Value = &'a ();
    type IteratorType = ascent::rayon::iter::Once<&'a ()>;
    #[inline]
    fn c_index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        self.0
            .contains(&key.0, &key.1, &key.2, &key.3, &key.4)
            .then(|| ascent::rayon::iter::once(&()))
    }
}
impl<'a, F, V, P, M, Fp> CRelIndexReadAll<'a> for CViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash + Send + Sync,
    V: Clone + Eq + Hash + Send + Sync,
    P: Clone + Eq + Hash + Send + Sync,
    M: Clone + Eq + Hash + Send + Sync,
    Fp: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type Value = &'a ();
    type ValueIteratorType = ascent::rayon::iter::Once<&'a ()>;
    type AllIteratorType = CFullAllParIter<'a, F, V, P, M, Fp>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CFullAllParIter {
            fwd: self.0.fwd.frozen(),
        }
    }
}

// The full index is where rows actually enter the store. Both the concurrent path (rule
// evaluation) and the `&mut` path (index build, and any serial caller) route to the same
// shared-reference insert.
impl<F, V, P, M, Fp> CRelFullIndexWrite for CViewFull<'_, F, V, P, M, Fp>
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
    fn insert_if_not_present(&self, key: &Self::Key, _v: ()) -> bool {
        self.0.c_insert(key)
    }
}
impl<F, V, P, M, Fp> RelFullIndexWrite for CViewFull<'_, F, V, P, M, Fp>
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
        self.0.c_insert(key)
    }
}
impl<F, V, P, M, Fp> CRelIndexWrite for CViewFull<'_, F, V, P, M, Fp>
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
    fn index_insert(&self, key: Self::Key, _value: ()) {
        self.0.c_insert(&key);
    }
}
impl<F, V, P, M, Fp> RelIndexWrite for CViewFull<'_, F, V, P, M, Fp>
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
        self.0.c_insert(&key);
    }
}

#[cfg(test)]
mod tests {
    use ascent::internal::{RelFullIndexRead, RelIndexRead, RelIndexReadAll};
    use ascent::rayon;

    use super::super::hybrid_set::SMALL_THRESHOLD;
    use super::*;

    type Store = CLocalsIndCommon<u32, u64, u64, i16, u64>;
    type Row = (u32, u64, u64, i16, u64);

    fn build(rows: &[Row]) -> Store {
        let store = Store::default();
        for row in rows {
            store.c_insert(row);
        }
        store
    }

    /// Builds a store from `rows` and checks every read view, serial and parallel, against those
    /// rows. Views read the *frozen* state, which is the only state a rule ever sees.
    fn check_views(rows: &[Row]) {
        let mut store = build(rows);
        let mut want: Vec<_> = rows.to_vec();
        want.sort_unstable();
        want.dedup();
        assert_eq!(store.len(), want.len(), "row count");
        assert_eq!(store.heap_report().rows, want.len(), "unfrozen heap_report");

        store.freeze();

        // Existence, both present and absent.
        for &(f, v, p, m, fp) in &want {
            assert!(store.contains(&f, &v, &p, &m, &fp), "missing {f:?}");
        }
        assert!(!store.contains(&999, &0, &0, &0, &0));
        for &(f, v, p, m, fp) in &want {
            assert!(!store.contains(&f, &v, &p, &m, &(fp + 1_000_000)));
        }

        // `none`: () -> every row, serially and in parallel.
        let view = CViewNone(&store);
        let mut got: Vec<_> = RelIndexRead::index_get(&view, &())
            .unwrap()
            .map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`none` view");
        let mut got: Vec<Row> = CRelIndexRead::c_index_get(&view, &())
            .unwrap()
            .map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`none` c_index_get");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .flat_map(|((), vals)| vals.map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`none` c_iter_all");

        // `0_1`: (F,V) -> (P,M,Fp).
        let view = CView01(&store);
        for &(f, v, ..) in &want {
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.1 == v)
                .collect();
            let mut got: Vec<_> = view
                .index_get(&(f, v))
                .unwrap()
                .map(|(p, m, fp)| (f, v, *p, *m, *fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_1` view at {:?}", (f, v));
            let mut got: Vec<Row> = view
                .c_index_get(&(f, v))
                .unwrap()
                .map(|(p, m, fp)| (f, v, *p, *m, *fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_1` c_index_get at {:?}", (f, v));
        }
        assert!(RelIndexRead::index_get(&view, &(999, 0)).is_none());
        assert!(CRelIndexRead::c_index_get(&view, &(999, 0)).is_none());
        let mut got: Vec<Row> = view
            .c_iter_all()
            .flat_map(|((f, v), vals)| {
                let (f, v) = (*f, *v);
                vals.map(move |(p, m, fp)| (f, v, *p, *m, *fp))
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_1` c_iter_all");

        // `0_1_2`: (F,V,P) -> (M,Fp). A miss must give `None`, not an empty iterator.
        let view = CView012(&store);
        for &(f, v, p, ..) in &want {
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.1 == v && r.2 == p)
                .collect();
            let mut got: Vec<_> = view
                .index_get(&(f, v, p))
                .unwrap()
                .map(|(m, fp)| (f, v, p, *m, *fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_1_2` view at {:?}", (f, v, p));
            let mut got: Vec<Row> = view
                .c_index_get(&(f, v, p))
                .unwrap()
                .map(|(m, fp)| (f, v, p, *m, *fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_1_2` c_index_get at {:?}", (f, v, p));
            assert!(
                RelIndexRead::index_get(&view, &(f, v, p + 1_000_000)).is_none(),
                "`0_1_2` must report a missing P as None"
            );
            assert!(
                CRelIndexRead::c_index_get(&view, &(f, v, p + 1_000_000)).is_none(),
                "`0_1_2` c_index_get must report a missing P as None"
            );
        }
        let mut got: Vec<_> = view
            .iter_all()
            .flat_map(|((f, v, p), vals)| vals.map(move |(m, fp)| (*f, *v, *p, *m, *fp)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_1_2` iter_all");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .flat_map(|((f, v, p), vals)| {
                let (f, v, p) = (*f, *v, *p);
                vals.map(move |(m, fp)| (f, v, p, *m, *fp))
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_1_2` c_iter_all");

        // `0_3_4`: (F,M,Fp) -> (V,P), derived by scanning through `fidx`.
        let view = CView034(&store);
        for &(f, _, _, m, fp) in &want {
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.3 == m && r.4 == fp)
                .collect();
            let mut got: Vec<_> = view
                .index_get(&(f, m, fp))
                .unwrap()
                .map(|(v, p)| (f, *v, *p, m, fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_3_4` view at {:?}", (f, m, fp));
            let mut got: Vec<Row> = view
                .c_index_get(&(f, m, fp))
                .unwrap()
                .map(|(v, p)| (f, *v, *p, m, fp))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_3_4` c_index_get at {:?}", (f, m, fp));
        }
        let mut got: Vec<_> = view
            .iter_all()
            .flat_map(|((f, m, fp), vals)| vals.map(move |(v, p)| (*f, *v, *p, *m, *fp)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_3_4` iter_all");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .flat_map(|((f, m, fp), vals)| {
                let (f, m, fp) = (*f, *m, *fp);
                vals.map(move |(v, p)| (f, *v, *p, m, fp))
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_3_4` c_iter_all");

        // The full existence index.
        let view = CViewFull(&store);
        for &(f, v, p, m, fp) in &want {
            assert!(view.contains_key(&(f, v, p, m, fp)));
        }
        let mut got: Vec<_> = RelIndexReadAll::iter_all(&view)
            .map(|((f, v, p, m, fp), _)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "full-index iter_all");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .map(|((f, v, p, m, fp), _)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "full-index c_iter_all");

        // Frozen is the read state; the store must survive a round trip back to writable.
        store.unfreeze();
        assert_eq!(store.len(), want.len(), "row count after unfreeze");
        assert_eq!(store.heap_report().leaf_elems, want.len());
    }

    /// Builds `groups` groups over two functions. Each group holds `group` leaves, spread over
    /// `paths` distinct `P`.
    fn rows(groups: usize, group: usize, paths: usize) -> Vec<Row> {
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
        // Inserting a row twice must change nothing.
        let mut dup = rows(2, 4, 2);
        dup.extend_from_slice(&dup.clone());
        check_views(&dup);
        // Straddle the small -> Swiss transition inside a group.
        for group in [
            SMALL_THRESHOLD - 1,
            SMALL_THRESHOLD,
            SMALL_THRESHOLD + 1,
            SMALL_THRESHOLD * 3,
        ] {
            check_views(&rows(2, group, 3));
        }
    }

    /// `absorb` is the delta->total merge. It has to keep `len`, `fidx`, and the groups consistent,
    /// whichever representation the two sides are in.
    #[test]
    fn absorb_unions_the_stores() {
        for (a_group, b_group) in [
            (2usize, 3usize),
            (SMALL_THRESHOLD, 2),
            (2, SMALL_THRESHOLD + 1),
            (SMALL_THRESHOLD + 1, SMALL_THRESHOLD + 1),
        ] {
            let a_rows = rows(4, a_group, 2);
            // Keys that overlap `a`'s but carry different leaves, plus one key `a` lacks.
            let b_rows: Vec<_> = rows(6, b_group, 2)
                .into_iter()
                .map(|(f, v, p, m, fp)| (f, v, p, m, fp + 7))
                .collect();

            let mut total = build(&a_rows);
            let mut delta = build(&b_rows);
            total.absorb(&mut delta);

            assert_eq!(delta.len(), 0, "absorbed delta must be empty");
            assert_eq!(delta.fidx.len(), 0, "absorbed delta must drop its fidx");
            assert_eq!(delta.fwd.len(), 0, "absorbed delta must drop its groups");

            let mut want: Vec<_> = a_rows.iter().chain(b_rows.iter()).copied().collect();
            want.sort_unstable();
            want.dedup();
            assert_eq!(total.len(), want.len(), "union row count");

            let report = total.heap_report();
            assert_eq!(report.rows, total.len());
            assert_eq!(report.leaf_elems, total.len(), "heap_report leaf count");
            assert_eq!(
                report.fidx_vs, report.fv_groups,
                "fidx must hold exactly one V per group"
            );

            total.freeze();
            for &(f, v, p, m, fp) in &want {
                assert!(total.contains(&f, &v, &p, &m, &fp));
            }
            // `fidx` must list the V of every group, or `0_3_4` can no longer find them.
            for ((f, v), _) in total.fwd.frozen().iter() {
                assert!(
                    total.fidx.frozen().get(f).is_some_and(|vs| vs.contains(v)),
                    "fidx missing {v} of function {f}"
                );
            }
        }
    }

    /// The property the whole design rests on: under concurrency, `insert_if_not_present` returns
    /// `true` exactly once per distinct row. Ascent pushes the physical row and sets `changed` on
    /// that `true`, so a duplicate `true` would inflate the row count and break semi-naive
    /// evaluation, and a missing one would drop the row.
    #[test]
    fn concurrent_inserts_have_exactly_one_winner() {
        use rayon::prelude::*;

        // Keys chosen so threads collide: few groups, many overlapping leaves, and group sizes
        // that straddle the Swiss promotion.
        let base: Vec<Row> = rows(8, SMALL_THRESHOLD + 5, 3);
        // Every row is offered by 4 different "threads' worth" of work, in different orders.
        let batches: Vec<Vec<Row>> = (0..4)
            .map(|k| {
                let mut b = base.clone();
                let n = b.len().max(1);
                b.rotate_left(k * 37 % n);
                b
            })
            .collect();

        let store = Store::default();
        let wins: usize = batches
            .par_iter()
            .map(|batch| {
                batch
                    .par_iter()
                    .filter(|row| {
                        CRelFullIndexWrite::insert_if_not_present(&CViewFull(&store), row, ())
                    })
                    .count()
            })
            .sum();

        let mut want = base.clone();
        want.sort_unstable();
        want.dedup();
        assert_eq!(wins, want.len(), "exactly one winner per distinct row");
        assert_eq!(store.len(), want.len(), "store row count");

        // The same rows through the serial store must give the same contents.
        let mut serial =
            super::super::locals_trie::LocalsIndCommon::<u32, u64, u64, i16, u64>::default();
        for row in &base {
            RelFullIndexWrite::insert_if_not_present(
                &mut super::super::locals_trie::ToFull::default().to_rel_index_write(&mut serial),
                row,
                (),
            );
        }
        assert_eq!(store.len(), serial.len(), "par and ser row counts agree");

        // `fwd` and `fidx` are maintained under two independent locks, and a racing insert on a
        // brand-new group releases the `fwd` shard before touching `fidx`. So the lockstep
        // invariant — exactly one V in `fidx` per `(F,V)` group — is the thing concurrency could
        // plausibly break, and `0_3_4` silently returns nothing for a V that goes missing.
        let report = store.heap_report();
        assert_eq!(
            report.fidx_vs, report.fv_groups,
            "fidx must hold exactly one V per group after concurrent inserts"
        );

        let mut store = store;
        store.freeze();
        for ((f, v), _) in store.fwd.frozen().iter() {
            assert!(
                store.fidx.frozen().get(f).is_some_and(|vs| vs.contains(v)),
                "fidx missing {v} of function {f}"
            );
        }
        let mut got: Vec<Row> = CViewNone(&store)
            .c_index_get(&())
            .unwrap()
            .map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "concurrent store contents");

        // Every `0_3_4` probe must find its rows, which is what the `fidx` invariant buys.
        let view = CView034(&store);
        for &(f, v, p, m, fp) in &want {
            let hits: Vec<_> = view
                .index_get(&(f, m, fp))
                .unwrap()
                .map(|(vv, pp)| (*vv, *pp))
                .collect();
            assert!(
                hits.contains(&(v, p)),
                "`0_3_4` lost {:?}",
                (f, v, p, m, fp)
            );
        }
    }
}
