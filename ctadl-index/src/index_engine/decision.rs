//! Decisions and decision sets: what the contextual relations of the index engine are keyed
//! by.
//!
//! A [`Decision`] names *what* a resolvent decided at a function -- "formal `n.p` holds call
//! target `t`" -- and never the route it arrived by. A [`DecisionSet`] is the lattice column of
//! the contextual relations: a contextual row `(f, v, p, a, p4)` holds under every decision in
//! its set, and a row derived from it inherits the set. Two decisions whose flows meet at a row
//! share the row instead of duplicating it, so the contextual relations are bounded by the
//! context-free ones (one row per `(f, v, p, a, p4)`) without dropping any decision.
//!
//! Decisions are interned to a dense [`DecisionId`] and sets are interned as sorted slices, so
//! a set is a 4-byte `Copy` handle, equality is a handle compare, and rows that hold the same
//! set share one slice. The interners are process-global and never free: a decision or a set
//! lives as long as the process, which is the price of hash-consing and is measured by
//! [`stats`] and [`interner_stats`].

use std::fmt::{self, Display};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, RwLock};

use ascent::Lattice;
use hashbrown::HashMap;

use crate::facts::{CallTargetObject, FormalIndex, Path};

/// What a function's contextual flows are conditioned on: its formal `formal.path` holds the
/// call target `target`. A caller establishes it by passing the target there, directly or from
/// its own formal (rules 2.1 and 2.2); the function's conditional flows are keyed by it and
/// applied at every such caller (rule 3.2).
///
/// The key names *what* was decided, never the route it arrived by, so two routes that
/// establish the same decision share one closure and both get the result.
///
/// `Ord` compares the target by handle; sort with [`Decision::content_cmp`] wherever the order
/// is printed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Decision {
    pub formal: FormalIndex,
    pub path: Path,
    pub target: CallTargetObject,
}

impl Decision {
    /// Orders by content, not by handle, so a sort that feeds output is reproducible.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.formal
            .cmp(&other.formal)
            .then_with(|| self.path.cmp(&other.path))
            .then_with(|| self.target.content_cmp(&other.target))
    }
}

impl Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[arg{}{} = {}]",
            self.formal,
            self.path.to_dot_string(),
            self.target
        )
    }
}

/// A dense, process-global identity for a [`Decision`]; see [`DecisionId::of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DecisionId(u32);

#[derive(Default)]
struct DecisionTable {
    ids: HashMap<Decision, DecisionId>,
    decisions: Vec<Decision>,
}

static DECISIONS: LazyLock<RwLock<DecisionTable>> = LazyLock::new(Default::default);
/// `{d}` as an exact and as a collapsing set, by id, interned once per decision: the rules ask
/// for a decision's singleton once per row they derive from it, and read it here without a
/// lock (an append-only vector; pushed in id order under the table's write lock).
static SINGLETONS: LazyLock<ascent::boxcar::Vec<(DecisionSet, DecisionSet)>> =
    LazyLock::new(ascent::boxcar::Vec::new);

impl DecisionId {
    /// The id of `d`, minted on first use.
    pub fn of(d: Decision) -> DecisionId {
        {
            let table = DECISIONS.read().unwrap();
            if let Some(id) = table.ids.get(&d) {
                return *id;
            }
        }
        let mut table = DECISIONS.write().unwrap();
        if let Some(id) = table.ids.get(&d) {
            return *id;
        }
        let id = DecisionId(u32::try_from(table.decisions.len()).expect("decision ids exhausted"));
        table.decisions.push(d.clone());
        table.ids.insert(d, id);
        let slot = SINGLETONS.push((
            DecisionSet::intern(&[id], false),
            DecisionSet::intern(&[id], true),
        ));
        debug_assert_eq!(slot, id.0 as usize);
        id
    }

    /// `{self}`, exact or collapsing; a lock-free read, not an interning.
    pub fn singleton(self, collapse: bool) -> DecisionSet {
        let pair = SINGLETONS[self.0 as usize];
        if collapse { pair.1 } else { pair.0 }
    }

