//! Everything known about one variable of one function, keyed by access path.
//!
//! Both BYODS stores in this module keep one *group* per `(function, variable)`. For `locals`
//! ([`super::locals_trie`]) the group is what reaches that variable: each leaf is an access
//! path `p` of the variable together with the formal `a.p4` that reaches `v.p`. For
//! `assign_like` ([`super::assign_like_trie`]) the group is the edges read from that variable:
//! each leaf is a source path `p` together with the destination vertex `v1.p1` that
//! `v1.p1 = v.p` writes. In both, the path is the first component of the leaf, and the local
//! propagation rules probe it exactly: "which formals reach `v.p`", "which edges read `v.p`".
//! So a group has to answer "the leaves at path `p`" without walking every leaf it holds.
//!
//! A [`HybridSet`] over the whole leaf cannot do that: it is keyed on the full tuple, so the
//! only way to find one path's leaves is a scan, and the scan is O(group). On the groups that
//! matter that is catastrophic — a dense binary function puts tens of thousands of leaves under
//! one variable (a call argument of a summary-instantiated call, or a formal that everything
//! reaches), and every exact probe would pay for all of them.
//!
//! So a group has two regimes, switched inside this one type:
//!
//! - **Flat.** At most [`SMALL_THRESHOLD`] leaves, held in one [`HybridSet`] over the full leaf.
//!   That is the overwhelmingly common case (67 to 100% of groups hold exactly one leaf), and it
//!   keeps the group two words wide with one allocation. An exact probe scans, which at this
//!   size is cheaper than a second hash lookup.
//! - **By path.** Above the threshold the leaves move into a map from path to a [`HybridSet`] of
//!   the remaining columns. An exact probe is now one hash lookup, and the leaves shrink by the
//!   width of the path they no longer repeat.
//!
//! The switch is one-way and happens on the insert that would take a flat group past the
//! threshold. Nothing outside this module sees the regime: iteration yields `(&P, &A, &B)`
//! whichever way the group is stored.

use std::hash::Hash;

use super::hybrid_set::{HybridSet, IntoIter as SetIntoIter, Iter as SetIter, SMALL_THRESHOLD};
use super::locals_trie::hb_bytes;

type Map<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;
type Set<T> = HybridSet<T>;

/// The leaves of one group, keyed by their access path `P`. See the module docs.
pub struct PathGroup<P, A, B> {
    inner: Inner<P, A, B>,
}

enum Inner<P, A, B> {
    Flat(Set<(P, A, B)>),
    ByPath {
        map: Map<P, Set<(A, B)>>,
        len: usize,
    },
}

impl<P, A, B> Default for PathGroup<P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<P, A, B> Clone for PathGroup<P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        let inner = match &self.inner {
            Inner::Flat(set) => Inner::Flat(set.clone()),
            Inner::ByPath { map, len } => Inner::ByPath {
                map: map.clone(),
                len: *len,
            },
        };
        Self { inner }
    }
}

