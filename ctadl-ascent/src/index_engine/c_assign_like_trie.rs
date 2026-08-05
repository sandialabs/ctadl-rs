//! The parallel twin of [`super::assign_like_trie`], for `ascent_par!`.
//!
//! Same shape as [`super::c_locals_trie`], one level simpler. The store is a single forward map
//! `(F, Vsrc) -> [(Vdst, Pdst, Psrc)]`, which serves both logical indices as views: `0_3`
//! point-probes `(F, Vsrc)`, and the full `0_1_2_3_4` index handles existence, dedup, and
//! whole-relation iteration. The outer `hashbrown::HashMap` becomes a `DashMap` so rayon threads
//! can insert concurrently; the leaves stay a `Vec`, deduped by linear scan, for the reason
//! [`super::assign_like_trie`] gives: keyed on `(F, Vsrc)` the relation fans out to fewer than two
//! leaves per group, and a whole hash table per group would cost more than the index it replaces.
//!
//! Correctness note, same as both serial modules: the *only* real `RelIndexMerge` lives on the
//! ind_common ([`CAssignTrie`]), so no row is merged twice per iteration.
//!
//! ## Seeding, and why the physical relation holds nothing
//!
//! `assign_like` is seeded with the original program assignments — about 94% of the final relation
//! on binary targets. Serially, [`super::assign_like_trie::SeedVec`] exploits the fact that Ascent
//! iterates the physical relation exactly once during index build: `iter()` *drains* the seed into
//! the trie, so the seed is never held in a second full-size buffer.
//!
//! Parallel codegen builds indices differently. It does not iterate; it runs
//! `(0..rel.len()).into_par_iter()` and *random-accesses* `rel[i]`, which a draining store cannot
//! serve. So the seed goes into the store directly, before the run, through
//! [`CAssignTrie::from_rows`] — which consumes the input `Vec` and frees it, giving exactly the
//! property `SeedVec` was written for. The physical relation is then a pure counter
//! ([`CSeedVec`]), holding no tuples and iterating nothing, so index build has nothing to do for
//! it.

use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::Index;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::c_locals_trie::{CMap, Hasher};
use super::locals_trie::{DynIter, hb_bytes};
use ascent::dashmap::ReadOnlyView;
use ascent::internal::{
    CRelFullIndexWrite, CRelIndexRead, CRelIndexReadAll, CRelIndexWrite, DashMapViewParIter,
    Freezable, RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use ascent::rayon::iter::ParallelIterator;
use ascent::rayon::iter::plumbing::UnindexedConsumer;
use ascent::rayon::prelude::IntoParallelRefIterator;

/// The leaves of one `(F, Vsrc)` group: `{col1, col2, col4}` of the tuple.
type Leaves<Vd, Pd, Ps> = Vec<(Vd, Pd, Ps)>;

// ---------------------------------------------------------------------------
// Physical `rel!` storage.
// ---------------------------------------------------------------------------

/// The parallel physical store for `assign_like`. It holds no tuples, only the row count.
///
/// See the module docs for why the seed does not live here under `ascent_par!`. Everything the
/// relation contains is in the shared [`CAssignTrie`]; this exists so Ascent's synthetic row
/// indices stay well-defined.
///
/// `len()` counts only the rows *derived* during the run, and must start at zero: parallel index
/// build walks `(0..rel.len())` and indexes the physical store, which holds no tuples to hand
/// back. The relation's true size is [`CAssignTrie::len`].
pub struct CSeedVec<T> {
    count: AtomicUsize,
    _p: PhantomData<T>,
}
impl<T> Default for CSeedVec<T> {
    fn default() -> Self {
        Self {
            count: AtomicUsize::new(0),
            _p: PhantomData,
        }
    }
}
impl<T> CSeedVec<T> {
    /// Count one row and return its index. Ascent calls this once per newly inserted row.
    #[inline(always)]
    pub fn push(&self, _v: T) -> usize {
        self.count.fetch_add(1, Ordering::Relaxed)
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Empty: index build has nothing to route, because the seed is already in the store.
    #[inline(always)]
    pub fn iter(&self) -> std::iter::Empty<&T> {
        std::iter::empty()
    }
}
impl<T> Index<usize> for CSeedVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("c_assign_like_trie::CSeedVec stores no tuples")
    }
}