    /// The decision this id was minted for.
    pub fn get(self) -> Decision {
        DECISIONS.read().unwrap().decisions[self.0 as usize].clone()
    }

    /// How many decisions have been minted in this process.
    pub fn count() -> usize {
        DECISIONS.read().unwrap().decisions.len()
    }
}

impl Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

/// A set of decisions: a 4-byte handle to a sorted, duplicate-free slice. `Copy`; equal sets
/// have equal handles. The lattice order is inclusion and the join is union.
///
/// A set is either *exact* or *collapsing*, and the two never meet (one kind per index). An
/// exact set is what it says. A collapsing set holds at most one decision: joining two
/// different singletons gives [`DecisionSet::top`], "every decision of this function", which
/// is above everything and absorbs every join. That is the flat lattice `⊥ < {d} < ⊤` -- a
/// coarser, parameter-free abstraction under which a row changes at most once after it is
/// created, where an exact set at a row changes once per decision that reaches the row after
/// it.
///
/// **Encoding.** Each kind has its own interner, whose ids all fit in 31 bits; the top bit,
/// [`COLLAPSING`], names the kind and so which interner the low bits index. The empty set and
/// the two ⊤s take raw values below 256, which neither interner issues (an id is at least its
/// shard count). The kind is a property of each set, not of the process, so indexes of both
/// kinds can be built side by side (R16).
///
/// `Eq` and `Hash` act on the handle alone. There is no `Ord`: the order is the lattice's
/// `PartialOrd` (R10).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecisionSet(NonZeroU32);

immortal::id_handle! {
    /// The members of an exact set with at least one member.
    struct ExactMembers([DecisionId]) in 256 shards, 31 bits;
}

immortal::id_handle! {
    /// The members of a collapsing set with at least one member (so exactly one).
    struct CollapsingMembers([DecisionId]) in 256 shards, 31 bits;
}

/// The bit of a [`DecisionSet`] that marks a collapsing set.
const COLLAPSING: u32 = 1 << 31;
const EMPTY: u32 = 1;
const TOP_EXACT: u32 = 2;
const TOP: u32 = TOP_EXACT | COLLAPSING;
/// Raw values below this, with the kind bit cleared, are reserved: the members interners have
/// 256 shards, and an id is `(local + 1) << 8 | shard`.
const FIRST_INTERNED: u32 = 256;