impl<P, A, B> PathGroup<P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    /// An empty group. Does not allocate.
    pub fn new() -> Self {
        Self {
            inner: Inner::Flat(Set::new()),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.inner {
            Inner::Flat(set) => set.len(),
            Inner::ByPath { len, .. } => *len,
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True once the group has switched to the by-path regime.
    #[inline]
    pub fn is_large(&self) -> bool {
        matches!(self.inner, Inner::ByPath { .. })
    }

    /// Number of distinct paths in the group. O(group) in the flat regime.
    pub fn num_paths(&self) -> usize {
        match &self.inner {
            Inner::Flat(set) => {
                let mut seen: Vec<&P> = Vec::with_capacity(set.len());
                for (p, _, _) in set.iter() {
                    if !seen.contains(&p) {
                        seen.push(p);
                    }
                }
                seen.len()
            }
            Inner::ByPath { map, .. } => map.len(),
        }
    }

    /// Heap bytes the group holds, load-factor slack included.
    pub fn heap_bytes(&self) -> usize {
        match &self.inner {
            Inner::Flat(set) => set.heap_bytes(),
            Inner::ByPath { map, .. } => {
                hb_bytes(
                    map.capacity(),
                    std::mem::size_of::<(P, Set<(A, B)>)>(),
                ) + map.values().map(Set::heap_bytes).sum::<usize>()
            }
        }
    }

    #[inline]
    pub fn contains(&self, p: &P, a: &A, b: &B) -> bool {
        match &self.inner {
            Inner::Flat(set) => set.contains(&(p.clone(), a.clone(), b.clone())),
            Inner::ByPath { map, .. } => map
                .get(p)
                .is_some_and(|set| set.contains(&(a.clone(), b.clone()))),
        }
    }

    /// Inserts a leaf. Returns true if it was new to the group.
    pub fn insert(&mut self, leaf: (P, A, B)) -> bool {
        match &mut self.inner {
            Inner::Flat(set) => {
                if set.len() < SMALL_THRESHOLD {
                    return set.insert(leaf);
                }
                // The insert that would take a flat group past the threshold promotes it. A
                // duplicate at exactly the threshold promotes a little early, which is harmless.
                self.promote();
                self.insert(leaf)
            }
            Inner::ByPath { map, len } => {
                let (p, a, b) = leaf;
                let added = map.entry(p).or_default().insert((a, b));
                if added {
                    *len += 1;
                }
                added
            }
        }
    }

    fn promote(&mut self) {
        let old = std::mem::replace(
            &mut self.inner,
            Inner::ByPath {
                map: Map::default(),
                len: 0,
            },
        );
        let Inner::Flat(set) = old else {
            unreachable!("promote() called on a by-path group")
        };
        let Inner::ByPath { map, len } = &mut self.inner else {
            unreachable!()
        };
        for (p, a, b) in set {
            if map.entry(p).or_default().insert((a, b)) {
                *len += 1;
            }
        }
    }

    /// Takes the union with `other`, returning how many leaves were new. Costs O(|other|).
    pub fn merge(&mut self, other: Self) -> usize {
        if other.is_empty() {
            return 0;
        }
        if self.is_empty() {
            let n = other.len();
            *self = other;
            return n;
        }
        let mut added = 0;
        for leaf in other {
            if self.insert(leaf) {
                added += 1;
            }
        }
        added
    }

    /// Every leaf, as `(&path, &a, &b)`.
    #[inline]
    pub fn iter(&self) -> Iter<'_, P, A, B> {
        match &self.inner {
            Inner::Flat(set) => Iter::Flat(set.iter()),
            Inner::ByPath { map, .. } => Iter::ByPath {
                outer: map.iter(),
                cur: None,
            },
        }
    }

    /// The leaves at exactly path `p`, or `None` if there are none. In the flat regime this is
    /// one scan of at most [`SMALL_THRESHOLD`] leaves; in the by-path regime it is one hash
    /// lookup.
    #[inline]
    pub fn get(&self, p: &P) -> Option<Get<'_, P, A, B>> {
        match &self.inner {
            Inner::Flat(set) => {
                if !set.iter().any(|(pp, _, _)| pp == p) {
                    return None;
                }
                Some(Get::Flat {
                    it: set.iter(),
                    p: p.clone(),
                })
            }
            Inner::ByPath { map, .. } => map.get(p).map(|set| Get::ByPath(set.iter())),
        }
    }
}

impl<P, A, B> IntoIterator for PathGroup<P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    type Item = (P, A, B);
    type IntoIter = IntoIter<P, A, B>;
    fn into_iter(self) -> IntoIter<P, A, B> {
        match self.inner {
            Inner::Flat(set) => IntoIter::Flat(set.into_iter()),
            Inner::ByPath { map, .. } => IntoIter::ByPath {
                outer: map.into_iter(),
                cur: None,
            },
        }
    }
}

/// Owning iterator over a group's leaves.
pub enum IntoIter<P, A, B> {
    Flat(SetIntoIter<(P, A, B), SMALL_THRESHOLD>),
    ByPath {
        outer: hashbrown::hash_map::IntoIter<P, Set<(A, B)>>,
        cur: Option<(P, SetIntoIter<(A, B), SMALL_THRESHOLD>)>,
    },
}

impl<P, A, B> Iterator for IntoIter<P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    type Item = (P, A, B);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            IntoIter::Flat(it) => it.next(),
            IntoIter::ByPath { outer, cur } => loop {
                if let Some((p, it)) = cur {
                    if let Some((a, b)) = it.next() {
                        return Some((p.clone(), a, b));
                    }
                    *cur = None;
                }
                let (p, set) = outer.next()?;
                *cur = Some((p, set.into_iter()));
            },
        }
    }
}

/// Borrowing iterator over a group's leaves.
pub enum Iter<'a, P, A, B> {
    Flat(SetIter<'a, (P, A, B), SMALL_THRESHOLD>),
    ByPath {
        outer: hashbrown::hash_map::Iter<'a, P, Set<(A, B)>>,
        cur: Option<(&'a P, SetIter<'a, (A, B), SMALL_THRESHOLD>)>,
    },
}

impl<'a, P, A, B> Iterator for Iter<'a, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    type Item = (&'a P, &'a A, &'a B);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Flat(it) => it.next().map(|(p, a, b)| (p, a, b)),
            Iter::ByPath { outer, cur } => loop {
                if let Some((p, it)) = cur {
                    if let Some((a, b)) = it.next() {
                        return Some((p, a, b));
                    }
                    *cur = None;
                }
                let (p, set) = outer.next()?;
                *cur = Some((p, set.iter()));
            },
        }
    }
}

impl<P, A, B> Clone for Iter<'_, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        match self {
            Iter::Flat(it) => Iter::Flat(it.clone()),
            Iter::ByPath { outer, cur } => Iter::ByPath {
                outer: outer.clone(),
                cur: cur.as_ref().map(|(p, it)| (*p, it.clone())),
            },
        }
    }
}