// ---------------------------------------------------------------------------
// The shared store, which is Ascent's `ind_common`.
//
// Column roles: F = FunctionId (col 0), Vd = dst var (col 1), Pd = dst path (col 2),
// Vs = src var (col 3), Ps = src path (col 4). The probe key is (F, Vs); the leaf is (Vd, Pd, Ps).
// ---------------------------------------------------------------------------

/// The concurrent `assign_like` store. See the module docs.
pub struct CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fwd: CMap<(F, Vs), Leaves<Vd, Pd, Ps>>,
    len: AtomicUsize,
}

impl<F, Vd, Pd, Vs, Ps> Default for CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            fwd: CMap::default(),
            len: AtomicUsize::new(0),
        }
    }
}

impl<F, Vd, Pd, Vs, Ps> Clone for CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        Self {
            fwd: self.fwd.clone(),
            len: AtomicUsize::new(self.len()),
        }
    }
}

impl<F, Vd, Pd, Vs, Ps> Freezable for CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn freeze(&mut self) {
        self.fwd.freeze();
    }
    fn unfreeze(&mut self) {
        self.fwd.unfreeze();
    }
}

impl<F, Vd, Pd, Vs, Ps> CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Build a store from the seed rows, consuming them. See the module docs: this is what
    /// replaces `SeedVec`'s draining index-build pass under `ascent_par!`, and it keeps the same
    /// property — the rows end up in the store and nowhere else.
    pub fn from_rows(rows: Vec<(F, Vd, Pd, Vs, Ps)>) -> Self {
        let store = Self::default();
        for row in rows {
            store.c_insert(&row);
        }
        store
    }

    /// Existence probe against the **frozen** store.
    #[inline]
    fn contains(&self, f: &F, vd: &Vd, pd: &Pd, vs: &Vs, ps: &Ps) -> bool {
        self.fwd
            .frozen()
            .get(&(f.clone(), vs.clone()))
            .is_some_and(|leaves| leaves.contains(&(vd.clone(), pd.clone(), ps.clone())))
    }

    /// Insert a full tuple through a **shared** reference. Returns true if it was new to *this*
    /// store, and true for exactly one caller when several race on the same tuple: the `(f, vs)`
    /// entry is one shard write lock, held across the leaf scan and push.
    fn c_insert(&self, key: &(F, Vd, Pd, Vs, Ps)) -> bool {
        let (f, vd, pd, vs, ps) = key;
        let leaf = (vd.clone(), pd.clone(), ps.clone());
        let mut leaves = self
            .fwd
            .unfrozen()
            .entry((f.clone(), vs.clone()))
            .or_default();
        if leaves.contains(&leaf) {
            false
        } else {
            leaves.push(leaf);
            drop(leaves);
            self.len.fetch_add(1, Ordering::Relaxed);
            true
        }
    }

    /// Merge `from` into `self`, taking the union. Ascent runs this single-threaded on `&mut`,
    /// with both sides unfrozen.
    fn absorb(&mut self, from: &mut Self) {
        let mut added = 0usize;
        {
            let fwd = self.fwd.unfrozen();
            // Drain `from`'s shard tables in place rather than replacing the map: `from` becomes
            // the next `new` store, and a fresh `DashMap` would allocate a whole shard array on
            // every fixpoint iteration. See `c_locals_trie::CLocalsIndCommon::absorb`.
            for shard in from.fwd.unfrozen_mut().shards_mut() {
                for (key, leaves) in shard.get_mut().drain() {
                    let mut dst = fwd.entry(key).or_default();
                    for leaf in leaves.into_inner() {
                        if !dst.contains(&leaf) {
                            dst.push(leaf);
                            added += 1;
                        }
                    }
                }
            }
        }
        *self.len.get_mut() += added;
        *from.len.get_mut() = 0;
    }

    /// Estimate the heap bytes the store holds, alongside what the default Ascent storage would
    /// hold for the same rows. Same report as the serial store, with the outer-map figure summed
    /// over `DashMap`'s shard tables. Runs in either freeze state.
    pub fn heap_report(&self) -> String {
        let sz_key = std::mem::size_of::<(F, Vs)>();
        let sz_leaf = std::mem::size_of::<(Vd, Pd, Ps)>();
        let sz_full = std::mem::size_of::<(F, Vd, Pd, Vs, Ps)>();
        let sz_outer = sz_key + std::mem::size_of::<Leaves<Vd, Pd, Ps>>();

        let mut trie = self.fwd.table_bytes(sz_outer);
        let mut max_group = 0usize;
        // The closure borrows `trie`/`max_group` mutably; scope it so they are readable after.
        {
            let mut visit = |leaves: &Leaves<Vd, Pd, Ps>| {
                trie += leaves.capacity() * sz_leaf;
                max_group = max_group.max(leaves.len());
            };
            match &self.fwd {
                CMap::Frozen(v) => v.iter().for_each(|(_, leaves)| visit(leaves)),
                CMap::Unfrozen(dm) => dm.iter().for_each(|e| visit(e.value())),
            }
        }
        if max_group > 4096 {
            log::warn!(
                "c_assign_like_trie: largest (F,Vsrc) group has {} leaves; linear leaf dedup is \
                 O(group size) and may be slow for such groups",
                max_group
            );
        }
        let n = self.len();
        let groups = self.fwd.len();
        let default_vec = n * sz_full;
        let default_full = hb_bytes(n, sz_full);
        let default_03 = hb_bytes(groups, sz_outer) + n * sz_leaf;
        let default_total = default_vec + default_full + default_03;
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        format!(
            "assign_like store estimate: trie {:.1} MB over {} rows ({} (F,Vsrc) groups, max group \
             {}) | default equiv ~{:.1} MB (Vec {:.1} + full {:.1} + 0_3 {:.1}) | saving ~{:.1} MB \
             | elem sizes key={} leaf={} full={} B",
            mb(trie),
            n,
            groups,
            max_group,
            mb(default_total),
            mb(default_vec),
            mb(default_full),
            mb(default_03),
            mb(default_total.saturating_sub(trie)),
            sz_key,
            sz_leaf,
            sz_full,
        )
    }

    /// Rebuild the logical relation as full tuples, consuming the store.
    ///
    /// This is the saved `assign_like` output; the physical relation holds no tuples. Draining as
    /// it goes frees each group while the output `Vec` fills, so the transient stays at about one
    /// copy of the relation — and this final assembly is the run's peak.
    pub fn into_vec(mut self) -> Vec<(F, Vd, Pd, Vs, Ps)> {
        let n = self.len();
        // A run can leave the store frozen (the last SCC to mention `assign_like` may use it
        // body-only, and that path freezes without unfreezing), so put it back in map form first.
        self.fwd.unfreeze();
        let fwd = std::mem::take(&mut self.fwd);
        let CMap::Unfrozen(fwd) = fwd else {
            unreachable!("just unfrozen")
        };
        let mut out = Vec::with_capacity(n);
        for ((f, vs), leaves) in fwd.into_iter() {
            for (vd, pd, ps) in leaves {
                out.push((f.clone(), vd, pd, vs.clone(), ps));
            }
        }
        out
    }
}

