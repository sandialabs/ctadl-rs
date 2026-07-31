//! A prefix-sharing (trie-like) BYODS data structure for the `assign_like` relation.
//!
//! `assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path)` is, after `locals`, the
//! other dominant memory consumer of the index phase. As a normal Ascent relation it is stored
//! ~3× over (see `cargo expand`): the physical `Vec`, the full existence index
//! `0_1_2_3_4` (whole tuple replicated as the hash key), and the probe index `0_3`
//! (`(F, src-var) -> [(dst-var, dst-path, src-path)]`, value columns stored inline). Every read
//! of `assign_like` in the ruleset binds exactly `{col0, col3}` = `(FunctionId, src-var)` — so a
//! single probe view suffices and the whole 3× replication is pure overhead.
//!
//! This module replaces all of that with one shared store (the Ascent BYODS "ind_common"):
//! a two-level map `fwd: (F, Vsrc) -> {(Vdst, Pdst, Psrc)}`. Each column is stored once. The
//! store serves both logical indices as lightweight *views*:
//!   - `0_3`: point-probe `(F, Vsrc)` -> iterate its leaf set.
//!   - full `0_1_2_3_4`: existence/dedup — look up `(F, Vsrc)`, test membership of the
//!     `(Vdst, Pdst, Psrc)` leaf. Also serves whole-relation iteration by walking the store.
//!
//! This is the same design as `locals_trie`, but simpler: `assign_like` needs no `P`-level
//! sub-trie, no inverse view, and no `fidx` side-index (`locals` needed all three).
//!
//! Correctness note (identical to `locals_trie`): a plain Ascent relation would double-merge the
//! store (once via the ind_common, once via the index write targets) and corrupt semi-naive
//! evaluation. We therefore make the *only* real merge live on the ind_common ([`AssignTrie`]);
//! every index write target has a no-op `RelIndexMerge`. Inserts enter the store through the full
//! index's write target ([`FullWrite`]); the `0_3` view is a [`NoopWrite`] because the data is
//! already present in the shared store.
//!
//! Unlike `locals` (a derived relation whose only post-run consumer is `.len()`), `assign_like`
//! is a *saved output*. The physical `Vec` is a [`CountingVec`] that stores no tuples, so the
//! result Vec is reconstructed from the store after the run via [`AssignTrie::to_vec`].

use std::cell::{Cell, RefCell};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::Index;

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};

use crate::index_engine::locals_trie::hb_bytes;
// Reuse the shared helpers from `locals_trie`: the clone-able boxed iterator and the no-op write
// target for view indices.
use crate::index_engine::locals_trie::{DynIter, NoopWrite};

type Map<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;

// ---------------------------------------------------------------------------
// Physical `rel!` storage.
//
// Unlike `locals` (a derived relation whose physical store is a pure `CountingVec` holding no
// tuples), `assign_like` is *seeded* via a `= SeedVec::from(..)` initializer with the original
// program assignments — ~94% of the final relation. Ascent's seed path is: assign the physical
// store, then, exactly once during index build, iterate it and `index_insert` each tuple into the
// indices (which, for this BYODS provider, routes into the shared trie via the full index's write
// target). `SeedVec` exploits that single pass: `iter()` *drains* the seed out, so after index
// build the seed lives only in the trie — never in a second full-size buffer. Every subsequent
// derived-row `push` is discarded (the row is already in the trie); only the row count is kept so
// `prog.assign_like.len()` and Ascent's synthetic row indices stay well-defined.
// ---------------------------------------------------------------------------
pub struct SeedVec<T> {
    /// The seed tuples, drained into the trie on the single index-build `iter()` pass. Interior
    /// mutability because that pass borrows the physical store as `&self`.
    seed: RefCell<Vec<T>>,
    /// Logical row count (seed size + discarded pushes). Read via `&self` (`len`), bumped via
    /// `&mut self` (`push`), so a plain field suffices.
    count: usize,
    /// Set the first time `iter()` runs, to catch a future Ascent codegen change that would call
    /// it twice (the second call would silently see an already-drained, empty seed).
    drained: Cell<bool>,
}
impl<T> Default for SeedVec<T> {
    fn default() -> Self {
        Self {
            seed: RefCell::new(Vec::new()),
            count: 0,
            drained: Cell::new(false),
        }
    }
}
impl<T> From<Vec<T>> for SeedVec<T> {
    fn from(v: Vec<T>) -> Self {
        let count = v.len();
        Self {
            seed: RefCell::new(v),
            count,
            drained: Cell::new(false),
        }
    }
}
impl<T> SeedVec<T> {
    #[inline(always)]
    pub fn push(&mut self, _v: T) {
        self.count += 1;
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.count
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Drain the seed. Ascent calls this exactly once, during index build (before any `push`), to
    /// route the seed into the trie via the full index's write target; draining ensures the seed
    /// is not retained for the rest of the run.
    #[inline]
    pub fn iter(&self) -> std::vec::IntoIter<T> {
        debug_assert!(
            !self.drained.replace(true),
            "assign_like_trie::SeedVec::iter called more than once; the seed would be lost"
        );
        std::mem::take(&mut *self.seed.borrow_mut()).into_iter()
    }
}
impl<T> Index<usize> for SeedVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("assign_like_trie::SeedVec is not randomly indexable")
    }
}