/// The interners' entries and heap bytes, for the profile.
pub fn interner_stats() -> [(&'static str, immortal::Stats); 2] {
    [
        ("DecisionSet.exact", ExactMembers::stats()),
        ("DecisionSet.collapsing", CollapsingMembers::stats()),
    ]
}

/// The widening bound on exact sets: an exact union with more members than this is ⊤. `0` is
/// no bound. Process-global (one bound per run); set by [`set_widen_bound`] before the run.
static WIDEN_BOUND: AtomicUsize = AtomicUsize::new(0);

/// Sets the widening bound for exact sets (`HybridContext::Bounded(k)` and `Spill(k)`); `0`
/// removes it.
pub fn set_widen_bound(k: usize) {
    WIDEN_BOUND.store(k, Ordering::Relaxed);
}

/// Whether a widened (⊤) row spills into the context-free relations instead of staying a
/// contextual row (`HybridContext::Spill(k)`). Read on the closure's hot rules, so an atomic
/// rather than a `config` clause.
static SPILL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_spill(on: bool) {
    SPILL.store(on, Ordering::Relaxed);
}

/// `true` when `ds` is ⊤ and ⊤ rows spill: the row leaves the contextual closure here.
pub fn spills(ds: DecisionSet) -> bool {
    ds.is_top() && SPILL.load(Ordering::Relaxed)
}

/// Unions computed (both operands non-trivial), how many of those produced a new set, and how
/// many missed the union cache.
static UNIONS: AtomicUsize = AtomicUsize::new(0);
static UNIONS_GREW: AtomicUsize = AtomicUsize::new(0);
static UNION_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
/// The union memo: a direct-mapped cache from an unordered pair of set handles to the union's
/// handle, shared by every thread without a lock. Each slot is a seqlock -- a writer takes the
/// slot by moving its sequence to odd, fills it, and releases it even; a reader accepts a slot
/// only if the sequence was even before and unchanged after -- so a torn entry is never
/// returned, and a writer that loses the race simply does not cache. Sets are interned, so the
/// same two handles meet again and again (every row's set against the set of every row that
/// feeds it), and the lookup is the hot path of the whole contextual closure: a thread-local
/// hash map here would cost more than the joins around it.
///
/// The key packs the smaller handle in the low half. No key is 0, so an unwritten slot never
/// matches.
struct UnionSlot {
    seq: AtomicU32,
    key: AtomicU64,
    result: AtomicU32,
}

// Rust puts `key` first, so the slot packs into 16 bytes.
const _: () = assert!(size_of::<UnionSlot>() == 16);

const UNION_CACHE_BITS: u32 = 20;
static UNION_CACHE: LazyLock<Box<[UnionSlot]>> = LazyLock::new(|| {
    (0..1usize << UNION_CACHE_BITS)
        .map(|_| UnionSlot {
            seq: AtomicU32::new(0),
            key: AtomicU64::new(0),
            result: AtomicU32::new(0),
        })
        .collect()
});

fn union_slot(key: u64) -> &'static UnionSlot {
    // Fibonacci hashing of the pair. A handle's low bits are its shard, which its interner
    // took from the set's hash, so every bit of the key carries information; the multiply
    // carries the high handle into the top bits.
    let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    &UNION_CACHE[(h >> (u64::BITS - UNION_CACHE_BITS)) as usize]
}

fn union_cache_get(key: u64) -> Option<u32> {
    let slot = union_slot(key);
    let s1 = slot.seq.load(Ordering::Acquire);
    if s1 & 1 == 1 {
        return None;
    }
    let (k, result) = (
        slot.key.load(Ordering::Relaxed),
        slot.result.load(Ordering::Relaxed),
    );
    std::sync::atomic::fence(Ordering::Acquire);
    if slot.seq.load(Ordering::Relaxed) != s1 || k != key {
        return None;
    }
    Some(result)
}