// The one real merge lives here, on the ind_common.
impl<F, Vd, Pd, Vs, Ps> RelIndexMerge for CAssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.absorb(from);
    }
}

// ---------------------------------------------------------------------------
// Parallel iterator adaptors.
// ---------------------------------------------------------------------------

/// A group's leaves as `(&Vd, &Pd, &Ps)`, split over the leaf slice. Leaves live in a `Vec`, so
/// this needs no intermediate allocation.
type LeafParIter<'a, Vd, Pd, Ps> = ascent::rayon::iter::Map<
    ascent::rayon::slice::Iter<'a, (Vd, Pd, Ps)>,
    for<'x> fn(&'x (Vd, Pd, Ps)) -> (&'x Vd, &'x Pd, &'x Ps),
>;

#[inline]
fn split_leaf<Vd, Pd, Ps>((vd, pd, ps): &(Vd, Pd, Ps)) -> (&Vd, &Pd, &Ps) {
    (vd, pd, ps)
}

#[inline]
fn leaf_par_iter<Vd, Pd, Ps>(leaves: &[(Vd, Pd, Ps)]) -> LeafParIter<'_, Vd, Pd, Ps>
where
    Vd: Sync,
    Pd: Sync,
    Ps: Sync,
{
    leaves.par_iter().map(split_leaf as _)
}

/// `iter_all` over `0_3`: one key per `(F, Vsrc)` group, with that group's leaves.
pub struct CView03AllParIter<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, Vs), Leaves<Vd, Pd, Ps>, Hasher>,
}

