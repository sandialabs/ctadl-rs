//! The admissible access paths, frozen for the fixpoint: a per-index, read-only table that
//! answers "is `prefix · [adjust] · rest` a path?" without building `prefix · [adjust] · rest`
//! (spec R8), and hands out each path's splits without interning them (R6).
//!
//! The local propagation rules of the index engine extend an access path across an edge and
//! then test the result against the admissible paths. Most extensions fail the test: measured
//! on a dense binary, the engine built four billion derived paths and kept six percent of them.
//! Building one would intern a node per new segment, mid-run and under a shard lock.
//!
//! [`PathSet`] holds handles only; the segments stay in the global `Path` arena. A path's
//! interned node stores its suffix hash, a fold of its segments onto its tail's hash, so the
//! hash of `prefix · [adjust] · rest` folds the new segments onto `rest`'s stored hash, and one
//! probe of the table settles membership. A hit is confirmed by walking the matched path's
//! segments against the new part and checking that the tail where they end is `rest` itself.
//! A miss means the candidate is not in the set, and nothing is left behind.
//!
//! The set is per index, not global (spec C1): the global interner also holds prefix keys,
//! rests and other indexes' paths, so having a handle is not being admissible.

use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ctadl_ir::mir::{Offset, PathSegment};
use hashbrown::HashTable;

use crate::facts::{Path, suffix_hash};

/// The table hash of a stored 32-bit suffix hash: hashbrown takes its bucket index from the low
/// bits and its tag from the top 7.
#[inline]
fn wide(h: u32) -> u64 {
    h as u64 | (h as u64) << 32
}

/// The splits of one admissible path: see [`PathSet::splits`].
#[derive(Clone, Copy, Default)]
pub struct Splits<'a> {
    /// Every component-wise split `(key, rest)`, from `([], p)` to `(p, [])`.
    pub exact: &'a [(Path, Path)],
    /// The splits before an offset component, `(key, [n]·tail)`.
    pub wild: &'a [(Path, Path)],
}

/// The candidate `body[..n] · [junction] · base`: the first `n` segments of `body`, then an
/// offset segment when `junction` is `Some`, then the existing path `base`. Normalized: no two
/// offsets are adjacent and no offset is zero.
struct Candidate {
    body: Path,
    n: usize,
    junction: Option<i64>,
    base: Path,
}

impl Candidate {
    /// The suffix hash the candidate's node would store, folded from `base`'s.
    #[inline]
    fn hash(&self) -> u32 {
        let mut h = self.base.suffix_hash();
        if let Some(s) = self.junction {
            h = suffix_hash(&PathSegment::Offset(Offset(s)), h);
        }
        fold(self.body, self.n, h)
    }

    /// Whether `p` is the candidate: its first segments are the new part, and the tail after
    /// them is `base`, compared by handle.
    #[inline]
    fn is(&self, mut p: Path) -> bool {
        let mut body = self.body;
        for _ in 0..self.n {
            let (Some((a, p_tail)), Some((b, body_tail))) = (p.uncons(), body.uncons()) else {
                return false;
            };
            if a != b {
                return false;
            }
            (p, body) = (p_tail, body_tail);
        }
        if let Some(s) = self.junction {
            match p.uncons() {
                Some((PathSegment::Offset(Offset(o)), tail)) if *o == s => p = tail,
                _ => return false,
            }
        }
        p == self.base
    }
}

/// `p`'s first `n` segments folded onto `h`, from the last outward. Recursion keeps it off the
/// heap; the caller bounds `n` by the set's deepest member.
fn fold(p: Path, n: usize, h: u32) -> u32 {
    if n == 0 {
        return h;
    }
    let (head, tail) = p.uncons().expect("fold past the end of a path");
    suffix_hash(head, fold(tail, n - 1, h))
}

/// The admissible paths. See the module docs.
///
/// Members are numbered by rank, their order by handle. Each member's precomputed splits sit in
/// arrays indexed by rank, and a handle's rank is one word of the membership bitset plus a
/// popcount.
pub struct PathSet {
    /// Every non-empty member, by stored suffix hash.
    table: HashTable<Path>,
    /// Bit `b` is set when the path whose [`Path::bits`] are `b` is a member.
    bits: Vec<u64>,
    /// The number of members in the words before each word of `bits`.
    ranks: Vec<u32>,
    /// Each member's trailing-offset split `(key, m)`; `m == 0` when it has none, since no path
    /// holds a zero offset.
    trailing: Vec<(Path, i64)>,
    /// Member `r`'s exact splits are `exact[exact_at[r]..exact_at[r + 1]]`.
    exact_at: Vec<u32>,
    exact: Vec<(Path, Path)>,
    /// As `exact_at` and `exact`, for the wild splits.
    wild_at: Vec<u32>,
    wild: Vec<(Path, Path)>,
    /// The most segments any member has. A candidate with a longer new part is not a member.
    max_depth: usize,
    /// Calls to [`PathSet::split_trailing_offset`] on a non-member, each a bug.
    split_misses: AtomicUsize,
}