// ---------------------------------------------------------------------------
// The shared store (Ascent's `ind_common`).
//
// Type params name the tuple columns by role:
//   F  = FunctionId (col 0)      Vd = dst var  (col 1)     Pd = dst path (col 2)
//   Vs = src var    (col 3)      Ps = src path (col 4)
// The probe key is (F, Vs) = {col0, col3}; the leaf is (Vd, Pd, Ps) = {col1, col2, col4}.
// ---------------------------------------------------------------------------
pub struct AssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    /// forward: (F, Vsrc) -> leaves (Vdst, Pdst, Psrc). Serves the `0_3` probe and, by membership
    /// scan / full walk, the `0_1_2_3_4` existence index.
    ///
    /// The leaf container is a `Vec` (dedup by linear scan), NOT a `HashSet`: `assign_like` keyed
    /// on `(F, Vsrc)` fans out to <2 leaves per group on average (measured 1.85 on binary
    /// targets), so a whole hashbrown table per group (min ~216 B for 1-2 elements) costs far more
    /// than the full-tuple index it is meant to replace — a `HashSet`-leaf trie measured *larger*
    /// than Ascent's default storage. A `Vec` stores singleton/tiny groups in a few dozen bytes.
    /// Dedup is O(group size); groups are tiny, so this is cheap in practice (guarded by the
    /// `WARN`-logged max-group-size in [`AssignTrie::heap_report`]).
    fwd: Map<(F, Vs), Vec<(Vd, Pd, Ps)>>,
    len: usize,
}

impl<F, Vd, Pd, Vs, Ps> AssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
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
    fn contains(&self, f: &F, vd: &Vd, pd: &Pd, vs: &Vs, ps: &Ps) -> bool {
        self.fwd
            .get(&(f.clone(), vs.clone()))
            .is_some_and(|leaves| {
                let probe = (vd.clone(), pd.clone(), ps.clone());
                leaves.contains(&probe)
            })
    }

    /// Insert a full tuple `(F, Vd, Pd, Vs, Ps)`; returns true if newly added to *this* store.
    fn insert(&mut self, key: &(F, Vd, Pd, Vs, Ps)) -> bool {
        let (f, vd, pd, vs, ps) = key;
        let leaves = self.fwd.entry((f.clone(), vs.clone())).or_default();
        let leaf = (vd.clone(), pd.clone(), ps.clone());
        if leaves.contains(&leaf) {
            false
        } else {
            leaves.push(leaf);
            self.len += 1;
            true
        }
    }

    /// Merge `from` into `self` (union). Used by the delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        for (key, leaves) in from.fwd.drain() {
            let dst = self.fwd.entry(key).or_default();
            for leaf in leaves {
                if !dst.contains(&leaf) {
                    dst.push(leaf);
                    self.len += 1;
                }
            }
        }
        from.len = 0;
    }

    /// Estimate the heap bytes held by the store, and what the default Ascent storage (physical
    /// `Vec` + full existence index + `0_3` probe index) would hold for the same rows, so the
    /// saving is visible without an external profiler. Estimates include hashbrown load-factor
    /// slack and are for relative comparison, not exact accounting. One pass over the store.
    pub fn heap_report(&self) -> String {
        let sz_key = std::mem::size_of::<(F, Vs)>();
        let sz_leaf = std::mem::size_of::<(Vd, Pd, Ps)>();
        let sz_full = std::mem::size_of::<(F, Vd, Pd, Vs, Ps)>();

        // Actual trie: outer map (F,Vs) -> Vec, plus each leaf Vec's heap allocation. Also track
        // the largest group, since leaf dedup is O(group size) — a hot group would flag O(N^2).
        let mut trie = hb_bytes(
            self.fwd.capacity(),
            sz_key + std::mem::size_of::<Vec<(Vd, Pd, Ps)>>(),
        );
        let mut max_group = 0usize;
        for leaves in self.fwd.values() {
            trie += leaves.capacity() * sz_leaf;
            max_group = max_group.max(leaves.len());
        }
        if max_group > 4096 {
            log::warn!(
                "assign_like_trie: largest (F,Vsrc) group has {} leaves; linear leaf dedup is \
                 O(group size) and may be slow for such groups",
                max_group
            );
        }
        // Default equivalent for the same `len` rows: physical Vec (sz_full each) + full index
        // (RelFullIndexType ~= HashMap<full tuple, ()>, full tuple as key) + `0_3` index
        // (HashMap<(F,Vs), Vec<(Vd,Pd,Ps)>>, value columns inline). Approximated at load factor.
        let n = self.len;
        let default_vec = n * sz_full;
        let default_full = hb_bytes(n, sz_full);
        let default_03 = hb_bytes(
            self.fwd.len(),
            sz_key + std::mem::size_of::<Vec<(Vd, Pd, Ps)>>(),
        ) + n * sz_leaf;
        let default_total = default_vec + default_full + default_03;
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        format!(
            "assign_like store estimate: trie {:.1} MB over {} rows ({} (F,Vsrc) groups, max group \
             {}) | default equiv ~{:.1} MB (Vec {:.1} + full {:.1} + 0_3 {:.1}) | saving ~{:.1} MB \
             | elem sizes key={} leaf={} full={} B",
            mb(trie),
            n,
            self.fwd.len(),
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

    /// Reconstruct the logical relation as full tuples `(F, Vd, Pd, Vs, Ps)`, consuming the store.
    /// Used to materialize the saved `assign_like` output after the run (the physical relation is a
    /// [`SeedVec`] holding no tuples). Draining as it goes frees each group's storage while the
    /// output Vec fills, so the reconstruction transient is ~1× (this is the run's peak — the final
    /// assembly — so avoiding a second full-relation copy here directly lowers peak footprint).
    pub fn into_vec(mut self) -> Vec<(F, Vd, Pd, Vs, Ps)> {
        let mut out = Vec::with_capacity(self.len);
        for ((f, vs), leaves) in self.fwd.drain() {
            for (vd, pd, ps) in leaves {
                out.push((f.clone(), vd.clone(), pd, vs.clone(), ps));
            }
        }
        out
    }
}

impl<F, Vd, Pd, Vs, Ps> Default for AssignTrie<F, Vd, Pd, Vs, Ps>
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            fwd: Map::default(),
            len: 0,
        }
    }
}

