//! The admissible access paths, as a set that answers "is `prefix · rest` a path?" without
//! building `prefix · rest`.
//!
//! The local propagation rules of the index engine extend an access path across an edge and
//! then test the result against `paths`, the set of syntactic program paths. Most extensions
//! fail the test: measured on a dense binary, the engine built four billion derived paths and
//! kept six percent of them. Building one means allocating a buffer and interning a cons cell
//! per prefix component, so the rejected paths were the bulk of the run.
//!
//! [`PathSet`] turns that into a lookup on components that already exist. It hashes the
//! *virtual* concatenation — walking the prefix, an optional offset adjustment and the rest
//! with the same offset-merging normalization `Path::from_accesses` applies — probes a table
//! keyed on that hash, and confirms a hit by comparing components. A hit hands back the set's
//! own interned `Path`, so nothing is allocated in either case.

use std::hash::Hasher;
use std::sync::Arc;

use ctadl_ir::mir::{Offset, PathSegment};
use rustc_hash::FxHasher;
use smallvec::SmallVec;

use crate::facts::Path;

type Map<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;

/// One component of a virtual, normalized path: a borrowed symbol or a merged offset.
#[derive(Clone, Copy)]
enum Seg<'a> {
    Sym(&'a PathSegment),
    Off(i64),
}

impl PartialEq<PathSegment> for Seg<'_> {
    fn eq(&self, other: &PathSegment) -> bool {
        match (self, other) {
            (Seg::Sym(a), b) => *a == b,
            (Seg::Off(a), PathSegment::Offset(Offset(b))) => a == b,
            _ => false,
        }
    }
}

/// Walks components and yields what [`Path::from_accesses`] would keep: runs of adjacent
/// offsets summed into one, and zero-sum runs dropped. Every `Path` is already in that form, so
/// on a single path this is the identity; across a junction of two paths it performs exactly
/// the merge `prepend_onto` performs.
struct Normalized<'a, I> {
    it: I,
    pending: i64,
    stash: Option<Seg<'a>>,
}

impl<'a, I: Iterator<Item = Seg<'a>>> Iterator for Normalized<'a, I> {
    type Item = Seg<'a>;
    #[inline]
    fn next(&mut self) -> Option<Seg<'a>> {
        if let Some(s) = self.stash.take() {
            return Some(s);
        }
        loop {
            match self.it.next() {
                Some(Seg::Sym(PathSegment::Offset(Offset(o)))) => self.pending += o,
                Some(Seg::Off(o)) => self.pending += o,
                Some(seg) => {
                    if self.pending != 0 {
                        let o = std::mem::take(&mut self.pending);
                        self.stash = Some(seg);
                        return Some(Seg::Off(o));
                    }
                    return Some(seg);
                }
                None => {
                    if self.pending != 0 {
                        return Some(Seg::Off(std::mem::take(&mut self.pending)));
                    }
                    return None;
                }
            }
        }
    }
}

#[inline]
fn normalized<'a, I: Iterator<Item = Seg<'a>>>(it: I) -> Normalized<'a, I> {
    Normalized {
        it,
        pending: 0,
        stash: None,
    }
}

/// Hashes a normalized component sequence. This is the set's own hash, not `PathSegment`'s:
/// the table is built and probed through this one function, so it only has to be
/// deterministic. A symbol hashes by its interned pointer, as `Symbol` itself does.
#[inline]
fn hash_segs<'a>(segs: impl Iterator<Item = Seg<'a>>) -> u64 {
    let mut h = FxHasher::default();
    for seg in segs {
        match seg {
            Seg::Sym(PathSegment::Symbol(s)) => {
                h.write_u8(1);
                h.write_usize(s.as_ptr() as usize);
            }
            Seg::Sym(PathSegment::Offset(Offset(o))) => {
                h.write_u8(2);
                h.write_i64(*o);
            }
            Seg::Off(o) => {
                h.write_u8(2);
                h.write_i64(o);
            }
        }
    }
    h.finish()
}

#[inline]
fn eq_path<'a>(path: &Path, segs: impl Iterator<Item = Seg<'a>>) -> bool {
    let mut it = path.iter();
    for seg in segs {
        match it.next() {
            Some(c) if seg == *c => {}
            _ => return false,
        }
    }
    it.next().is_none()
}

