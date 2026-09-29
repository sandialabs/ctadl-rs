//! Decisions and decision sets: what the contextual relations of the index engine are keyed
//! by.
//!
//! A [`Decision`] names *what* a resolvent decided at a function -- "formal `n.p` holds call
//! target `t`" -- and never the route it arrived by. A [`DecisionSet`] is the lattice column of
//! the contextual relations: a contextual row `(f, v, p, a, p4)` holds under every decision in
//! its set, and a row derived from it inherits the set. Two decisions whose flows meet at a row
//! share the row instead of duplicating it, so the contextual relations are bounded by the
//! context-free ones (one row per `(f, v, p, a, p4)`), as the old call-string lattice bounded
//! them, without dropping any decision the way the call-string lattice did.
//!
//! Decisions are interned to a dense [`DecisionId`] and sets are interned as sorted slices, so
//! a set is a `Copy` pointer, equality is a pointer compare, and rows that hold the same set
//! share one allocation. Both interners are process-global and never free: a decision or a set
//! lives as long as the process, which is the price of hash-consing and is measured by
//! [`stats`].

use std::fmt::{self, Display};
use std::sync::atomic::{AtomicUsize, Ordering};
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Decision {
    pub formal: FormalIndex,
    pub path: Path,
    pub target: CallTargetObject,
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

/// A set of decisions, interned as a sorted, duplicate-free slice. `Copy`; equal sets are
/// pointer-equal. The lattice order is inclusion and the join is union.
///
/// A set is either *exact* or *collapsing*, and the two never meet (one kind per run). An exact
/// set is what it says. A collapsing set holds at most one decision: joining two different
/// singletons gives [`DecisionSet::top`], "every decision of this function", which is above
/// everything and absorbs every join. That is the flat lattice `⊥ < {d} < ⊤` -- a coarser,
/// parameter-free abstraction under which a row changes at most once after it is created,
/// where an exact set at a row changes once per decision that reaches the row after it.
#[derive(Clone, Copy)]
pub struct DecisionSet {
    ids: &'static [DecisionId],
    collapse: bool,
}

/// The address that marks ⊤; never dereferenced as a set of decisions.
static TOP_MARK: [DecisionId; 1] = [DecisionId(u32::MAX)];

static SETS: LazyLock<immortal::Interner<[DecisionId]>> =
    LazyLock::new(|| immortal::Interner::new(64));
/// The widening bound on exact sets: an exact union with more members than this is ⊤. `0` is
/// no bound. Process-global like the interners (one kind of set per run); set by
/// [`set_widen_bound`] before the run.
static WIDEN_BOUND: AtomicUsize = AtomicUsize::new(0);

/// Sets the widening bound for exact sets (`HybridContext::Bounded(k)`); `0` removes it.
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

/// Unions computed (both operands non-trivial), and how many of those produced a new set.
static UNIONS: AtomicUsize = AtomicUsize::new(0);
static UNIONS_GREW: AtomicUsize = AtomicUsize::new(0);
/// The union memo: a direct-mapped cache from a pair of set addresses to the union's slice,
/// shared by every thread without a lock. Each slot is a seqlock -- a writer takes the slot
/// by moving its sequence to odd, fills it, and releases it even; a reader accepts a slot only
/// if the sequence was even before and unchanged after -- so a torn entry is never returned,
/// and a writer that loses the race simply does not cache. Sets are interned, so the same two
/// addresses meet again and again (every row's set against the set of every row that feeds
/// it), and the lookup is the hot path of the whole contextual closure: a thread-local hash
/// map here cost more than the joins around it.
struct UnionSlot {
    seq: AtomicUsize,
    a: AtomicUsize,
    b: AtomicUsize,
    ptr: AtomicUsize,
    len: AtomicUsize,
}

const UNION_CACHE_BITS: u32 = 20;
static UNION_CACHE: LazyLock<Box<[UnionSlot]>> = LazyLock::new(|| {
    (0..1usize << UNION_CACHE_BITS)
        .map(|_| UnionSlot {
            seq: AtomicUsize::new(0),
            a: AtomicUsize::new(0),
            b: AtomicUsize::new(0),
            ptr: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
        })
        .collect()
});

fn union_slot(a: usize, b: usize) -> &'static UnionSlot {
    // Fibonacci hashing of the pair; the addresses are 16-byte aligned so the low bits carry
    // nothing.
    let h = (a.rotate_left(17) ^ b).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    &UNION_CACHE[h >> (usize::BITS - UNION_CACHE_BITS)]
}

fn union_cache_get(a: usize, b: usize) -> Option<&'static [DecisionId]> {
    let slot = union_slot(a, b);
    let s1 = slot.seq.load(Ordering::Acquire);
    if s1 & 1 == 1 {
        return None;
    }
    let (sa, sb) = (
        slot.a.load(Ordering::Relaxed),
        slot.b.load(Ordering::Relaxed),
    );
    let (ptr, len) = (
        slot.ptr.load(Ordering::Relaxed),
        slot.len.load(Ordering::Relaxed),
    );
    std::sync::atomic::fence(Ordering::Acquire);
    if slot.seq.load(Ordering::Relaxed) != s1 || sa != a || sb != b {
        return None;
    }
    // SAFETY: every (ptr, len) stored by `union_cache_put` came from an interned slice, which
    // lives for the rest of the process; the seqlock protocol guarantees the four fields were
    // read from one consistent write.
    Some(unsafe { std::slice::from_raw_parts(ptr as *const DecisionId, len) })
}