impl<F, Vd, Pd, Vs, Ps> Clone for AssignTrie<F, Vd, Pd, Vs, Ps>
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
            len: self.len,
        }
    }
}

// The ONE real merge lives here (on the ind_common).
impl<F, Vd, Pd, Vs, Ps> RelIndexMerge for AssignTrie<F, Vd, Pd, Vs, Ps>
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
    // default merge_delta_to_total_new_to_delta: move delta->total, swap new<->delta.
}

// ---------------------------------------------------------------------------
// Write target of the full existence index: performs the real inserts into the store, but has a
// NO-OP merge (the real merge is the ind_common's, see module docs). The `0_3` view uses the
// shared `NoopWrite` from `locals_trie`.
// ---------------------------------------------------------------------------
pub struct FullWrite<'a, F, Vd, Pd, Vs, Ps>(&'a mut AssignTrie<F, Vd, Pd, Vs, Ps>)
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash;

impl<'a, F, Vd, Pd, Vs, Ps> RelFullIndexWrite for FullWrite<'a, F, Vd, Pd, Vs, Ps>
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
        self.0.insert(key)
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexWrite for FullWrite<'a, F, Vd, Pd, Vs, Ps>
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
        self.0.insert(&key);
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexMerge for FullWrite<'a, F, Vd, Pd, Vs, Ps>
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
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

// ---------------------------------------------------------------------------
// Read views + their ToRelIndex markers.
// ---------------------------------------------------------------------------

/// Defines a zero-sized `ToRelIndex` marker over the concrete `AssignTrie` store.
/// `$wrty`/`$wrbody` give the write target (real for the full index, no-op for the `0_3` view).
macro_rules! marker {
    ($to:ident, $view:ident, $wrty:ty, $rel:ident => $wrbody:expr) => {
        pub struct $to<F, Vd, Pd, Vs, Ps>(PhantomData<(F, Vd, Pd, Vs, Ps)>);
        impl<F, Vd, Pd, Vs, Ps> Default for $to<F, Vd, Pd, Vs, Ps> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<F, Vd, Pd, Vs, Ps> ToRelIndex<AssignTrie<F, Vd, Pd, Vs, Ps>> for $to<F, Vd, Pd, Vs, Ps>
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
                AssignTrie<F, Vd, Pd, Vs, Ps>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a AssignTrie<F, Vd, Pd, Vs, Ps>,
            ) -> Self::RelIndex<'a> {
                $view(rel)
            }
            type RelIndexWrite<'a>
                = $wrty
            where
                Self: 'a,
                AssignTrie<F, Vd, Pd, Vs, Ps>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                $rel: &'a mut AssignTrie<F, Vd, Pd, Vs, Ps>,
            ) -> Self::RelIndexWrite<'a> {
                $wrbody
            }
        }
    };
}