fn union_cache_put(key: u64, result: u32) {
    let slot = union_slot(key);
    let s = slot.seq.load(Ordering::Relaxed);
    if s & 1 == 1
        || slot
            .seq
            .compare_exchange(s, s.wrapping_add(1), Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    // Pairs with the reader's acquire fence: a reader that sees either store below also sees
    // the odd sequence, so its second load of `seq` rejects the slot. The Acquire on the claim
    // orders these stores after the previous writer's.
    std::sync::atomic::fence(Ordering::Release);
    slot.key.store(key, Ordering::Relaxed);
    slot.result.store(result, Ordering::Relaxed);
    slot.seq.store(s.wrapping_add(2), Ordering::Release);
}

impl DecisionSet {
    const fn reserved(raw: u32) -> DecisionSet {
        DecisionSet(NonZeroU32::new(raw).unwrap())
    }

    fn intern(sorted: &[DecisionId], collapse: bool) -> DecisionSet {
        debug_assert!(sorted.windows(2).all(|w| w[0] < w[1]));
        if sorted.is_empty() {
            return DecisionSet::empty();
        }
        let raw = if collapse {
            CollapsingMembers::intern(sorted).raw() | COLLAPSING
        } else {
            ExactMembers::intern(sorted).raw()
        };
        DecisionSet(NonZeroU32::new(raw).expect("ids are never 0"))
    }

    /// The members of an explicit set, read without a lock; empty for the empty set and ⊤.
    #[inline]
    fn members(self) -> &'static [DecisionId] {
        let raw = self.0.get();
        let id = raw & !COLLAPSING;
        if id < FIRST_INTERNED {
            return &[];
        }
        // SAFETY: a set at or above `FIRST_INTERNED` was made by `intern`, which put the bits
        // of a handle of the kind its top bit names in the low bits, and the set's holder
        // happens after that.
        unsafe {
            if raw & COLLAPSING != 0 {
                CollapsingMembers::from_raw_unchecked(id).get()
            } else {
                ExactMembers::from_raw_unchecked(id).get()
            }
        }
    }

    /// The empty set: the lattice bottom (of either kind; it joins into anything).
    pub const fn empty() -> DecisionSet {
        DecisionSet::reserved(EMPTY)
    }

    /// `{d}`, exact or collapsing.
    #[cfg(test)]
    pub fn singleton(d: DecisionId, collapse: bool) -> DecisionSet {
        DecisionSet::intern(&[d], collapse)
    }

    /// ⊤: every decision of the function the row belongs to. A collapsing join makes it, and
    /// so does an exact union that passes the widening bound (`top_exact`).
    pub const fn top() -> DecisionSet {
        DecisionSet::reserved(TOP)
    }

    /// ⊤ of the exact kind: what a bounded exact union widens to. Absorbs every exact join.
    pub const fn top_exact() -> DecisionSet {
        DecisionSet::reserved(TOP_EXACT)
    }

    pub fn is_top(self) -> bool {
        self == DecisionSet::top() || self == DecisionSet::top_exact()
    }

    /// Whether this is a collapsing set (see the type's doc). The empty set is exact.
    pub fn is_collapsing(self) -> bool {
        self.0.get() & COLLAPSING != 0
    }

    pub fn is_empty(self) -> bool {
        self == DecisionSet::empty()
    }

    /// Elements of an explicit set; 0 for ⊤, which is not an explicit set.
    pub fn len(self) -> usize {
        self.members().len()
    }

    /// The explicit elements; empty for ⊤, whose members are implicit.
    pub fn ids(self) -> impl Iterator<Item = DecisionId> + 'static {
        self.members().iter().copied()
    }

    /// The decisions in an explicit set, looked up once per element.
    pub fn decisions(self) -> Vec<Decision> {
        let table = DECISIONS.read().unwrap();
        self.ids()
            .map(|id| table.decisions[id.0 as usize].clone())
            .collect()
    }

    pub fn contains(self, d: DecisionId) -> bool {
        self.is_top() || self.members().binary_search(&d).is_ok()
    }

    /// `self ⊆ other`, by a merge walk (both sorted).
    pub fn is_subset(self, other: DecisionSet) -> bool {
        if self == other || self.is_empty() || other.is_top() {
            return true;
        }
        if self.is_top() {
            return false;
        }
        let (a, b) = (self.members(), other.members());
        if a.len() > b.len() {
            return false;
        }
        let mut j = 0;
        for x in a {
            while j < b.len() && b[j] < *x {
                j += 1;
            }
            if j == b.len() || b[j] != *x {
                return false;
            }
            j += 1;
        }
        true
    }

    /// `self ∪ other`, interned. Returns `self` itself when nothing is added. Between
    /// collapsing sets, two different singletons give ⊤.
    pub fn union(self, other: DecisionSet) -> DecisionSet {
        if self == other || other.is_empty() {
            return self;
        }
        if self.is_empty() {
            return other;
        }
        if self.is_collapsing() || other.is_collapsing() {
            debug_assert!(self.is_collapsing() && other.is_collapsing());
            UNIONS.fetch_add(1, Ordering::Relaxed);
            if self.is_top() {
                return self;
            }
            UNIONS_GREW.fetch_add(1, Ordering::Relaxed);
            return DecisionSet::top();
        }
        UNIONS.fetch_add(1, Ordering::Relaxed);
        // Exact ⊤ (a widened set) absorbs.
        if self.is_top() {
            return self;
        }
        if other.is_top() {
            UNIONS_GREW.fetch_add(1, Ordering::Relaxed);
            return other;
        }
        // The key is unordered: union is commutative.
        let key = {
            let (a, b) = (self.0.get() as u64, other.0.get() as u64);
            if a < b { a | b << 32 } else { b | a << 32 }
        };
        let result = match union_cache_get(key) {
            // SAFETY: the seqlock returns only a result that `union_cache_put` stored whole,
            // and every result stored is a set's raw handle (never 0).
            Some(raw) => DecisionSet(unsafe { NonZeroU32::new_unchecked(raw) }),
            None => {
                UNION_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
                let result = self.union_uncached(other);
                union_cache_put(key, result.0.get());
                result
            }
        };
        if result != self {
            UNIONS_GREW.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn union_uncached(self, other: DecisionSet) -> DecisionSet {
        if other.is_subset(self) {
            return self;
        }
        if self.is_subset(other) {
            return other;
        }
        let (a, b) = (self.members(), other.members());
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Less => {
                    out.push(a[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(b[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    out.push(a[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        let bound = WIDEN_BOUND.load(Ordering::Relaxed);
        if bound != 0 && out.len() > bound {
            return DecisionSet::top_exact();
        }
        DecisionSet::intern(&out, false)
    }

    /// `self ∩ other`, interned.
    pub fn intersection(self, other: DecisionSet) -> DecisionSet {
        if self == other || self.is_top() {
            return other;
        }
        if other.is_top() {
            return self;
        }
        let out: Vec<DecisionId> = self.ids().filter(|d| other.contains(*d)).collect();
        DecisionSet::intern(&out, self.is_collapsing())
    }
}

/// Inclusion order, as [`Lattice`] requires; not total.
impl PartialOrd for DecisionSet {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering::*;
        if self == other {
            Some(Equal)
        } else if self.is_subset(*other) {
            Some(Less)
        } else if other.is_subset(*self) {
            Some(Greater)
        } else {
            None
        }
    }
}

impl Lattice for DecisionSet {
    fn join_mut(&mut self, other: Self) -> bool {
        let joined = self.union(other);
        if joined == *self {
            false
        } else {
            *self = joined;
            true
        }
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let met = self.intersection(other);
        if met == *self {
            false
        } else {
            *self = met;
            true
        }
    }
}

impl Default for DecisionSet {
    fn default() -> Self {
        DecisionSet::empty()
    }
}

impl fmt::Debug for DecisionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_top() {
            return write!(f, "⊤");
        }
        f.debug_list().entries(self.ids()).finish()
    }
}

impl Display for DecisionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_top() {
            return write!(f, "⊤");
        }
        let ds = self.decisions();
        write!(f, "{{")?;
        for (i, d) in ds.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{d}")?;
        }
        write!(f, "}}")
    }
}

/// Process-wide counters for the decision interners, for the debug log.
pub fn stats() -> String {
    format!(
        "decisions={} set unions={} (grew {}, union cache misses {})",
        DecisionId::count(),
        UNIONS.load(Ordering::Relaxed),
        UNIONS_GREW.load(Ordering::Relaxed),
        UNION_CACHE_MISSES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(i: u32) -> DecisionId {
        DecisionId(i)
    }

    #[test]
    fn collapsing_sets_meet_at_top() {
        let a = DecisionSet::singleton(d(1), true);
        let b = DecisionSet::singleton(d(2), true);
        assert_eq!(a.union(a), a);
        assert_eq!(a.union(DecisionSet::empty()), a);
        let t = a.union(b);
        assert!(t.is_top() && t.contains(d(7)) && t.union(a).is_top() && a.is_subset(t));
        assert!(!t.is_subset(a));
        assert_eq!(a.partial_cmp(&t), Some(std::cmp::Ordering::Less));
        let mut x = a;
        assert!(x.join_mut(b) && x.is_top());
        assert!(!x.join_mut(a));
    }

    #[test]
    fn union_is_sorted_and_interned() {
        let a = DecisionSet::intern(&[d(1), d(3)], false);
        let b = DecisionSet::intern(&[d(2), d(3), d(9)], false);
        let u = a.union(b);
        assert_eq!(u.ids().collect::<Vec<_>>(), vec![d(1), d(2), d(3), d(9)]);
        assert_eq!(u, b.union(a));
        assert_eq!(a.union(a), a);
        assert_eq!(a.union(DecisionSet::empty()), a);
        assert!(a.is_subset(u) && b.is_subset(u) && !u.is_subset(a));
        assert_eq!(a.partial_cmp(&u), Some(std::cmp::Ordering::Less));
        assert_eq!(a.partial_cmp(&b), None);
        let mut x = a;
        assert!(x.join_mut(b));
        assert_eq!(x, u);
        assert!(!x.join_mut(a));
        assert_eq!(a.intersection(b).ids().collect::<Vec<_>>(), vec![d(3)]);
    }

    /// Both kinds live in one process, and a set's handle alone says which it is.
    #[test]
    fn a_handle_names_its_kind() {
        assert_eq!(size_of::<DecisionSet>(), 4);
        assert_eq!(size_of::<Option<DecisionSet>>(), 4);
        let exact = DecisionSet::singleton(d(5), false);
        let collapsing = DecisionSet::singleton(d(5), true);
        assert_ne!(exact, collapsing);
        assert!(!exact.is_collapsing() && collapsing.is_collapsing());
        assert_eq!(exact.ids().collect::<Vec<_>>(), [d(5)]);
        assert_eq!(collapsing.ids().collect::<Vec<_>>(), [d(5)]);
        let grown = exact.union(DecisionSet::singleton(d(6), false));
        assert!(!grown.is_collapsing() && grown.len() == 2);
        assert_eq!(
            collapsing.union(DecisionSet::singleton(d(6), true)),
            DecisionSet::top()
        );
        assert!(DecisionSet::top().is_collapsing() && !DecisionSet::top_exact().is_collapsing());
        assert!(DecisionSet::top_exact().is_top() && !DecisionSet::empty().is_top());
        assert_eq!(DecisionSet::top_exact().len(), 0);
        assert!(DecisionSet::empty().is_empty() && DecisionSet::empty().ids().next().is_none());
        assert_eq!(DecisionSet::intern(&[], true), DecisionSet::empty());
        assert_eq!(
            collapsing.intersection(DecisionSet::singleton(d(6), true)),
            DecisionSet::empty()
        );
    }

    /// Two keys that share a union-cache slot, written in turn while readers look both up: a
    /// hit must return the result stored for the key it matched, never the other's. Each key
    /// pairs the empty set with another handle, a pair `union` never caches.
    #[test]
    fn union_cache_never_returns_a_torn_entry() {
        use std::sync::atomic::AtomicBool;

        let results = [0x1111_1111u32, 0x2222_2222];
        let mut by_slot = HashMap::new();
        let keys = (EMPTY as u64 + 1..)
            .map(|b| EMPTY as u64 | b << 32)
            .find_map(|k| {
                let slot = std::ptr::from_ref(union_slot(k)) as usize;
                by_slot.insert(slot, k).map(|other| [other, k])
            })
            .unwrap();
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            for w in 0..2 {
                let stop = &stop;
                s.spawn(move || {
                    for i in 0..1_000_000 {
                        let j = (i + w) % 2;
                        union_cache_put(keys[j], results[j]);
                    }
                    stop.store(true, Ordering::Relaxed);
                });
            }
            for _ in 0..2 {
                let stop = &stop;
                s.spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        for (j, &key) in keys.iter().enumerate() {
                            if let Some(r) = union_cache_get(key) {
                                assert_eq!(r, results[j], "key {j}: torn entry");
                            }
                        }
                    }
                });
            }
        });
    }
}
