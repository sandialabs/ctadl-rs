//! A flat, CSR-style BYODS data structure for the `locals` relation.
//!
//! `locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` uses more memory than anything
//! else in the index phase. As a normal Ascent relation it is stored about 6 times over: once
//! in the physical `Vec`, and again in each of the indices `none`, `0_1`, `0_1_2`, the full
//! existence index `0_1_2_3_4`, and the inverse `0_3_4`. Every index stores its value columns
//! *inline*, so the full 5-column tuple is copied many times.
//!
//! This module replaces all of that with one shared store: the Ascent BYODS "ind_common".
//! Every logical index becomes a lightweight *view* over that single store.
//!
//! ## Why flat, not a nested trie
//!
//! The earlier design stored `(F,V) -> P -> {(M,Fp)}` as `HashMap<(F,V), HashMap<P,
//! HashSet<(M,Fp)>>>`. On a representative workload the groups turned out to be *tiny*: about
//! 5 leaves per `(F,V)` group, spread over about 2 distinct `P`. That left the two inner hash
//! levels nearly empty. Each one pays hashbrown's minimum **4**-bucket allocation, plus
//! control bytes, to hold about 2 elements. Most of the store's bytes went to that structural
//! slack rather than to the data itself: 36k inner maps were about 53%, 70k leaf sets about
//! 38%, and only about 13% was real data plus the outer map.
//!
//! Flattening does not win all of that back. The flat form stores `P` inline on every leaf,
//! where the nested form shared one `P` per leaf set. Measured against a faithful rebuild of
//! the nested design over identical data, the flat design is 1.1–2.3× smaller, not about 10×.
//!
//! We collapse both inner levels into one **[`HybridSet`] of `(P,M,Fp)` per `(F,V)` group**.
//! That gives us:
//!   - one heap allocation per group, instead of `1 + (#distinct P)` tiny hash tables;
//!   - leaves packed 24 B each, with no per-element control bytes. In the small
//!     representation the whole occupancy map is a single `u64` word beside the slots;
//!   - a group that is only two words wide, so the outer map entry is 32 B rather than 48.
//!     One structure changes regime as it grows; it is not an enum over two representations;
//!   - existence checks that probe rather than scan, at every group size.
//!
//! ## Large groups
//!
//! No single representation serves both ends of the group-size distribution. A function with
//! dense aliasing is *rare*, but its `(F,V)` group grows to tens of thousands of leaves. If
//! the delta->total merge re-copies the whole accumulated group, then building a group of size
//! `G` costs O(G^2). On such inputs that dominates everything else: profiling put it at about
//! 55% of index time (see memory-investigation.md §7/§8).
//!
//! So a group that grows past [`SMALL_THRESHOLD`] switches to Swiss probing, using this
//! crate's own table (see [`super::hybrid_set::swiss`]). There the merge is O(delta), because
//! it inserts only the new leaves. Below the threshold the group probes linearly over an
//! allocation that holds its element slots plus one occupancy word. The switch changes the
//! regime *within* one structure; it does not change the type. See [`super::hybrid_set`] for
//! why that is the right small representation, why it is not an enum, and how the transition
//! is made cheap.
//!
//! The forward map `(F,V) -> [ (P,M,Fp) ]` serves `none`, `0_1`, `0_1_2`, existence, and
//! iteration. We do not materialize a full inverse. As before, the `0_3_4` view is *derived*
//! by scanning, and a small side-index `fidx: F -> {V}` narrows each `0_3_4` probe to the
//! flow-variables of the function being probed.
//!
//! Correctness note (we differ here from `ascent_byods_rels::eqrel`): Ascent merges the shared
//! store twice per iteration, once through the ind_common and once through the full-index
//! write target. eqrel tolerates that, because union-find merge is idempotent. `locals` is a
//! plain relation, so a double merge would corrupt semi-naive evaluation. We therefore keep
//! the *only* real merge on the ind_common ([`LocalsIndCommon`]). Every index write target
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

use super::hybrid_set::{HybridSet, SMALL_THRESHOLD};

// The store keys are trusted ids derived from the program, so we hash them with
// the fast, deterministic `FxHasher` instead of the DoS-resistant SipHash the
// std collections use.
type Map<K, V> = hashbrown::HashMap<K, V, BuildHasherDefault<FxHasher>>;
type Set<T> = hashbrown::HashSet<T, BuildHasherDefault<FxHasher>>;