/// The splits of one admissible path, computed once: see [`PathSet::splits`].
#[derive(Default)]
pub struct Splits {
    /// Every component-wise split `(key, rest)`, from `([], p)` to `(p, [])`.
    pub exact: Vec<(Path, Path)>,
    /// The splits before an offset component, `(key, [n]·tail)`.
    pub wild: Vec<(Path, Path)>,
}

/// The admissible paths. See the module docs.
pub struct PathSet {
    table: Map<u64, SmallVec<[Path; 1]>>,
    /// The splits of every admissible path, keyed by the path. Every path a rule splits — an
    /// edge's source path, a `locals` path — is admissible, so this is total over what the
    /// rules ask, and a split costs a lookup instead of interning each prefix anew.
    splits: Map<Path, Splits>,
    len: usize,
}

impl PathSet {
    pub fn from_paths(paths: impl IntoIterator<Item = Path>) -> Self {
        let mut set = PathSet {
            table: Map::default(),
            splits: Map::default(),
            len: 0,
        };
        for p in paths {
            let h = hash_segs(normalized(p.iter().map(Seg::Sym)));
            let bucket = set.table.entry(h).or_default();
            if !bucket.contains(&p) {
                bucket.push(p);
                set.splits.insert(
                    p,
                    Splits {
                        exact: p.prefix_keys(),
                        wild: p.prefix_keys_wild(),
                    },
                );
                set.len += 1;
            }
        }
        set
    }

    /// The splits of `p` — [`Path::prefix_keys`] and [`Path::prefix_keys_wild`], precomputed.
    /// `p` must be admissible; an inadmissible path has no splits, which is also the right
    /// answer for a rule that gates on membership.
    #[inline]
    pub fn splits(&self, p: &Path) -> &Splits {
        static NONE: std::sync::LazyLock<Splits> = std::sync::LazyLock::new(Splits::default);
        self.splits.get(p).unwrap_or(&NONE)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn contains(&self, p: &Path) -> bool {
        let h = hash_segs(normalized(p.iter().map(Seg::Sym)));
        self.table.get(&h).is_some_and(|b| b.contains(p))
    }

    /// The admissible path equal to `prefix · [adjust] · rest`, if there is one, where `[adjust]`
    /// is an offset component that is present only when `adjust` is `Some`. Equivalent to
    /// `Path::from_accesses(prefix ++ adjust ++ rest)` followed by a membership test, without
    /// building the path.
    #[inline]
    pub fn concat(&self, prefix: &Path, adjust: Option<i64>, rest: &Path) -> Option<Path> {
        let segs = || {
            normalized(
                prefix
                    .iter()
                    .map(Seg::Sym)
                    .chain(adjust.map(Seg::Off))
                    .chain(rest.iter().map(Seg::Sym)),
            )
        };
        let h = hash_segs(segs());
        let bucket = self.table.get(&h)?;
        bucket.iter().find(|p| eq_path(p, segs())).copied()
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
        assert_eq!(set.concat(&Path::empty(), None, &p(".x.y")), Some(p(".x.y")));
        assert_eq!(set.concat(&p(".x"), None, &Path::empty()), Some(p(".x")));
        assert_eq!(set.concat(&p(".y"), None, &p(".x")), None);
        // offsets merge at the junction
        assert_eq!(set.concat(&p(".x.[3]"), None, &p(".[5]")), Some(p(".x.[8]")));
        assert_eq!(set.concat(&p(".x.[3]"), Some(5), &Path::empty()), Some(p(".x.[8]")));
        assert_eq!(set.concat(&p(".x.[3]"), Some(2), &p(".[3].y")), Some(p(".x.[8].y")));
        // a zero-sum run disappears
        assert_eq!(set.concat(&p(".x.[3]"), Some(-3), &p(".y")), Some(p(".x.y")));
        assert_eq!(set.concat(&p(".[4]"), Some(-4), &Path::empty()), Some(Path::empty()));
        assert_eq!(set.concat(&p(".[1]"), None, &p(".[3].deref")), Some(p(".[4].deref")));
        // and the result is the set's own interned path
        assert_eq!(set.concat(&p(".x"), None, &p(".[8]")), Some(p(".x.[8]")));
    }
}