fn union_cache_put(a: usize, b: usize, result: &'static [DecisionId]) {
    let slot = union_slot(a, b);
    let s = slot.seq.load(Ordering::Relaxed);
    if s & 1 == 1
        || slot
            .seq
            .compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    slot.a.store(a, Ordering::Relaxed);
    slot.b.store(b, Ordering::Relaxed);
    slot.ptr.store(result.as_ptr() as usize, Ordering::Relaxed);
    slot.len.store(result.len(), Ordering::Relaxed);
    slot.seq.store(s + 2, Ordering::Release);
}

impl DecisionSet {
    fn intern(sorted: &[DecisionId], collapse: bool) -> DecisionSet {
        debug_assert!(sorted.windows(2).all(|w| w[0] < w[1]));
        DecisionSet {
            ids: SETS.intern(sorted),
            collapse,
        }
    }

    /// The empty set: the lattice bottom (of either kind; it joins into anything).
    pub fn empty() -> DecisionSet {
        static EMPTY: LazyLock<DecisionSet> = LazyLock::new(|| DecisionSet::intern(&[], false));
        *EMPTY
    }

    /// `{d}`, exact or collapsing.
    pub fn singleton(d: DecisionId, collapse: bool) -> DecisionSet {
        DecisionSet::intern(&[d], collapse)
    }

    /// ⊤: every decision of the function the row belongs to. A collapsing join makes it, and
    /// so does an exact union that passes the widening bound (`top_exact`).
    pub fn top() -> DecisionSet {
        DecisionSet {
            ids: &TOP_MARK,
            collapse: true,
        }
    }

    /// ⊤ of the exact kind: what a bounded exact union widens to. Absorbs every exact join.
    pub fn top_exact() -> DecisionSet {
        DecisionSet {
            ids: &TOP_MARK,
            collapse: false,
        }
    }

    pub fn is_top(self) -> bool {
        std::ptr::eq(self.ids, &TOP_MARK)
    }

    /// Whether this is a collapsing set (see the type's doc).
    pub fn is_collapsing(self) -> bool {
        self.collapse
    }

    pub fn is_empty(self) -> bool {
        self.ids.is_empty()
    }

    /// Elements of an explicit set; 0 for ⊤, which is not an explicit set.
    pub fn len(self) -> usize {
        if self.is_top() { 0 } else { self.ids.len() }
    }

    /// The explicit elements; empty for ⊤, whose members are implicit.
    pub fn ids(self) -> impl Iterator<Item = DecisionId> + 'static {
        let ids: &'static [DecisionId] = if self.is_top() { &[] } else { self.ids };
        ids.iter().copied()
    }

    /// The decisions in an explicit set, looked up once per element.
    pub fn decisions(self) -> Vec<Decision> {
        let table = DECISIONS.read().unwrap();
        self.ids()
            .map(|id| table.decisions[id.0 as usize].clone())
            .collect()
    }

    pub fn contains(self, d: DecisionId) -> bool {
        self.is_top() || self.ids.binary_search(&d).is_ok()
    }

    /// `self ⊆ other`, by a merge walk (both sorted).
    pub fn is_subset(self, other: DecisionSet) -> bool {
        if std::ptr::eq(self.ids, other.ids) || self.ids.is_empty() || other.is_top() {
            return true;
        }
        if self.is_top() || self.ids.len() > other.ids.len() {
            return false;
        }
        let (a, b) = (self.ids, other.ids);
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

    /// `self ∪ other`, interned. Returns `self` itself (pointer-equal) when nothing is added.
    /// Between collapsing sets, two different singletons give ⊤.
    pub fn union(self, other: DecisionSet) -> DecisionSet {
        if std::ptr::eq(self.ids, other.ids) || other.ids.is_empty() {
            return self;
        }
        if self.ids.is_empty() {
            return other;
        }
        if self.collapse || other.collapse {
            debug_assert!(self.collapse && other.collapse);
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
        let (a, b) = {
            let (a, b) = (self.ids.as_ptr() as usize, other.ids.as_ptr() as usize);
            if a < b { (a, b) } else { (b, a) }
        };
        let result = match union_cache_get(a, b) {
            Some(ids) => DecisionSet {
                ids,
                collapse: self.collapse,
            },
            None => {
                let result = self.union_uncached(other);
                union_cache_put(a, b, result.ids);
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
        let (a, b) = (self.ids, other.ids);
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
        if std::ptr::eq(self.ids, other.ids) || self.is_top() {
            return other;
        }
        if other.is_top() {
            return self;
        }
        let out: Vec<DecisionId> = self
            .ids
            .iter()
            .copied()
            .filter(|d| other.contains(*d))
            .collect();
        DecisionSet::intern(&out, self.collapse)
    }
}

impl PartialEq for DecisionSet {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.ids, other.ids)
    }
}
impl Eq for DecisionSet {}

impl std::hash::Hash for DecisionSet {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.ids.as_ptr() as usize).hash(state)
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
        f.debug_list().entries(self.ids.iter()).finish()
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
        "decisions={} set unions={} (grew {})",
        DecisionId::count(),
        UNIONS.load(Ordering::Relaxed),
        UNIONS_GREW.load(Ordering::Relaxed),
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
}