impl PathSet {
    /// Freezes `paths`. Interns every split key, and so every trailing-offset split, that the
    /// fixpoint will ask for; rests are tails of members, which are interned with them.
    pub fn from_paths(paths: impl IntoIterator<Item = Path>) -> Self {
        let mut members: Vec<Path> = paths.into_iter().collect();
        // Rank order is handle order.
        members.sort_unstable();
        members.dedup();
        let words = members.last().map_or(0, |p| p.bits() as usize / 64 + 1);
        let mut set = PathSet {
            table: HashTable::with_capacity(members.len()),
            bits: vec![0; words],
            ranks: vec![0; words],
            trailing: Vec::with_capacity(members.len()),
            exact_at: Vec::with_capacity(members.len() + 1),
            exact: Vec::new(),
            wild_at: Vec::with_capacity(members.len() + 1),
            wild: Vec::new(),
            max_depth: 0,
            split_misses: AtomicUsize::new(0),
        };
        for &p in &members {
            set.bits[p.bits() as usize / 64] |= 1 << (p.bits() % 64);
            set.exact_at.push(set.exact.len() as u32);
            set.exact.extend(p.prefix_keys());
            set.wild_at.push(set.wild.len() as u32);
            set.wild.extend(p.prefix_keys_wild());
            set.trailing
                .push(p.split_trailing_offset().unwrap_or((Path::EMPTY, 0)));
            set.max_depth = set.max_depth.max(p.len());
            if !p.is_empty() {
                set.table
                    .insert_unique(wide(p.suffix_hash()), p, |q| wide(q.suffix_hash()));
            }
        }
        set.exact_at.push(set.exact.len() as u32);
        set.wild_at.push(set.wild.len() as u32);
        let mut before = 0u32;
        for (rank, word) in set.ranks.iter_mut().zip(&set.bits) {
            *rank = before;
            before += word.count_ones();
        }
        set
    }

    /// `p`'s rank, if it is a member.
    #[inline]
    fn rank(&self, p: Path) -> Option<usize> {
        let b = p.bits() as usize;
        let word = *self.bits.get(b / 64)?;
        let bit = 1u64 << (b % 64);
        (word & bit != 0)
            .then(|| self.ranks[b / 64] as usize + (word & (bit - 1)).count_ones() as usize)
    }