// ---------------------------------------------------------------------------
// Sizing hashbrown's own tables, for [`HeapReport`].
// ---------------------------------------------------------------------------
//
// The store's leaves live in `HybridSet`s, and those report their own bytes. But the maps
// holding those sets are hashbrown's, and hashbrown reports only `capacity()`. These three
// items turn a capacity into bytes. They live here because this store is what they were
// written for, and what the counting-allocator bench validates them against. `assign_like_trie`
// and `hybrid_set`'s tests use them from here.

/// Width in bytes of hashbrown's SIMD control group.
///
/// On top of one control byte per bucket, every table's allocation carries `Group::WIDTH`
/// trailing control bytes. The heap estimators below need that width, and hashbrown does not
/// export it. hashbrown picks the implementation by target feature
/// (`hashbrown-0.16.1/src/control/group/mod.rs`); we mirror that choice here.
pub const HB_GROUP_WIDTH: usize = if cfg!(all(
    target_feature = "sse2",
    any(target_arch = "x86", target_arch = "x86_64"),
    not(miri)
)) {
    16 // sse2
} else if cfg!(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little",
    not(miri)
)) {
    8 // neon
} else {
    std::mem::size_of::<usize>() // generic fallback: Group is a usize
};

/// Number of buckets hashbrown allocated for a table that reports `capacity`. Returns 0 if the
/// table has never allocated.
///
/// This inverts hashbrown's `bucket_mask_to_capacity`. A table of `b >= 8` buckets reports
/// `b / 8 * 7`, keeping 12.5% of its slots empty. The smallest table has `b == 4` and reports
/// 3, so the floor is **4** buckets, not 8. `capacity_to_buckets` does lift the minimum to 8 or
/// 16 buckets for elements narrower than 4 bytes. We do not model that lift, because the
/// `capacity` the table reports already reflects it.
pub fn hb_buckets(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    (capacity * 8).div_ceil(7).next_power_of_two().max(4)
}

/// Estimated heap bytes held by a hashbrown table whose element type is `elem` bytes wide and
/// whose `capacity()` is `capacity`. The estimate includes load-factor slack, because it prices
/// the buckets actually allocated rather than the elements stored.
///
/// For any element whose alignment is at most [`HB_GROUP_WIDTH`], this reproduces hashbrown's
/// `calculate_layout_for` exactly: element slots padded up to the control alignment, then one
/// control byte per bucket, plus a [`HB_GROUP_WIDTH`] mirror. Every type we measure through
/// this is 8-byte-aligned, so the estimate is exact. An element with a stricter alignment would
/// allocate slightly more padding than we report.
pub fn hb_bytes(capacity: usize, elem: usize) -> usize {
    let buckets = hb_buckets(capacity);
    if buckets == 0 {
        return 0;
    }
    buckets
        .saturating_mul(elem)
        .next_multiple_of(HB_GROUP_WIDTH)
        + buckets
        + HB_GROUP_WIDTH
}

// ---------------------------------------------------------------------------
// A single `(F,V)` group's leaves.
// ---------------------------------------------------------------------------

/// The leaves of one `(F,V)` group, held in a [`HybridSet`].
///
/// A `HybridSet` is a two-word structure. While the group holds at most [`SMALL_THRESHOLD`]
/// leaves it probes linearly over its bare element slots. That is by far the common case: 67
/// to 100% of the groups hold exactly *one* leaf, in every store we have measured, whatever
/// the largest group in it was. Above the threshold it switches to Swiss probing, which
/// keeps the per-iteration delta->total merge at O(delta) instead of re-copying the whole
/// accumulated group every round. Being two words rather than three saves 8 B on *every* entry
/// of the forward map, whether or not that entry ever gets promoted.
type Group<P, M, Fp> = HybridSet<(P, M, Fp)>;