marker!(To03, View03, NoopWrite<(F, Vs), (Vd, Pd, Ps)>, _rel => NoopWrite::default());
marker!(ToFull, ViewFull, FullWrite<'a, F, Vd, Pd, Vs, Ps>, rel => FullWrite(rel));

pub struct View03<'a, F, Vd, Pd, Vs, Ps>(&'a AssignTrie<F, Vd, Pd, Vs, Ps>)
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash;
pub struct ViewFull<'a, F, Vd, Pd, Vs, Ps>(&'a AssignTrie<F, Vd, Pd, Vs, Ps>)
where
    F: Clone + Eq + Hash,
    Vd: Clone + Eq + Hash,
    Pd: Clone + Eq + Hash,
    Vs: Clone + Eq + Hash,
    Ps: Clone + Eq + Hash;

// ---- 0_3: (F, Vsrc) -> (Vdst, Pdst, Psrc) ---------------------------------
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexRead<'a> for View03<'a, F, Vd, Pd, Vs, Ps>
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
        let leaves = self.0.fwd.get(key)?;
        Some(DynIter::new(move || {
            leaves.iter().map(|(vd, pd, ps)| (vd, pd, ps))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len()
    }
}
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexReadAll<'a> for View03<'a, F, Vd, Pd, Vs, Ps>
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
        Box::new(self.0.fwd.iter().map(|((f, vs), leaves)| {
            let it = DynIter::new(move || leaves.iter().map(|(vd, pd, ps)| (vd, pd, ps)));
            ((f, vs), it)
        }))
    }
}

// ---- full 0_1_2_3_4: existence ---------------------------------------------
impl<'a, F, Vd, Pd, Vs, Ps> RelFullIndexRead<'a> for ViewFull<'a, F, Vd, Pd, Vs, Ps>
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
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexRead<'a> for ViewFull<'a, F, Vd, Pd, Vs, Ps>
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
impl<'a, F, Vd, Pd, Vs, Ps> RelIndexReadAll<'a> for ViewFull<'a, F, Vd, Pd, Vs, Ps>
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
        Box::new(self.0.fwd.iter().flat_map(|((f, vs), leaves)| {
            leaves
                .iter()
                .map(move |(vd, pd, ps)| ((f, vd, pd, vs, ps), std::iter::once(&())))
        }))
    }
}

// ---------------------------------------------------------------------------
// The BYODS provider macros (see `locals_trie` for the protocol).
// ---------------------------------------------------------------------------

#[doc(hidden)]
#[macro_export]
macro_rules! assign_like_trie_rel_codegen {
    ($($tt:tt)*) => {};
}
pub use assign_like_trie_rel_codegen as rel_codegen;

#[doc(hidden)]
#[macro_export]
macro_rules! assign_like_trie_rel {
    ($name:ident, ($f:ty, $vd:ty, $pd:ty, $vs:ty, $ps:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::assign_like_trie::SeedVec<($f, $vd, $pd, $vs, $ps)>
    };
}
pub use assign_like_trie_rel as rel;

#[doc(hidden)]
#[macro_export]
macro_rules! assign_like_trie_rel_ind_common {
    ($name:ident, ($f:ty, $vd:ty, $pd:ty, $vs:ty, $ps:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::assign_like_trie::AssignTrie<$f, $vd, $pd, $vs, $ps>
    };
}
pub use assign_like_trie_rel_ind_common as rel_ind_common;

#[doc(hidden)]
#[macro_export]
macro_rules! assign_like_trie_rel_full_ind {
    ($name:ident, ($f:ty, $vd:ty, $pd:ty, $vs:ty, $ps:ty), $inds:tt, $par:ident, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::assign_like_trie::ToFull<$f, $vd, $pd, $vs, $ps>
    };
}
pub use assign_like_trie_rel_full_ind as rel_full_ind;

#[doc(hidden)]
#[macro_export]
macro_rules! assign_like_trie_rel_ind {
    ($name:ident, ($f:ty, $vd:ty, $pd:ty, $vs:ty, $ps:ty), $inds:tt, $par:ident, $args:tt, [0, 3], $key:ty, $val:ty) => {
        $crate::index_engine::assign_like_trie::To03<$f, $vd, $pd, $vs, $ps>
    };
}
pub use assign_like_trie_rel_ind as rel_ind;