    /// The splits of `p` ([`Path::prefix_keys`] and [`Path::prefix_keys_wild`]), precomputed.
    /// A non-member has none, which is the right answer for a rule that gates on membership.
    #[inline]
    pub fn splits(&self, p: &Path) -> Splits<'_> {
        match self.rank(*p) {
            Some(r) => Splits {
                exact: &self.exact[self.exact_at[r] as usize..self.exact_at[r + 1] as usize],
                wild: &self.wild[self.wild_at[r] as usize..self.wild_at[r + 1] as usize],
            },
            None => Splits::default(),
        }
    }

    /// [`Path::split_trailing_offset`], precomputed. The rules ask it of `locals` paths and edge
    /// source paths, which are members or empty. A non-member is a bug: debug builds panic, and
    /// release builds count it and answer `None`.
    #[inline]
    pub fn split_trailing_offset(&self, p: &Path) -> Option<(Path, i64)> {
        // The empty path need not be a member (codegen can leave it out), and has no split.
        if p.is_empty() {
            return None;
        }
        match self.rank(*p) {
            Some(r) => {
                let (key, m) = self.trailing[r];
                (m != 0).then_some((key, m))
            }
            None => {
                self.split_misses.fetch_add(1, Ordering::Relaxed);
                debug_assert!(false, "split_trailing_offset of a non-member path {p}");
                None
            }
        }
    }

    /// How many times [`PathSet::split_trailing_offset`] was asked about a non-member.
    pub fn split_misses(&self) -> usize {
        self.split_misses.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.trailing.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trailing.is_empty()
    }

    #[inline]
    pub fn contains(&self, p: &Path) -> bool {
        self.rank(*p).is_some()
    }

    /// Heap bytes of the frozen table, its bitset and its splits. The paths themselves are in the
    /// global interner.
    pub fn bytes(&self) -> usize {
        self.table.allocation_size()
            + self.bits.capacity() * size_of::<u64>()
            + self.ranks.capacity() * size_of::<u32>()
            + self.trailing.capacity() * size_of::<(Path, i64)>()
            + (self.exact_at.capacity() + self.wild_at.capacity()) * size_of::<u32>()
            + (self.exact.capacity() + self.wild.capacity()) * size_of::<(Path, Path)>()
    }

    /// The member equal to `prefix · [adjust] · rest`, normalized as [`Path::from_accesses`]
    /// normalizes, if there is one. `[adjust]` is an offset segment, present only when `adjust`
    /// is `Some`. A miss is an ordinary answer.
    ///
    /// Folds the new segments onto `rest`'s stored hash and probes once, so it costs time in the
    /// length of `prefix`, not of the result, and neither interns nor allocates nor locks.
    #[inline]
    pub fn concat(&self, prefix: &Path, adjust: Option<i64>, rest: &Path) -> Option<Path> {
        // Offsets merge only at the junction: `prefix` and `rest` are each normalized.
        let mut len = 0;
        let mut last_off = None;
        let mut p = *prefix;
        while let Some((head, tail)) = p.uncons() {
            len += 1;
            if tail.is_empty() {
                last_off = match head {
                    PathSegment::Offset(Offset(o)) => Some(*o),
                    PathSegment::Symbol(_) => None,
                };
            }
            p = tail;
        }
        let candidate = if last_off.is_some() || adjust.is_some() {
            let rest_off = rest.head_offset();
            let sum = last_off.unwrap_or(0) + adjust.unwrap_or(0) + rest_off.unwrap_or(0);
            Candidate {
                body: *prefix,
                n: len - last_off.is_some() as usize,
                junction: (sum != 0).then_some(sum),
                base: if rest_off.is_some() {
                    rest.tail()
                } else {
                    *rest
                },
            }
        } else {
            Candidate {
                body: *prefix,
                n: len,
                junction: None,
                base: *rest,
            }
        };
        if candidate.n == 0 && candidate.junction.is_none() {
            return self.contains(&candidate.base).then_some(candidate.base);
        }
        if candidate.n + candidate.junction.is_some() as usize > self.max_depth {
            return None;
        }
        let h = candidate.hash();
        self.table
            .find(wide(h), |&p| p.suffix_hash() == h && candidate.is(p))
            .copied()
    }

    /// The member equal to `p` with its prefix `prefix` replaced by `new_prefix`, as
    /// [`Path::substitute_prefix`] would build it, if there is one. A miss, whether `prefix` is
    /// not a prefix of `p` or the result is not a member, is an ordinary answer.
    #[inline]
    pub fn substitute_prefix(&self, p: &Path, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        let (adjust, rest) = crate::facts::match_prefix_split(p, prefix)?;
        self.concat(new_prefix, adjust, &rest)
    }
}

/// A shared [`PathSet`] that can sit in a relation: it compares and hashes by identity, and the
/// program holds exactly one.
#[derive(Clone)]
pub struct PathSetRef(pub Arc<PathSet>);