/// The leaves at one path, as `(&a, &b)`.
pub enum Get<'a, P, A, B> {
    Flat {
        it: SetIter<'a, (P, A, B), SMALL_THRESHOLD>,
        p: P,
    },
    ByPath(SetIter<'a, (A, B), SMALL_THRESHOLD>),
}

impl<'a, P, A, B> Iterator for Get<'a, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    type Item = (&'a A, &'a B);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Get::Flat { it, p } => loop {
                let (pp, a, b) = it.next()?;
                if pp == p {
                    return Some((a, b));
                }
            },
            Get::ByPath(it) => it.next().map(|(a, b)| (a, b)),
        }
    }
}

impl<P, A, B> Clone for Get<'_, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        match self {
            Get::Flat { it, p } => Get::Flat {
                it: it.clone(),
                p: p.clone(),
            },
            Get::ByPath(it) => Get::ByPath(it.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regimes_agree() {
        let mut g: PathGroup<u32, u16, u64> = PathGroup::new();
        assert!(g.get(&1).is_none());
        for i in 0..200u32 {
            assert!(g.insert((i % 7, (i % 3) as u16, i as u64)));
            assert!(!g.insert((i % 7, (i % 3) as u16, i as u64)));
        }
        assert_eq!(g.len(), 200);
        assert!(g.is_large());
        assert_eq!(g.num_paths(), 7);
        assert_eq!(g.iter().count(), 200);
        let at3: Vec<_> = g.get(&3).unwrap().collect();
        assert_eq!(at3.len(), 200 / 7 + usize::from(3 < 200 % 7));
        assert!(at3.iter().all(|(_, b)| **b % 7 == 3));
        assert!(g.get(&9).is_none());
        assert!(g.contains(&3, &0, &3));
        assert!(!g.contains(&3, &1, &3));

        let mut small: PathGroup<u32, u16, u64> = PathGroup::new();
        for i in 0..10u32 {
            small.insert((i % 2, 0, i as u64));
        }
        assert!(!small.is_large());
        assert_eq!(small.get(&1).unwrap().count(), 5);
        assert!(small.get(&2).is_none());

        // Only `(0, 0, 0)` is shared between the two groups.
        assert_eq!(g.merge(small), 9);
        let mut other: PathGroup<u32, u16, u64> = PathGroup::new();
        other.insert((100, 100, 100));
        assert_eq!(g.merge(other), 1);
        assert_eq!(g.len(), 210);
        assert_eq!(g.into_iter().count(), 210);
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use std::collections::HashSet;

    /// A cheap deterministic generator, so the test needs no dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// Random inserts, merges and probes against a plain set, across both regimes.
    #[test]
    fn agrees_with_a_set_model() {
        let mut rng = Lcg(7);
        for round in 0..200 {
            let paths = 1 + (rng.next() % 40) as u32;
            let mut g: PathGroup<u32, u16, u64> = PathGroup::new();
            let mut model: HashSet<(u32, u16, u64)> = HashSet::new();
            let mut other: PathGroup<u32, u16, u64> = PathGroup::new();
            let mut other_model: HashSet<(u32, u16, u64)> = HashSet::new();
            let n = (rng.next() % 300) as usize;
            for _ in 0..n {
                let leaf = ((rng.next() % paths as u64) as u32, (rng.next() % 5) as u16, rng.next() % 50);
                if rng.next().is_multiple_of(3) {
                    assert_eq!(other.insert(leaf), other_model.insert(leaf), "round {round}");
                } else {
                    assert_eq!(g.insert(leaf), model.insert(leaf), "round {round}");
                }
                assert_eq!(g.len(), model.len());
                let probe = ((rng.next() % paths as u64) as u32, (rng.next() % 5) as u16, rng.next() % 50);
                assert_eq!(g.contains(&probe.0, &probe.1, &probe.2), model.contains(&probe));
            }
            let before = model.len();
            let added = g.merge(other);
            model.extend(other_model.iter().copied());
            assert_eq!(added, model.len() - before, "round {round}");
            assert_eq!(g.len(), model.len());
            let all: HashSet<_> = g.iter().map(|(p, a, b)| (*p, *a, *b)).collect();
            assert_eq!(all, model, "round {round}");
            for p in 0..paths {
                let expect: HashSet<_> = model.iter().filter(|l| l.0 == p).map(|l| (l.1, l.2)).collect();
                let got: HashSet<_> = g.get(&p).into_iter().flatten().map(|(a, b)| (*a, *b)).collect();
                assert_eq!(got, expect, "round {round} path {p}");
                assert_eq!(g.get(&p).is_some(), !expect.is_empty());
            }
            assert_eq!(g.num_paths(), model.iter().map(|l| l.0).collect::<HashSet<_>>().len());
            let owned: HashSet<_> = g.into_iter().collect();
            assert_eq!(owned, model);
        }
    }
}