// ---------------------------------------------------------------------------
// Physical `rel!` storage. It stores no tuples, since all the data lives in the
// shared ind_common. It does track the row count, so that `prog.locals.len()`
// still reports the true size. That call is the only thing that reads the
// physical relation after a run. The generated code calls `push` exactly once
// per newly-inserted row.
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
// A boxed iterator we can clone. This is a local copy of byods' private
// IteratorFromDyn, which we cannot reach from here.
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
// The shared store, which is Ascent's `ind_common`.
// ---------------------------------------------------------------------------
pub struct LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    /// Forward map: `(F,V)` -> the set of `(P, M, Fp)` leaves for that group. It serves the
    /// `none`, `0_1`, `0_1_2`, and existence views directly, and the `0_3_4` view by
    /// *scanning*. Because a group is a set, existence is a probe, and `0_1_2` filters the
    /// group for its `P` rather than slicing a run. One heap allocation per group replaces
    /// the old nested `HashMap<P, HashSet<(M,Fp)>>`, which on tiny groups was mostly
    /// hashbrown slack.
    fwd: Map<(F, V), Group<P, M, Fp>>,
    /// Side-index: `F` -> the set of `V` present for that function. It lets a `0_3_4` probe
    /// restrict its scan to the flow-variables of the function being probed, instead of
    /// walking every `(F,V)` group in `fwd`. We keep it in lockstep with `fwd`'s outer keys,
    /// one V per `(F,V)` group. That is cheap: one V (8 B plus hashbrown slack) per group,
    /// against a store measured in gigabytes.
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

    /// Number of distinct `(F, V)` groups. That is the number of distinct variables reached by
    /// some formal, because `fwd`'s outer key is exactly the subject of a `locals` row. It
    /// costs O(1): the trie already keys on the prefix that a scan of the rows would have to
    /// rediscover.
    #[inline]
    pub fn num_reached_variables(&self) -> usize {
        self.fwd.len()
    }

    /// Phase-0 instrumentation. Estimates the heap bytes held by the forward store against
    /// those held by the `fidx` side-index, so we can see *which* structure dominates before
    /// we optimize. An external `phys_footprint` cannot attribute bytes to a sub-structure,
    /// so we count them here.
    ///
    /// Hash-table bytes include load-factor slack ([`hb_bytes`], which is exact for these
    /// element types), and group bytes come from [`HybridSet::heap_bytes`]. The whole report
    /// therefore accounts for allocation sizes, not for payload. It takes one pass, O(rows).
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
        // Groups are unordered, so counting distinct `P` needs a scratch set rather than a run
        // count. We use one set and clear it per group, which keeps the report to a single
        // extra allocation.
        let mut ps: Set<&P> = Set::default();
        for group in self.fwd.values() {
            r.leaf_elems += group.len();
            r.max_group = r.max_group.max(group.len());
            // Coarse power-of-two histogram of group sizes: bucket i counts the groups whose
            // leaf count falls in `[2^i, 2^(i+1))`. This is what tells a benchmark, or a
            // profile of a real target, which regime the store is in: many tiny groups, which
            // the small representation is tuned for, or a few huge ones, which get promoted.
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
        // `(P,M,Fp)` are cheap to clone: 8-byte handles plus an i16. Cloning them lets us skip
        // a borrow-key helper.
        self.fwd
            .get(&(f.clone(), v.clone()))
            .is_some_and(|group| group.contains(&(p.clone(), m.clone(), fp.clone())))
    }

    /// Insert a full tuple. Returns true if it was new to *this* store.
    fn insert(&mut self, key: &(F, V, P, M, Fp)) -> bool {
        use hashbrown::hash_map::Entry;
        let (f, v, p, m, fp) = key;
        // Fetch the (F,V) group. The first time a group appears, record its V in the
        // side-index.
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

    /// Merge `from` into `self`, taking the union. The delta->total move uses this.
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

/// Byte estimate for the `locals` store, broken down per structure. See
/// [`LocalsIndCommon::heap_report`].
#[derive(Debug, Clone)]
pub struct HeapReport {
    /// Logical row count. Equals the size of `locals`.
    pub rows: usize,
    /// Number of distinct `(F,V)` groups, that is, of outer forward keys.
    pub fv_groups: usize,
    /// Number of distinct `(F,V,P)` entries across all groups.
    pub p_entries: usize,
    /// Number of `(P,M,Fp)` leaf elements across all groups. Equals `rows`.
    pub leaf_elems: usize,
    /// Estimated bytes for the whole forward store: the outer map plus every group's leaves.
    pub fwd_bytes: usize,
    /// Number of distinct functions in the `fidx` side-index, that is, of its outer keys.
    pub fidx_funcs: usize,
    /// Number of `V` entries across all `fidx` sets. Equals the number of distinct `(F,V)`
    /// groups, which is `fv_groups`.
    pub fidx_vs: usize,
    /// Estimated bytes for the `fidx` side-index: the outer map plus every function's V set.
    pub fidx_bytes: usize,
    /// Element sizes `(outer, _unused, leaf, fidx_outer, fidx_val)`, for reference.
    pub elem_sizes: (usize, usize, usize, usize, usize),
    /// Size in leaves of the largest single `(F,V)` group. This is what tells us whether the
    /// small, linear-probing representation is still the right one for this store.
    pub max_group: usize,
    /// How many groups grew past [`SMALL_THRESHOLD`] and switched to Swiss probing.
    pub large_groups: usize,
    /// Power-of-two histogram of group sizes. `group_hist[i]` counts the groups with
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
        // Read "0:12 1:3 5:1" as: 12 groups of 1 leaf, 3 groups of 2-3, and 1 group of 32-63.
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

// The one real merge lives here, on the ind_common.
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
    // We take the default `merge_delta_to_total_new_to_delta`: it moves delta into total, then
    // swaps new and delta.
}

// ---------------------------------------------------------------------------
// Write targets.
// ---------------------------------------------------------------------------

/// No-op write target for the partial (view) indices. Data enters the store only through the
/// full index, and only the ind_common merges.
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

/// Write target of the full existence index. It performs the real inserts into the store, but
/// its merge is a no-op. The real merge belongs to the ind_common; see the module docs.
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
// Read views and their ToRelIndex markers.
//
// Each `To*` marker is a zero-sized field in the generated struct. `to_rel_index` borrows the
// shared store and produces a read view; `to_rel_index_write` produces a write target.
// ---------------------------------------------------------------------------

/// Defines a zero-sized `ToRelIndex` marker over the concrete `LocalsIndCommon` store.
/// `$wrty` and `$wrbody` give the write target: the real one for the full index, a no-op one
/// for the views.
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
        // A group is a set, not a sorted run, so the leaves carrying this `P` are scattered.
        // This view therefore filters rather than slicing a range. The one scan up front is
        // what lets us return `None` for a `P` the group does not hold, which cuts the caller's
        // whole join. Without it, a miss would hand the planner a `Some` that it has to drive
        // to exhaustion. The scan costs O(group), it stops at the first match, and the median
        // group holds a single leaf.
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
        // Emit one key per distinct (F,V,P), each with all of its (M,Fp) leaves. Groups are
        // unordered, so we have to bucket the leaves of one `P` rather than slice them off as a
        // run. This is not a hot path. The rules point-probe `0_1_2`, so we only land here if a
        // planner chose it as a join driver.
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
        // This view is derived; there is no materialized inverse. For the probed function `f`,
        // we visit only its flow-variables, found through the `fidx` side-index, and yield
        // every `(v, p)` whose group holds a leaf with this `(m, fp)`. That touches one
        // function's `(F,V)` groups instead of scanning all of `fwd`. It is a cold path: rule
        // 2.2 makes on the order of tens of probes. We return `Some` of an iterator that may be
        // empty, which for a join means the same thing as `None`.
        let c = self.0;
        let (f, m, fp) = key.clone();
        Some(DynIter::new(move || {
            let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
            // We keep `fidx[f]` and `fwd` in lockstep, so every V here has an `fwd` group.
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
        // There is no materialized inverse to size. `0_3_4` is only ever probed, never used as
        // a join driver, so a large estimate is safe: it discourages the planner from choosing
        // it as a driver, which is what we want. We use the row count as a conservative upper
        // bound.
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
        // This is not on any hot path. Rule 2.2 only ever point-probes `0_3_4`; nothing
        // iterates it as a driver. So we give a correct fallback by inverting `fwd` on demand
        // and throwing the result away. If this ever shows up in a profile, some rule has
        // started iterating `0_3_4`, and we should reconsider materializing the inverse.
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
// The BYODS provider macros. Codegen calls them as
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

    /// Builds a store from `rows` and checks every read view against those rows.
    ///
    /// The views are the part of this module that changed when groups stopped being sorted.
    /// `0_1_2` now filters where it used to slice a run, and `iter_all` buckets where it used
    /// to split one. Callers exercise groups on both sides of `SMALL_THRESHOLD`.
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

        // `none`: () -> every row.
        let view = ViewNone(&store);
        let mut got: Vec<_> = RelIndexRead::index_get(&view, &())
            .unwrap()
            .map(|(f, v, p, m, fp)| (*f, *v, *p, *m, *fp))
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "`none` view");

        // `0_1`: (F,V) -> (P,M,Fp).
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

        // `0_1_2`: (F,V,P) -> (M,Fp). A miss must give `None`, not an empty iterator.
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

        // `0_3_4`: (F,M,Fp) -> (V,P), derived by scanning through `fidx`.
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

        // The full existence index.
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

    /// Builds `groups` groups over two functions. Each group holds `group` leaves, spread over
    /// `paths` distinct `P`.
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

    /// `absorb` is the delta->total merge. It has to keep `len`, `fidx`, and the groups
    /// consistent, whichever representation the two sides are in.
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
            // `fidx` must list the V of every group, or `0_3_4` can no longer find them.
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