impl<'a, F, Vd, Pd, Vs, Ps> ParallelIterator for CView03AllParIter<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash + Send + Sync,
    Vd: Clone + Eq + Hash + Send + Sync,
    Pd: Clone + Eq + Hash + Send + Sync,
    Vs: Clone + Eq + Hash + Send + Sync,
    Ps: Clone + Eq + Hash + Send + Sync,
{
    type Item = ((&'a F, &'a Vs), LeafParIter<'a, Vd, Pd, Ps>);

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .map(|((f, vs), leaves)| ((f, vs), leaf_par_iter(leaves)))
            .drive_unindexed(consumer)
    }
}

/// `iter_all` over the full existence index: every row keyed by itself.
pub struct CFullAllParIter<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
{
    fwd: &'a ReadOnlyView<(F, Vs), Leaves<Vd, Pd, Ps>, Hasher>,
}

impl<'a, F, Vd, Pd, Vs, Ps> ParallelIterator for CFullAllParIter<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash + Send + Sync,
    Vd: Clone + Eq + Hash + Send + Sync,
    Pd: Clone + Eq + Hash + Send + Sync,
    Vs: Clone + Eq + Hash + Send + Sync,
    Ps: Clone + Eq + Hash + Send + Sync,
{
    type Item = (
        (&'a F, &'a Vd, &'a Pd, &'a Vs, &'a Ps),
        ascent::rayon::iter::Once<&'a ()>,
    );

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        DashMapViewParIter::new(self.fwd)
            .flat_map_iter(|((f, vs), leaves)| {
                leaves
                    .iter()
                    .map(move |(vd, pd, ps)| ((f, vd, pd, vs, ps), ascent::rayon::iter::once(&())))
            })
            .drive_unindexed(consumer)
    }
}

// ---------------------------------------------------------------------------
// Read views and their `ToRelIndex` markers. As in `c_locals_trie`, the read view doubles as the
// write target, because the blanket `ToRelIndex0` impl routes `to_c_rel_index_write` to
// `to_rel_index`.
// ---------------------------------------------------------------------------

macro_rules! marker {
    ($to:ident, $view:ident) => {
        pub struct $to<F, Vd, Pd, Vs, Ps>(PhantomData<(F, Vd, Pd, Vs, Ps)>);
        impl<F, Vd, Pd, Vs, Ps> Default for $to<F, Vd, Pd, Vs, Ps> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<F, Vd, Pd, Vs, Ps> Freezable for $to<F, Vd, Pd, Vs, Ps> {}
        impl<F, Vd, Pd, Vs, Ps> ToRelIndex<CAssignTrie<F, Vd, Pd, Vs, Ps>>
            for $to<F, Vd, Pd, Vs, Ps>
        where
            F: Clone + Eq + Hash,
            Vd: Clone + Eq + Hash,
            Pd: Clone + Eq + Hash,
            Vs: Clone + Eq + Hash,
            Ps: Clone + Eq + Hash,
        {
            type RelIndex<'a>
                = $view<'a, F, Vd, Pd, Vs, Ps>
            where
                Self: 'a,
                CAssignTrie<F, Vd, Pd, Vs, Ps>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a CAssignTrie<F, Vd, Pd, Vs, Ps>,
            ) -> Self::RelIndex<'a> {
                $view(rel)
            }
            type RelIndexWrite<'a>
                = $view<'a, F, Vd, Pd, Vs, Ps>
            where
                Self: 'a,
                CAssignTrie<F, Vd, Pd, Vs, Ps>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                rel: &'a mut CAssignTrie<F, Vd, Pd, Vs, Ps>,
            ) -> Self::RelIndexWrite<'a> {
                $view(rel)
            }
        }

        pub struct $view<'a, F, Vd, Pd, Vs, Ps>(&'a CAssignTrie<F, Vd, Pd, Vs, Ps>)
        where
            F: Clone + Eq + Hash,
            Vd: Clone + Eq + Hash,
            Pd: Clone + Eq + Hash,
            Vs: Clone + Eq + Hash,
            Ps: Clone + Eq + Hash;

        // The real merge lives on the ind_common; a write target must never merge again.
        impl<F, Vd, Pd, Vs, Ps> RelIndexMerge for $view<'_, F, Vd, Pd, Vs, Ps>
        where
            F: Clone + Eq + Hash,
            Vd: Clone + Eq + Hash,
            Pd: Clone + Eq + Hash,
            Vs: Clone + Eq + Hash,
            Ps: Clone + Eq + Hash,
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