impl PartialEq for PathSetRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PathSetRef {}
impl std::hash::Hash for PathSetRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.0) as usize);
    }
}
impl std::fmt::Debug for PathSetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PathSet({} paths)", self.0.len())
    }
}
impl std::ops::Deref for PathSetRef {
    type Target = PathSet;
    fn deref(&self) -> &PathSet {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    #[test]
    fn concat_matches_from_accesses() {
        let set = PathSet::from_paths([
            p(".x"),
            p(".x.y"),
            p(".x.[8]"),
            p(".x.[8].y"),
            p(".[4]"),
            p(".[4].deref"),
            Path::empty(),
        ]);
        assert_eq!(set.len(), 7);
        assert!(set.contains(&p(".x.y")));
        assert!(!set.contains(&p(".y")));
        // plain concatenation
        assert_eq!(set.concat(&p(".x"), None, &p(".y")), Some(p(".x.y")));
        assert_eq!(
            set.concat(&Path::empty(), None, &p(".x.y")),
            Some(p(".x.y"))
        );
        assert_eq!(set.concat(&p(".x"), None, &Path::empty()), Some(p(".x")));
        assert_eq!(set.concat(&p(".y"), None, &p(".x")), None);
        // offsets merge at the junction
        assert_eq!(
            set.concat(&p(".x.[3]"), None, &p(".[5]")),
            Some(p(".x.[8]"))
        );
        assert_eq!(
            set.concat(&p(".x.[3]"), Some(5), &Path::empty()),
            Some(p(".x.[8]"))
        );
        assert_eq!(
            set.concat(&p(".x.[3]"), Some(2), &p(".[3].y")),
            Some(p(".x.[8].y"))
        );
        // a zero-sum run disappears
        assert_eq!(
            set.concat(&p(".x.[3]"), Some(-3), &p(".y")),
            Some(p(".x.y"))
        );
        assert_eq!(
            set.concat(&p(".[4]"), Some(-4), &Path::empty()),
            Some(Path::empty())
        );
        assert_eq!(
            set.concat(&p(".[1]"), None, &p(".[3].deref")),
            Some(p(".[4].deref"))
        );
        // and the result is the set's own interned path
        assert_eq!(set.concat(&p(".x"), None, &p(".[8]")), Some(p(".x.[8]")));
    }

    /// Every `(prefix, adjust, rest)` over a small grammar agrees with building the path and
    /// testing it, on a set that holds some of the results and not others.
    #[test]
    fn concat_agrees_with_building_the_path() {
        let parts = [
            "",
            ".x",
            ".y",
            ".[2]",
            ".[-2]",
            ".x.[2]",
            ".[2].x",
            ".x.[-2]",
            ".x.y",
            ".[4].x.[2]",
            ".y.[2].x",
        ];
        let adjusts = [None, Some(2), Some(-2), Some(4)];
        let build = |a: &str, adj: Option<i64>, b: &str| {
            let mut segs: Vec<PathSegment> = p(a).iter().cloned().collect();
            segs.extend(adj.map(|o| PathSegment::Offset(Offset(o))));
            segs.extend(p(b).iter().cloned());
            Path::from_accesses(segs)
        };
        let mut all = Vec::new();
        for a in parts {
            for adj in adjusts {
                for b in parts {
                    all.push(build(a, adj, b));
                }
            }
        }
        all.sort_unstable();
        all.dedup();
        // Every other result is a member, the empty path included or not.
        for parity in [0, 1] {
            let members: Vec<Path> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| i % 2 == parity)
                .map(|(_, p)| *p)
                .collect();
            let set = PathSet::from_paths(members.iter().copied());
            assert_eq!(set.len(), members.len());
            assert!(members.iter().all(|q| set.contains(q)));
            for a in parts {
                for adj in adjusts {
                    for b in parts {
                        let want = build(a, adj, b);
                        let got = set.concat(&p(a), adj, &p(b));
                        let expected = set.contains(&want).then_some(want);
                        assert_eq!(got, expected, "{a} · {adj:?} · {b}");
                    }
                }
            }
        }
    }

    #[test]
    fn substitute_prefix_agrees_with_path() {
        let paths = [
            "", ".x", ".x.y", ".x.[3]", ".x.[3].y", ".x.[5].y", ".[2]", ".z.[1].y", ".z.y",
        ];
        let set = PathSet::from_paths(paths.iter().map(|s| p(s)));
        for ap in paths {
            for pre in paths {
                for np in paths {
                    let want = p(ap)
                        .substitute_prefix(&p(pre), &p(np))
                        .filter(|q| set.contains(q));
                    assert_eq!(
                        set.substitute_prefix(&p(ap), &p(pre), &p(np)),
                        want,
                        "{ap} [{pre} := {np}]"
                    );
                }
            }
        }
    }

    #[test]
    fn splits_are_precomputed_per_member() {
        let set = PathSet::from_paths([p(".x.[4]"), p(".[2].y"), p(".y")]);
        let q = p(".x.[4]");
        assert_eq!(set.splits(&q).exact, q.prefix_keys().as_slice());
        assert_eq!(set.splits(&q).wild, q.prefix_keys_wild().as_slice());
        assert_eq!(set.split_trailing_offset(&q), Some((p(".x"), 4)));
        assert_eq!(set.split_trailing_offset(&p(".[2].y")), None);
        assert!(set.splits(&p(".z")).exact.is_empty());
        // The empty path is answered without being a member.
        assert!(!set.contains(&Path::empty()));
        assert_eq!(set.split_trailing_offset(&Path::empty()), None);
        assert_eq!(set.split_misses(), 0);
        assert!(set.bytes() > 0);
    }
}