marker!(CTo03, CView03);
marker!(CToFull, CViewFull);

// ---- 0_3: (F, Vsrc) -> (Vdst, Pdst, Psrc) ---------------------------------
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexRead<'a> for CView03<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vs);
    type Value = (&'a Vd, &'a Pd, &'a Ps);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, Vs)) -> Option<Self::IteratorType> {
        let leaves = self.0.fwd.frozen().get(key)?;
        Some(DynIter::new(move || leaves.iter().map(split_leaf)))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len_estimate()
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexReadAll<'a> for CView03<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a Vs);
    type Value = (&'a Vd, &'a Pd, &'a Ps);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.frozen().iter().map(|((f, vs), leaves)| {
            let it = DynIter::new(move || leaves.iter().map(split_leaf));
            ((f, vs), it)
        }))
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> CRelIndexRead<'a> for CView03<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash + Send + Sync,
    Vd: Clone + Eq + Hash + Send + Sync,
    Pd: Clone + Eq + Hash + Send + Sync,
    Vs: Clone + Eq + Hash + Send + Sync,
    Ps: Clone + Eq + Hash + Send + Sync,
{
    type Key = (F, Vs);
    type Value = (&'a Vd, &'a Pd, &'a Ps);
    type IteratorType = LeafParIter<'a, Vd, Pd, Ps>;
    #[inline]
    fn c_index_get(&'a self, key: &(F, Vs)) -> Option<Self::IteratorType> {
        let leaves = self.0.fwd.frozen().get(key)?;
        Some(leaf_par_iter(leaves))
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> CRelIndexReadAll<'a> for CView03<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash + Send + Sync,
    Vd: Clone + Eq + Hash + Send + Sync,
    Pd: Clone + Eq + Hash + Send + Sync,
    Vs: Clone + Eq + Hash + Send + Sync,
    Ps: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a Vs);
    type Value = (&'a Vd, &'a Pd, &'a Ps);
    type ValueIteratorType = LeafParIter<'a, Vd, Pd, Ps>;
    type AllIteratorType = CView03AllParIter<'a, F, Vd, Pd, Vs, Ps>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CView03AllParIter {
            fwd: self.0.fwd.frozen(),
        }
    }
}
impl<F, Vd, Pd, Vs, Ps> RelIndexWrite for CView03<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vs);
    type Value = (Vd, Pd, Ps);
    #[inline(always)]
    fn index_insert(&mut self, _key: Self::Key, _value: Self::Value) {}
}
impl<F, Vd, Pd, Vs, Ps> CRelIndexWrite for CView03<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vs);
    type Value = (Vd, Pd, Ps);
    #[inline(always)]
    fn index_insert(&self, _key: Self::Key, _value: Self::Value) {}
}

// ---- full 0_1_2_3_4: existence and the one real writer ---------------------
impl<'a, F, Vd, Pd, Vs, Ps> RelFullIndexRead<'a> for CViewFull<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
    #[inline]
    fn contains_key(&'a self, key: &Self::Key) -> bool {
        self.0.contains(&key.0, &key.1, &key.2, &key.3, &key.4)
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexRead<'a> for CViewFull<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
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
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexReadAll<'a> for CViewFull<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a Vd, &'a Pd, &'a Vs, &'a Ps);
    type Value = &'a ();
    type ValueIteratorType = std::iter::Once<&'a ()>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.frozen().iter().flat_map(|((f, vs), leaves)| {
            leaves
                .iter()
                .map(move |(vd, pd, ps)| ((f, vd, pd, vs, ps), std::iter::once(&())))
        }))
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> CRelIndexRead<'a> for CViewFull<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
    type Value = &'a ();
    type IteratorType = ascent::rayon::iter::Once<&'a ()>;
    #[inline]
    fn c_index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        self.0
            .contains(&key.0, &key.1, &key.2, &key.3, &key.4)
            .then(|| ascent::rayon::iter::once(&()))
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> CRelIndexReadAll<'a> for CViewFull<'a, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash + Send + Sync,
    Vd: Clone + Eq + Hash + Send + Sync,
    Pd: Clone + Eq + Hash + Send + Sync,
    Vs: Clone + Eq + Hash + Send + Sync,
    Ps: Clone + Eq + Hash + Send + Sync,
{
    type Key = (&'a F, &'a Vd, &'a Pd, &'a Vs, &'a Ps);
    type Value = &'a ();
    type ValueIteratorType = ascent::rayon::iter::Once<&'a ()>;
    type AllIteratorType = CFullAllParIter<'a, F, Vd, Pd, Vs, Ps>;
    #[inline]
    fn c_iter_all(&'a self) -> Self::AllIteratorType {
        CFullAllParIter {
            fwd: self.0.fwd.frozen(),
        }
    }
}
impl<F, Vd, Pd, Vs, Ps> CRelFullIndexWrite for CViewFull<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
    type Value = ();
    #[inline]
    fn insert_if_not_present(&self, key: &Self::Key, _v: ()) -> bool {
        self.0.c_insert(key)
    }
}
impl<F, Vd, Pd, Vs, Ps> RelFullIndexWrite for CViewFull<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
    type Value = ();
    #[inline]
    fn insert_if_not_present(&mut self, key: &Self::Key, _v: ()) -> bool {
        self.0.c_insert(key)
    }
}
impl<F, Vd, Pd, Vs, Ps> CRelIndexWrite for CViewFull<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
    type Value = ();
    #[inline]
    fn index_insert(&self, key: Self::Key, _value: ()) {
        self.0.c_insert(&key);
    }
}
impl<F, Vd, Pd, Vs, Ps> RelIndexWrite for CViewFull<'_, F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    type Key = (F, Vd, Pd, Vs, Ps);
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

    use super::*;

    type Store = CAssignTrie<u32, u64, u64, u64, u64>;
    type Row = (u32, u64, u64, u64, u64);

    fn rows(groups: usize, group: usize) -> Vec<Row> {
        (0..groups)
            .flat_map(|g| {
                (0..group).map(move |i| {
                    (
                        (g % 3) as u32,
                        i as u64,
                        (i % 2) as u64,
                        g as u64,
                        (i / 2) as u64,
                    )
                })
            })
            .collect()
    }

    fn build(rows: &[Row]) -> Store {
        let store = Store::default();
        for row in rows {
            store.c_insert(row);
        }
        store
    }

    fn check_views(rs: &[Row]) {
        let mut store = build(rs);
        let mut want: Vec<_> = rs.to_vec();
        want.sort_unstable();
        want.dedup();
        assert_eq!(store.len(), want.len(), "row count");

        store.freeze();

        for &(f, vd, pd, vs, ps) in &want {
            assert!(store.contains(&f, &vd, &pd, &vs, &ps));
            assert!(!store.contains(&f, &vd, &pd, &vs, &(ps + 1_000_000)));
        }

        // `0_3`: (F, Vsrc) -> (Vdst, Pdst, Psrc), serially and in parallel.
        let view = CView03(&store);
        for &(f, _, _, vs, _) in &want {
            let expect: Vec<_> = want
                .iter()
                .copied()
                .filter(|r| r.0 == f && r.3 == vs)
                .collect();
            let mut got: Vec<_> = view
                .index_get(&(f, vs))
                .unwrap()
                .map(|(vd, pd, ps)| (f, *vd, *pd, vs, *ps))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_3` view at {:?}", (f, vs));
            let mut got: Vec<Row> = view
                .c_index_get(&(f, vs))
                .unwrap()
                .map(|(vd, pd, ps)| (f, *vd, *pd, vs, *ps))
                .collect();
            got.sort_unstable();
            assert_eq!(got, expect, "`0_3` c_index_get at {:?}", (f, vs));
        }
        assert!(RelIndexRead::index_get(&view, &(999, 0)).is_none());
        assert!(CRelIndexRead::c_index_get(&view, &(999, 0)).is_none());
        let mut got: Vec<_> = view
            .iter_all()
            .flat_map(|((f, vs), vals)| vals.map(move |(vd, pd, ps)| (*f, *vd, *pd, *vs, *ps)))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_3` iter_all");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .flat_map(|((f, vs), vals)| {
                let (f, vs) = (*f, *vs);
                vals.map(move |(vd, pd, ps)| (f, *vd, *pd, vs, *ps))
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`0_3` c_iter_all");

        // The full existence index.
        let view = CViewFull(&store);
        for &row in &want {
            assert!(view.contains_key(&row));
        }
        let mut got: Vec<_> = RelIndexReadAll::iter_all(&view)
            .map(|((f, vd, pd, vs, ps), _)| (*f, *vd, *pd, *vs, *ps))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "full-index iter_all");
        let mut got: Vec<Row> = view
            .c_iter_all()
            .map(|((f, vd, pd, vs, ps), _)| (*f, *vd, *pd, *vs, *ps))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "full-index c_iter_all");

        store.unfreeze();
        let mut got = store.into_vec();
        got.sort_unstable();
        assert_eq!(got, want, "into_vec");
    }

    #[test]
    fn views_agree_with_their_rows() {
        check_views(&[]);
        check_views(&rows(1, 1));
        check_views(&rows(5, 3));
        let mut dup = rows(3, 4);
        dup.extend_from_slice(&dup.clone());
        check_views(&dup);
    }

    #[test]
    fn absorb_unions_the_stores() {
        let a_rows = rows(4, 3);
        let b_rows: Vec<_> = rows(6, 3)
            .into_iter()
            .map(|(f, vd, pd, vs, ps)| (f, vd, pd, vs, ps + 7))
            .collect();

        let mut total = build(&a_rows);
        let mut delta = build(&b_rows);
        total.absorb(&mut delta);

        assert_eq!(delta.len(), 0, "absorbed delta must be empty");
        assert_eq!(delta.fwd.len(), 0, "absorbed delta must drop its groups");

        let mut want: Vec<_> = a_rows.iter().chain(b_rows.iter()).copied().collect();
        want.sort_unstable();
        want.dedup();
        assert_eq!(total.len(), want.len(), "union row count");
        let mut got = total.into_vec();
        got.sort_unstable();
        assert_eq!(got, want, "union contents");
    }

    /// `insert_if_not_present` must return `true` exactly once per distinct row under concurrency:
    /// Ascent pushes the physical row and sets `changed` on that `true`.
    #[test]
    fn concurrent_inserts_have_exactly_one_winner() {
        use rayon::prelude::*;

        let base = rows(16, 8);
        let batches: Vec<Vec<Row>> = (0..4)
            .map(|k| {
                let mut b = base.clone();
                let n = b.len().max(1);
                b.rotate_left(k * 29 % n);
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
        let mut got = store.into_vec();
        got.sort_unstable();
        assert_eq!(got, want, "concurrent store contents");
    }

    /// `from_rows` is the seeding path: it must produce the same store as inserting one by one,
    /// and dedup the seed exactly as the store would.
    #[test]
    fn from_rows_matches_one_by_one() {
        let mut seed = rows(7, 5);
        seed.extend_from_slice(&seed.clone());
        let store = Store::from_rows(seed.clone());
        let mut want = seed;
        want.sort_unstable();
        want.dedup();
        assert_eq!(store.len(), want.len());
        let mut got = store.into_vec();
        got.sort_unstable();
        assert_eq!(got, want);
    }
}
