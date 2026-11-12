//! Data types for facts

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display};
use std::num::NonZeroU32;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::LazyLock;

use ctadl_ir::Symbol;
use derive_builder::Builder;
use internment::ArcIntern;
use packed_struct::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::error::{Error, ErrorContext};
use ctadl_ir::{Idx, cir, cir::Offset};

pub mod parquet;
pub mod schema;

immortal::id_handle! {
    /// An interned string: a 4-byte handle into a process-wide interner that never frees.
    /// Ordered by id, so sort with `content_cmp` wherever the order is printed. Serde writes the
    /// string and interns it on reading.
    pub struct Str(str);
}

impl Default for Str {
    fn default() -> Self {
        *EMPTY_STR
    }
}

impl Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for Str {
    fn from(s: &str) -> Self {
        Str::intern(s)
    }
}

impl From<String> for Str {
    fn from(s: String) -> Self {
        Str::intern(&s)
    }
}

impl From<ArcIntern<str>> for Str {
    fn from(s: ArcIntern<str>) -> Self {
        Str::intern(&s)
    }
}

impl Str {
    pub fn as_str(&self) -> &'static str {
        self.get()
    }

    /// Orders by the string, not by id, so a sort that feeds output is reproducible.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self == other {
            return std::cmp::Ordering::Equal;
        }
        self.as_str().cmp(other.as_str())
    }
}

impl Serialize for Str {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Str {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrVisitor;
        impl serde::de::Visitor<'_> for StrVisitor {
            type Value = Str;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Str, E> {
                Ok(Str::intern(s))
            }
        }
        deserializer.deserialize_str(StrVisitor)
    }
}

impl Deref for Str {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

type EltId = Str;

lazy_static::lazy_static! {
    pub static ref EMPTY_STR: Str = Str::from("");
}

immortal::id_handle! {
    /// A handle to an interned [`cir::PathSegment`], the head of a [`PathNode`].
    pub struct SegmentId(cir::PathSegment) in 16 shards;
}

/// One cons cell of an interned [`Path`]: the handle of its first segment and of the rest.
///
/// Every suffix of an interned path is itself interned, so a path's tail is one arena read.
/// Equality reads `head` and `tail` only, and hashing reads `hash` only, which is a function of
/// them.
///
/// Three `u32`s with no pointer, so a file can hold nodes that a process maps as its interner's
/// base ([`crate::index_engine::path_set`]): `tail` is 0 for `None`.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct PathNode {
    head: SegmentId,
    tail: Option<PathId>,
    /// The suffix hash `mix(head, tail's hash)`, so the hash of `segments · rest` folds from
    /// `rest`'s without walking it.
    hash: u32,
}

impl PartialEq for PathNode {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.tail == other.tail && self.head == other.head
    }
}

impl Eq for PathNode {}

impl std::hash::Hash for PathNode {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.hash);
    }
}

immortal::id_handle! {
    /// A handle to an interned, non-empty [`PathNode`]. [`Path`] wraps it.
    pub struct PathId(PathNode);
}

/// The suffix hash of the empty path.
pub(crate) const EMPTY_PATH_HASH: u32 = 0;

/// A [`PathNode`] as a file holds it: the bits of its head's [`SegmentId`], of its tail's
/// [`PathId`] (0 for none) and its suffix hash.
pub(crate) type RawNode = [u32; 3];

/// Hashes as a [`PathNode`] whose suffix hash it holds does, so an index over a file's nodes can
/// be built from their [`RawNode`]s.
struct NodeHash(u32);

impl std::hash::Hash for NodeHash {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.0);
    }
}

/// The hash index of a file's path nodes for [`install_path_base`]. Node 0 is the placeholder
/// that holds [`Path::EMPTY`]'s id, so the index leaves it out.
pub(crate) fn path_base_index(nodes: &[RawNode]) -> Vec<u64> {
    let hashes: Vec<NodeHash> = nodes.iter().map(|n| NodeHash(n[2])).collect();
    immortal::base_index(&hashes, 1)
}

/// Whether this process can take a path base: it has interned no path and no segment.
pub fn can_install_path_base() -> bool {
    PathId::stats().entries == 0 && SegmentId::stats().entries == 0
}

/// Makes `segments` segments `1..=K` and `nodes` paths `1..=N` of this process, as numbered in
/// the file they came from, so that a path's bits there are its bits here. `nodes[0]`, whose bits
/// are [`Path::EMPTY`]'s, is a placeholder no lookup finds. `node_index` is
/// [`path_base_index`] of `nodes`. Every path interned afterwards gets bits above `N`.
///
/// Fails, installing nothing, if this process has interned a path or a segment, or if the segments
/// repeat, or if a node names a segment that is not one of `segments`, or a tail that does not come
/// before it.
pub(crate) fn install_path_base(
    segments: Vec<cir::PathSegment>,
    nodes: &'static [RawNode],
    node_index: &'static [u64],
) -> Result<(), String> {
    if !can_install_path_base() {
        return Err("this process has interned paths already".to_string());
    }
    let k = segments.len();
    if segments.iter().collect::<hashbrown::HashSet<_>>().len() != k {
        return Err("a segment repeats".to_string());
    }
    if !nodes.is_empty() && !node_index.len().is_power_of_two() {
        return Err("the node index's length is not a power of two".to_string());
    }
    for (i, &[head, tail, _]) in nodes.iter().enumerate() {
        // Node `i` has bits `i + 1`, and its tail must be an earlier node other than the
        // placeholder.
        if !(1..=k).contains(&(head as usize)) || !(tail == 0 || (2..=i as u32).contains(&tail)) {
            return Err(format!(
                "node {} has segment {head} of {k} and tail {tail}",
                i + 1
            ));
        }
    }
    let segment_index = immortal::base_index(&segments, 0);
    SegmentId::interner().install_base(Vec::leak(segments), Vec::leak(segment_index));
    if !nodes.is_empty() {
        // SAFETY: `PathNode` is `repr(C)` with three 4-byte fields, as `RawNode` is: `head` a
        // `NonZeroU32` that each node's is (checked above), `tail` an `Option` of one whose
        // `None` is 0, and `hash` a `u32`. Every `head` is a segment of the base just
        // installed, and every `tail` a node before it, so each is an id the interners issue.
        let nodes: &'static [PathNode] =
            unsafe { std::slice::from_raw_parts(nodes.as_ptr().cast(), nodes.len()) };
        PathId::interner().install_base(nodes, node_index);
    }
    Ok(())
}

/// The suffix hash of `head · tail`, given `tail`'s. It hashes the segment's id, not the segment,
/// so it is the same in every process that numbers segments the same; a `cir::Symbol` hashes by
/// address.
#[inline]
pub(crate) fn suffix_hash(head: SegmentId, tail_hash: u32) -> u32 {
    suffix_hash_raw(head.raw(), tail_hash)
}

/// [`suffix_hash`] of the segment whose id has bits `head`.
#[inline]
pub(crate) fn suffix_hash_raw(head: u32, tail_hash: u32) -> u32 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write_u32(head);
    h.write_u32(tail_hash);
    // FxHash mixes its low output bits poorly; fold a full multiply over it.
    let m = (h.finish() as u128).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    ((m >> 64) as u64 ^ m as u64) as u32
}

/// A path: a sequence of field/array accesses, with no variable.
///
/// A path is the segment part of an access path (see [`cir::AccessPath`]); [`FlowVertex`] pairs
/// one with a variable. It is represented as a stack of accesses, innermost first. So the path of
/// the access path x.foo.bar.baz, `.foo.bar.baz`, is represented as
/// `cons(.foo, cons(.bar, cons(.baz)))`. Each cons cell is a
/// [`PathNode`] interned in a process-wide interner that never frees, so common suffixes are
/// shared and a `Path` is a 4-byte handle: a [`PathId`]'s bits, or for the empty path a reserved
/// value no `PathId` has, so [`Path::EMPTY`] is a compile-time constant and `Option<Path>` is 4
/// bytes too.
///
/// `Eq` and `Hash` act on the handle. So does `Ord`, which is therefore not reproducible across
/// runs: sort with [`Path::content_cmp`] wherever the order is printed. serde, `Display` and
/// [`Path::parse`] go through the content.
///
/// A `Path` is always in the normal form of [`cir::path`]: contiguous runs of offset accesses are
/// summed, so there is never more than one offset access in a row, and `#value` absorbs itself and
/// adjacent offsets. In effect, the offsets are "addresses of" the containing field.
///
/// A zero offset is dropped: it denotes the address of the field it is applied to, so `.x.[0]` and
/// `.x` are the same address and must be the same path. Keeping them distinct would let the two
/// spellings of one address miss each other -- a callee that derefs a pointer parameter yields
/// `Argument(0).[0].deref`, while the caller's own storage for that object is `.[k].deref`, and the
/// summary would never match.
#[derive(Clone, Copy, Eq, PartialEq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Path(NonZeroU32);

impl std::hash::Hash for Path {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.0.get());
    }
}

impl Default for Path {
    fn default() -> Self {
        Path::EMPTY
    }
}

impl Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Path({:?})", self.to_dot_string())
    }
}

impl Path {
    /// The empty path. Its bits are no `PathId`'s: an interner with more than one shard never
    /// issues an id whose local bits are zero.
    pub const EMPTY: Path = Path(NonZeroU32::MIN);

    #[inline]
    fn from_id(id: Option<PathId>) -> Path {
        id.map_or(Path::EMPTY, |id| Path(NonZeroU32::new(id.raw()).unwrap()))
    }

    #[inline]
    fn id(self) -> Option<PathId> {
        // SAFETY: a `Path` other than `EMPTY` holds the bits of a `PathId` that `from_id` was
        // given, or that the LSM provider copied out of such a `Path`.
        (self != Path::EMPTY).then(|| unsafe { PathId::from_raw_unchecked(self.0.get()) })
    }

    /// `head · tail`, which the caller has normalized: `head` and `tail`'s head are not both
    /// offsets, and `head` is not a zero offset.
    #[inline]
    fn cons(head: cir::PathSegment, tail: Path) -> Path {
        Path::cons_id(SegmentId::intern(&head), tail)
    }

    /// [`Path::cons`] of a segment already interned.
    #[inline]
    fn cons_id(head: SegmentId, tail: Path) -> Path {
        let hash = suffix_hash(head, tail.suffix_hash());
        Path::from_id(Some(PathId::intern(&PathNode {
            head,
            tail: tail.id(),
            hash,
        })))
    }

    #[inline]
    fn node(self) -> Option<&'static PathNode> {
        self.id().map(PathId::get)
    }

    /// The hash [`PathNode`] stores for this path: [`suffix_hash`] folded over its segments.
    #[inline]
    pub(crate) fn suffix_hash(self) -> u32 {
        self.node().map_or(EMPTY_PATH_HASH, |n| n.hash)
    }

    /// The first segment and the rest, in one arena read. `None` for the empty path.
    #[inline]
    pub(crate) fn uncons(self) -> Option<(&'static cir::PathSegment, Path)> {
        self.node().map(|n| (n.head.get(), Path::from_id(n.tail)))
    }

    /// As [`Path::uncons`], with the first segment's handle.
    #[inline]
    pub(crate) fn uncons_id(self) -> Option<(SegmentId, Path)> {
        self.node().map(|n| (n.head, Path::from_id(n.tail)))
    }

    /// The handles of this path's segments, from the innermost out.
    fn segment_ids(self) -> Vec<SegmentId> {
        let mut out = Vec::new();
        let mut p = self;
        while let Some((head, tail)) = p.uncons_id() {
            out.push(head);
            p = tail;
        }
        out
    }

    /// The path of `segments`, which are a prefix of a path's segments, and so normal already:
    /// the normal form's equations are between neighbors only.
    fn from_prefix(segments: &[SegmentId]) -> Path {
        segments
            .iter()
            .rev()
            .fold(Path::EMPTY, |tail, &head| Path::cons_id(head, tail))
    }

    /// This path without its last segment. The empty path for the empty path.
    pub fn without_last(&self) -> Path {
        let comps = self.segment_ids();
        Path::from_prefix(&comps[..comps.len().saturating_sub(1)])
    }

    /// The path whose bits are `raw`, unchecked.
    ///
    /// # Safety
    ///
    /// `raw` is 1, [`Path::EMPTY`]'s bits, or the bits of a [`PathId`] this process issued.
    #[inline]
    pub(crate) unsafe fn from_raw_unchecked(raw: u32) -> Path {
        // SAFETY: neither is 0.
        Path(unsafe { NonZeroU32::new_unchecked(raw) })
    }

    /// Creates a path from a sequence of accesses
    /// The segments are put in normal form ([`cir::path::normalize`]), so every `Path` is normal.
    pub fn from_accesses(iter: impl IntoIterator<Item = cir::PathSegment>) -> Self {
        let items = cir::path::normalize(iter.into_iter().inspect(|item| {
            debug_assert!(
                !matches!(item, cir::PathSegment::Field(s) if s.is_empty()),
                "Field(\"\") is not a representable path segment"
            );
        }));
        items
            .into_iter()
            .rev()
            .fold(Path::EMPTY, |tail, head| Path::cons(head, tail))
    }

    /// Creates an empty path
    #[inline]
    pub const fn empty() -> Self {
        Path::EMPTY
    }

    /// Denotes the empty path
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        *self == Path::EMPTY
    }

    /// Len is O(n)
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether this handle was handed out by the interner, for handles read back from storage
    /// that bypassed it. Always true for a `Path` made by safe code.
    pub fn is_valid(&self) -> bool {
        self.is_empty() || PathId::from_raw(self.0.get()).is_some()
    }

    /// The handle's bits: [`Path::EMPTY`]'s, which are 1, or the [`PathId`]'s. Distinct paths
    /// have distinct bits.
    #[inline]
    pub fn raw(self) -> u32 {
        self.0.get()
    }

    /// The path whose bits are `raw`, if it is the empty path's or a handed-out [`PathId`]'s.
    pub fn from_raw(raw: u32) -> Option<Path> {
        if raw == Path::EMPTY.raw() {
            return Some(Path::EMPTY);
        }
        PathId::from_raw(raw).map(|id| Path::from_id(Some(id)))
    }

    /// Orders by content: segment by segment from the innermost, a proper prefix first. Walks both paths through the arena, and stops at a shared suffix.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let (mut a, mut b) = (*self, *other);
        loop {
            if a == b {
                return Ordering::Equal;
            }
            match (a.node(), b.node()) {
                (None, _) => return Ordering::Less,
                (_, None) => return Ordering::Greater,
                (Some(x), Some(y)) => match x.head.get().cmp(y.head.get()) {
                    Ordering::Equal => (a, b) = (Path::from_id(x.tail), Path::from_id(y.tail)),
                    unequal => return unequal,
                },
            }
        }
    }

    /// Creates a path from a sequence of *field names*, every one of which becomes a
    /// [`cir::PathSegment::Field`].
    ///
    /// This is deliberately named rather than a `FromIterator<S: AsRef<str>>` impl: an iterator
    /// of strings is exactly as plausible a spelling for "parse these as path segments",
    /// and silently choosing `Symbol` for all of them is how a model port could never name an
    /// offset. Callers holding *syntax* want [`Path::parse`]; callers holding names want this.
    pub fn from_symbol_names(iter: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self::from_accesses(
            iter.into_iter()
                .map(|name| cir::PathSegment::Field(ArcIntern::from(name.as_ref()))),
        )
    }

    /// Parses a path in the canonical grammar ([`cir::path_syntax`]).
    ///
    /// Not injective, and deliberately so: parsing normalizes through [`Path::from_accesses`],
    /// which sums adjacent offset runs and drops `Offset(0)`. So `".[1].[2]"` and `".[3]"` both
    /// yield `Offset(3)`, and `".x.[0]"` yields just `.x`. That is a *semantic* property of this
    /// type (see the type docs), not a parsing one -- [`cir::parse_segments`] gives the segments
    /// exactly as written if you need them.
    pub fn parse(s: &str) -> Result<Self, cir::PathSyntaxError> {
        cir::parse_segments(s).map(Self::from_accesses)
    }

    /// Returns the canonical string representation, e.g. `.foo.bar`, `.foo.[8]`, `.\[_elem_]`.
    /// The exact inverse of [`Path::parse`], modulo that parse's documented normalization.
    pub fn to_dot_string(&self) -> String {
        cir::path_to_string(self.iter())
    }

    /// Iterates from the innermost access out
    pub fn iter(&self) -> PathIter {
        PathIter(*self)
    }

    /// Concatenates self onto other, returning new path
    pub fn concat(&self, other: &Self) -> Self {
        prepend_onto(self, *other)
    }

    /// Substitutes given prefix of path with new_prefix and returns the new path.
    #[inline(always)]
    pub fn substitute_prefix(&self, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        match_prefix(self, prefix).map(|suffix| prepend_onto(new_prefix, suffix))
    }

    /// Same as substitute_prefix but only returns a new path if the suffix after prefix matching
    /// is non-empty. Also merges offsets like substitute_prefix.
    #[inline]
    pub fn substitute_prefix_with_nonempty_suffix(
        &self,
        prefix: &Path,
        new_prefix: &Path,
    ) -> Option<Path> {
        match_prefix(self, prefix)
            .filter(|s| !s.is_empty())
            .map(|suffix| prepend_onto(new_prefix, suffix))
    }

    /// True iff `self` starts with `port` structurally: `self` is `port` followed
    /// by zero or more additional [`PathSegment`](cir::PathSegment) components,
    /// compared component-wise (innermost-first) with **no** offset arithmetic
    /// (unlike [`match_prefix`], whose last component merges offsets). An empty
    /// `port` matches every path.
    ///
    /// This is the "extension of the port" predicate used to expand wildcard sink
    /// ports into the concrete paths that live beneath them: a port
    /// `Argument(3)` (empty path) matches `Argument(3).deref`, `Argument(3).[12].deref`,
    /// etc.; a port `Argument(3).deref` matches `Argument(3).deref.foo` but not
    /// `Argument(3).[12].deref` (the offset precedes the deref, so it is not a suffix).
    pub fn is_extension_of(&self, port: &Path) -> bool {
        let mut self_it = self.iter();
        for p in port.iter() {
            match self_it.next() {
                Some(c) if c == p => {}
                _ => return false,
            }
        }
        true
    }

    /// Every component-wise split of this path, as `(key, rest)` with `self == key · rest`, so
    /// that `match_prefix(self, key) == Some(rest)`. Together with [`Path::prefix_keys_wild`]
    /// this spells out every prefix [`match_prefix`] accepts, as join keys.
    ///
    /// `rest` is a tail of this path, so it shares its storage; each `key` is interned anew.
    pub fn prefix_keys(&self) -> Vec<(Path, Path)> {
        let comps = self.segment_ids();
        let mut out = Vec::with_capacity(comps.len() + 1);
        // `rest` is `comps[k..]`, a tail of this path.
        let mut rest = *self;
        for k in 0..=comps.len() {
            let key = Path::from_prefix(&comps[..k]);
            out.push((key, rest));
            rest = rest.tail();
        }
        out
    }

    /// The splits of this path at an offset component, as `(key, rest)` where `rest` begins
    /// with that offset, `[n]`: a prefix `key.[m]` matches for **any** `m != n`, yielding the
    /// suffix `[n - m] · tail` where `rest = [n] · tail` — the offset arithmetic
    /// [`match_prefix`] performs on its last component. A rule that joins on the wild key
    /// matches it with [`Path::wild_rest`]; the case `m == n` is the exact key `key.[n]` of
    /// [`Path::prefix_keys`], so a wild match excludes it. A `#value` component splits the same
    /// way, `rest` beginning with `#value`, and matches every `key.[m]`.
    pub fn prefix_keys_wild(&self) -> Vec<(Path, Path)> {
        let comps = self.segment_ids();
        let mut out = Vec::new();
        // `rest` is `comps[k..]`, a tail of this path.
        let mut rest = *self;
        for k in 0..comps.len() {
            if matches!(
                comps[k].get(),
                cir::PathSegment::Offset(_) | cir::PathSegment::Value
            ) {
                let key = Path::from_prefix(&comps[..k]);
                out.push((key, rest));
            }
            rest = rest.tail();
        }
        out
    }

    /// The match of a wild split's `rest` (see [`Path::prefix_keys_wild`]) against a prefix
    /// `key.[m]`: the offset adjustment and the suffix after it. `[n]·tail` matches when
    /// `n != m` (the case `n == m` is an exact key) and gives `(n - m, tail)`; `#value·tail`
    /// matches any `m` and gives `(0, #value·tail)`, since `[m].#value = #value`.
    #[inline]
    pub fn wild_rest(&self, m: i64) -> Option<(i64, Path)> {
        match self.head()? {
            cir::PathSegment::Offset(Offset(n)) if *n != m => Some((n - m, self.tail())),
            cir::PathSegment::Value => Some((0, *self)),
            _ => None,
        }
    }

    /// Splits off a trailing offset component: `key.[m]` gives `(key, m)`. `None` for the empty
    /// path and for a path whose last component is a symbol.
    pub fn split_trailing_offset(&self) -> Option<(Path, i64)> {
        let comps = self.segment_ids();
        match comps.last()?.get() {
            cir::PathSegment::Offset(cir::Offset(m)) => {
                Some((Path::from_prefix(&comps[..comps.len() - 1]), *m))
            }
            _ => None,
        }
    }

    /// True for the path `#value` itself, the suffix that, like the empty one, moves forward only
    /// (S8 of `docs/ir.md`).
    #[inline]
    pub fn is_value(&self) -> bool {
        self.uncons()
            .is_some_and(|(head, tail)| head.is_value() && tail.is_empty())
    }

    /// The first component. `None` for the empty path.
    #[inline]
    pub fn head(&self) -> Option<&'static cir::PathSegment> {
        self.node().map(|n| n.head.get())
    }

    /// The value of the first component if it is an offset.
    #[inline]
    pub fn head_offset(&self) -> Option<i64> {
        match self.head() {
            Some(cir::PathSegment::Offset(cir::Offset(n))) => Some(*n),
            _ => None,
        }
    }

    /// This path without its first component. The empty path for the empty path.
    #[inline]
    pub fn tail(&self) -> Path {
        self.node().map_or(Path::EMPTY, |n| Path::from_id(n.tail))
    }

    /// This path with its first component replaced by `head`, which the caller has normalized
    /// against the tail as [`Path::cons`] requires. Not for the empty path.
    #[inline]
    fn with_head(&self, head: cir::PathSegment) -> Path {
        debug_assert!(!self.is_empty());
        Path::cons(head, self.tail())
    }
}

/// Iterates a [`Path`]'s segments from the innermost out.
#[derive(Clone)]
pub struct PathIter(Path);

impl Iterator for PathIter {
    type Item = &'static cir::PathSegment;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.0.node()?;
        self.0 = Path::from_id(node.tail);
        Some(node.head.get())
    }
}

// serde writes the cons list node by node from the head: `Nil`, or a struct
// `Node { head, tail }`, inside a newtype `Path`.
impl Serialize for Path {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Node(Path);
        impl Serialize for Node {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeStruct;
                match self.0.node() {
                    None => serializer.serialize_unit_variant("Node", 0, "Nil"),
                    Some(node) => {
                        let mut state = serializer.serialize_struct("Node", 2)?;
                        state.serialize_field("head", node.head.get())?;
                        state.serialize_field("tail", &Node(Path::from_id(node.tail)))?;
                        state.end()
                    }
                }
            }
        }
        serializer.serialize_newtype_struct("Path", &Node(*self))
    }
}

impl<'de> Deserialize<'de> for Path {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Node(Path);
        impl<'de> Deserialize<'de> for Node {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                // `Nil` is written as a unit variant, which a self-describing format such as
                // JSON reads back as the string.
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum NodeData {
                    Nil,
                    Name(String),
                    Cons { head: cir::PathSegment, tail: Node },
                }
                Ok(Node(match NodeData::deserialize(deserializer)? {
                    NodeData::Nil => Path::EMPTY,
                    NodeData::Name(name) if name == "Nil" => Path::EMPTY,
                    NodeData::Name(name) => {
                        return Err(serde::de::Error::invalid_value(
                            serde::de::Unexpected::Str(&name),
                            &"Nil or a path node",
                        ));
                    }
                    NodeData::Cons { head, tail } => Path::cons(head, tail.0),
                }))
            }
        }
        #[derive(Deserialize)]
        #[serde(rename = "Path")]
        struct Repr(Node);
        Ok(Repr::deserialize(deserializer)?.0.0)
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Heap {
    pub formal_index: FormalIndex,
    pub path: Path,
}

impl Heap {
    pub fn new(formal_index: FormalIndex) -> Self {
        Self {
            formal_index,
            path: Path::empty(),
        }
    }

    pub fn with_path(formal_index: FormalIndex, path: Path) -> Self {
        Self { formal_index, path }
    }

    pub fn index(&self) -> FormalIndex {
        self.formal_index
    }
}

/// The canonical printer: `Display` and [`FromStr`] are inverses, so a rendered path can be fed
/// straight back in. Note there is no `path(...)` wrapper -- a vertex renders as `%L1.deref`.
impl Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        cir::write_path(&mut out, self.iter());
        f.write_str(&out)
    }
}

impl FromStr for Path {
    type Err = cir::PathSyntaxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Fully qualified ID of a function
#[repr(transparent)]
#[derive(Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Function(pub EltId);

impl Deref for Function {
    type Target = Str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Function {
    /// Orders by name, not by the name's id.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.content_cmp(&other.0)
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Function(name) = self;
        write!(f, "function({name})")
    }
}

impl From<Str> for Function {
    #[inline]
    fn from(s: Str) -> Self {
        Function(s)
    }
}

impl From<ArcIntern<str>> for Function {
    #[inline]
    fn from(s: ArcIntern<str>) -> Self {
        Function(Str::from(s))
    }
}

impl FromStr for Function {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Function(s.into()))
    }
}

/// Index into the parameter list. Negative indices are reserved for the engine
#[derive(
    Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Serialize, Deserialize, Default,
)]
#[repr(transparent)]
pub struct FormalIndex(i16);

/// Start of return values. If there's more than one return value, uses -2, -3, etc
pub const RETURN_INDEX: i16 = -1i16;

impl FormalIndex {
    #[inline]
    pub fn new(i: i16) -> Self {
        Self(i)
    }
}

impl Deref for FormalIndex {
    type Target = i16;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<i8> for FormalIndex {
    #[inline]
    fn from(i: i8) -> Self {
        Self(i.into())
    }
}

impl From<i16> for FormalIndex {
    #[inline]
    fn from(i: i16) -> Self {
        Self(i)
    }
}

impl TryFrom<usize> for FormalIndex {
    type Error = Error;
    #[inline]
    fn try_from(i: usize) -> Result<Self, Self::Error> {
        match i.try_into() {
            Ok(i) => Ok(Self(i)),
            Err(_) => Err(Error::FactsConvert(
                "usize too big for FormalIndex".to_string(),
            )),
        }
    }
}

impl Display for FormalIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<cir::ParameterIdx> for FormalIndex {
    type Error = Error;
    fn try_from(p: cir::ParameterIdx) -> Result<FormalIndex, Self::Error> {
        match p.index().try_into() {
            Ok(i) => Ok(FormalIndex(i)),
            Err(_e) => Err(Error::FactsConvert(
                "ParameterIdx too big for FormalIndex".to_string(),
            )),
        }
    }
}

/// A global variable of the project (G1 of `docs/ir.md`). A global *is* its name (G3): two
/// programs that declare `g` declare one variable, so the id is the interned name and two
/// imports' declarations meet at one id without a table. Serde writes the name.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GlobalId(pub Str);

impl GlobalId {
    pub fn new(name: impl Into<Str>) -> Self {
        GlobalId(name.into())
    }

    pub fn name(&self) -> Str {
        self.0
    }

    /// Orders by name, not by id.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.content_cmp(&other.0)
    }
}

impl Debug for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Global({})", self.0)
    }
}

/// `$name`, the textual form of a global (G1).
impl Display for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}", self.0)
    }
}

/// The root of a reachability or summary row: a formal (the return is
/// `Formal(RETURN_INDEX)`), or a global. Packed in four bytes, since it is a column of
/// `locals`: a formal's `i16`, or the top bit and the global's name id.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
#[repr(transparent)]
pub struct Root(u32);

const ROOT_GLOBAL: u32 = 1 << 31;

/// What a [`Root`] is.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum RootKind {
    Formal(FormalIndex),
    Global(GlobalId),
}

impl Root {
    #[inline]
    pub fn formal(i: FormalIndex) -> Self {
        Root((*i as u16) as u32)
    }

    #[inline]
    pub fn global(g: GlobalId) -> Self {
        let raw = g.0.raw();
        assert!(
            raw & ROOT_GLOBAL == 0,
            "a global's name id does not fit a root"
        );
        Root(raw | ROOT_GLOBAL)
    }

    #[inline]
    pub fn is_global(&self) -> bool {
        self.0 & ROOT_GLOBAL != 0
    }

    #[inline]
    pub fn as_formal(&self) -> Option<FormalIndex> {
        (!self.is_global()).then(|| FormalIndex::new(self.0 as u16 as i16))
    }

    #[inline]
    pub fn as_global(&self) -> Option<GlobalId> {
        self.is_global().then(|| {
            // SAFETY: the bits are `raw()` of a `Str` `Root::global` was handed.
            GlobalId(unsafe { Str::from_raw_unchecked(self.0 & !ROOT_GLOBAL) })
        })
    }

    pub fn kind(&self) -> RootKind {
        match self.as_global() {
            Some(g) => RootKind::Global(g),
            None => RootKind::Formal(self.as_formal().unwrap()),
        }
    }

    /// The bits, for the LSM store.
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Whether a global root's name was handed out, for roots read back from storage that
    /// bypassed the interner.
    pub fn is_valid(&self) -> bool {
        !self.is_global() || Str::from_raw(self.0 & !ROOT_GLOBAL).is_some()
    }

    /// Whether a flow into `path` of this root reaches the caller: [`isout`] for a formal, and
    /// always for a global, which every instance shares (G2).
    #[inline]
    pub fn isout(&self, formal_type: FormalType, path: &Path) -> bool {
        match self.as_formal() {
            Some(i) => isout(&i, formal_type, path),
            None => true,
        }
    }

    /// Orders by what the root denotes, not by its bits.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.kind(), other.kind()) {
            (RootKind::Formal(a), RootKind::Formal(b)) => a.cmp(&b),
            (RootKind::Global(a), RootKind::Global(b)) => a.content_cmp(&b),
            (RootKind::Formal(_), RootKind::Global(_)) => std::cmp::Ordering::Less,
            (RootKind::Global(_), RootKind::Formal(_)) => std::cmp::Ordering::Greater,
        }
    }
}

impl From<FormalIndex> for Root {
    fn from(i: FormalIndex) -> Self {
        Root::formal(i)
    }
}

/// The formal of that index.
impl From<i16> for Root {
    fn from(i: i16) -> Self {
        Root::formal(i.into())
    }
}

impl From<GlobalId> for Root {
    fn from(g: GlobalId) -> Self {
        Root::global(g)
    }
}

impl From<RootKind> for Root {
    fn from(k: RootKind) -> Self {
        match k {
            RootKind::Formal(i) => Root::formal(i),
            RootKind::Global(g) => Root::global(g),
        }
    }
}

impl Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            RootKind::Formal(i) => write!(f, "Formal({i})"),
            RootKind::Global(g) => write!(f, "{g:?}"),
        }
    }
}

/// A formal prints as its index, as [`FormalIndex`] does; a global as `$name`.
impl Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            RootKind::Formal(i) => write!(f, "{i}"),
            RootKind::Global(g) => write!(f, "{g}"),
        }
    }
}

impl Serialize for Root {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.kind().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Root {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(RootKind::deserialize(deserializer)?.into())
    }
}

impl Serialize for RootKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        enum R<'a> {
            Formal(i16),
            Global(&'a str),
        }
        match self {
            RootKind::Formal(i) => R::Formal(**i),
            RootKind::Global(g) => R::Global(g.0.as_str()),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RootKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum R {
            Formal(i16),
            Global(String),
        }
        Ok(match R::deserialize(deserializer)? {
            R::Formal(i) => RootKind::Formal(i.into()),
            R::Global(g) => RootKind::Global(GlobalId::new(g)),
        })
    }
}

/// The *global frame*, the distinguished frame of program-level facts (`docs/debugging.md`): the
/// flows between two global paths, which hold in every instance (G6 of `docs/ir.md`), and the
/// call-target and string labels at global paths. It has no formals and no call sites. Reserved,
/// as [`RETURN_INDEX`] is: the largest id an [`InsnSiteId`] can hold, which no [`IdMap`] hands
/// out, and which every `IdMap` names [`GLOBAL_FRAME_NAME`].
pub const GLOBAL_FRAME: FunctionId = FunctionId { id: (1 << 28) - 1 };

/// The name of [`GLOBAL_FRAME`].
pub const GLOBAL_FRAME_NAME: &str = "<globals>";

static GLOBAL_FRAME_FUNCTION: LazyLock<Function> =
    LazyLock::new(|| Function::from(Str::from(GLOBAL_FRAME_NAME)));

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackedInsnSiteId(pub [u8; 8]);

impl PackedInsnSiteId {
    pub fn try_from_parts(
        func_id: FunctionId,
        insn_id: InsnId,
    ) -> Result<Self, packed_struct::PackingError> {
        let site_id = InsnSiteId::new(func_id, insn_id);
        InsnSiteId::pack(&site_id).map(PackedInsnSiteId)
    }
}

impl Deref for PackedInsnSiteId {
    type Target = [u8; 8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<InsnSiteId> for PackedInsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: InsnSiteId) -> Result<PackedInsnSiteId, Self::Error> {
        InsnSiteId::pack(&site_id).map(PackedInsnSiteId)
    }
}

impl TryFrom<PackedInsnSiteId> for InsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: PackedInsnSiteId) -> Result<InsnSiteId, Self::Error> {
        InsnSiteId::unpack(&site_id)
    }
}

impl TryFrom<&PackedInsnSiteId> for InsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: &PackedInsnSiteId) -> Result<InsnSiteId, Self::Error> {
        InsnSiteId::unpack(site_id)
    }
}

impl Display for PackedInsnSiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(site_id) = InsnSiteId::try_from(self) {
            write!(f, "{}:{}", site_id.func_id.id, site_id.insn_id.id)
        } else {
            write!(f, "packed({:?})", self.0)
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackedCallArg(pub [u8; 8]);

impl PackedCallArg {
    pub fn try_from_parts(
        insn_id: InsnId,
        formal: FormalIndex,
    ) -> Result<Self, packed_struct::PackingError> {
        let call_arg = CallArgId::new(insn_id, formal);
        CallArgId::pack(&call_arg).map(PackedCallArg)
    }
}

impl Deref for PackedCallArg {
    type Target = [u8; 8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<CallArgId> for PackedCallArg {
    type Error = packed_struct::PackingError;
    fn try_from(call_arg: CallArgId) -> Result<PackedCallArg, Self::Error> {
        CallArgId::pack(&call_arg).map(PackedCallArg)
    }
}

impl TryFrom<PackedCallArg> for CallArgId {
    type Error = packed_struct::PackingError;
    fn try_from(packed: PackedCallArg) -> Result<CallArgId, Self::Error> {
        CallArgId::unpack(&packed)
    }
}

impl TryFrom<&PackedCallArg> for CallArgId {
    type Error = packed_struct::PackingError;
    fn try_from(packed: &PackedCallArg) -> Result<CallArgId, Self::Error> {
        CallArgId::unpack(packed)
    }
}

impl Display for PackedCallArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(call_arg) = CallArgId::try_from(self) {
            write!(f, "{}:{}", call_arg.insn_id.id, call_arg.formal)
        } else {
            write!(f, "packed({:?})", self.0)
        }
    }
}

/// An instruction site represents an instruction and the function in which it is contained. We use
/// a packed struct so we only need 64 bits for this information. The function id is stored in 28
/// bits; the instruction id is stored in the remaining 36 bits.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, PackedStruct)]
#[packed_struct(bit_numbering = "msb0", size_bytes = "8")]
pub struct InsnSiteId {
    #[packed_field(bits = "0..=27", endian = "msb")]
    pub func_id: FunctionId,
    #[packed_field(bits = "28..64", endian = "msb")]
    pub insn_id: InsnId,
}

impl InsnSiteId {
    pub fn new(function_id: FunctionId, insn_id: InsnId) -> Self {
        Self {
            func_id: function_id,
            insn_id,
        }
    }
}

/// A call argument represents an instruction and the formal index of the argument. We use a packed
/// struct so we only need 64 bits for this information. The instruction id is stored in 36 bits;
/// the formal index is stored in 16 bits.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, PackedStruct)]
#[packed_struct(bit_numbering = "msb0", size_bytes = "8")]
pub struct CallArgId {
    #[packed_field(bits = "0..36", endian = "msb")]
    pub insn_id: InsnId,
    #[packed_field(bits = "36..52", endian = "msb")]
    pub formal: i16,
}

impl CallArgId {
    pub fn new(insn_id: InsnId, formal: FormalIndex) -> Self {
        Self {
            insn_id,
            formal: *formal,
        }
    }

    pub fn formal(&self) -> FormalIndex {
        FormalIndex::from(self.formal)
    }
}

/// A function ID. The packed bit-size of this has to be kept in sync with [`InsnSiteId`].
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    PackedStruct,
    Serialize,
    Deserialize,
    Default,
)]
#[packed_struct(bit_numbering = "msb0", size_bits = "28")]
#[repr(transparent)]
pub struct FunctionId {
    #[packed_field(bits = "0..28", endian = "msb")]
    pub id: u32,
}

impl FunctionId {
    pub fn new(id: u32) -> Self {
        FunctionId { id }
    }

    pub fn incr_assign(&mut self) {
        self.id += 1;
    }
}

/// An instruction ID. The packed bit-size of this has to be kept in sync with [`InsnSiteId`].
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    PackedStruct,
    Serialize,
    Deserialize,
    Default,
)]
#[packed_struct(bit_numbering = "msb0", size_bits = "36")]
#[repr(transparent)]
pub struct InsnId {
    #[packed_field(bits = "0..36", endian = "msb")]
    pub id: u64,
}

impl InsnId {
    pub fn new(id: u64) -> Self {
        InsnId { id }
    }

    pub fn incr_assign(&mut self) {
        self.id += 1;
    }
}

/// Which artifact import an indexed instruction came from.
///
/// A project indexes several imports into *one* fact base: function and instruction ids are
/// project-global, interned across every import. Source spans are not. A
/// [`source_info::FileSpanId`] is an index into the source-info database of the import the
/// instruction was codegen'd from, so the same number denotes a different line in every other
/// import. This id says which database to read it in; without it a span resolves everywhere and
/// means nothing anywhere. See [`schema::index_source_map`](crate::facts::schema::index_source_map).
///
/// The value is a position in the index-time import list, and
/// [`schema::import_id`](crate::facts::schema::import_id) records the name it stood for.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, Default,
)]
#[repr(transparent)]
pub struct ImportId(pub u32);

/// A variable with metadata that relates it to functions and call sites
///
/// Flow variables are crafted to be 8 bytes. There are five variants, four of them told apart by
/// the top 2 bits. The Local variable is represented with its [`Str`]'s 32-bit id, and the Global
/// shares its tag, with bit 61 set, and is its name's 32-bit id. The Formal is an i16. And the
/// Packed Call Arg similarly only requires 62 bits.
#[derive(Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Default)]
pub struct FlowVariable(u64);

const TAG_UNINIT: u64 = 0 << 62;
const TAG_LOCAL: u64 = 1 << 62;
const TAG_FORMAL: u64 = 2 << 62;
const TAG_CALL_ARG: u64 = 3 << 62;
const TAG_MASK: u64 = 3 << 62;
const DATA_MASK: u64 = !TAG_MASK;
/// Under [`TAG_LOCAL`], marks a global.
const GLOBAL_BIT: u64 = 1 << 61;

#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub enum FlowVariableKind {
    Uninit,
    Local(Str),
    Formal(FormalIndex),
    CallArg(PackedCallArg),
    Global(GlobalId),
}

impl From<FlowVariableKind> for FlowVariable {
    fn from(kind: FlowVariableKind) -> Self {
        match kind {
            FlowVariableKind::Uninit => FlowVariable(TAG_UNINIT),
            FlowVariableKind::Local(s) => FlowVariable(s.raw() as u64 | TAG_LOCAL),
            FlowVariableKind::Formal(f) => {
                let val = (*f as u16) as u64;
                FlowVariable(val | TAG_FORMAL)
            }
            FlowVariableKind::CallArg(c) => {
                let val = u64::from_be_bytes(c.0);
                debug_assert!((val & TAG_MASK) == 0, "CallArg top bits must be 0");
                FlowVariable(val | TAG_CALL_ARG)
            }
            FlowVariableKind::Global(g) => FlowVariable(g.0.raw() as u64 | TAG_LOCAL | GLOBAL_BIT),
        }
    }
}

impl FlowVariable {
    pub fn local(s: Str) -> Self {
        FlowVariableKind::Local(s).into()
    }

    pub fn formal_index(f: FormalIndex) -> Self {
        FlowVariableKind::Formal(f).into()
    }

    pub fn call_arg_packed(c: PackedCallArg) -> Self {
        FlowVariableKind::CallArg(c).into()
    }

    pub fn global(g: GlobalId) -> Self {
        FlowVariableKind::Global(g).into()
    }

    /// The vertex of root `r` at the call `insn`: the actual of a formal, and a global itself,
    /// which no call passes (G5).
    pub fn call_root(insn: InsnId, r: Root) -> Self {
        match r.kind() {
            RootKind::Formal(n) => FlowVariable::call_actual(insn, n),
            RootKind::Global(g) => FlowVariable::global(g),
        }
    }

    /// The root this variable is in its own function: a formal or a global.
    pub fn as_root(&self) -> Option<Root> {
        match self.kind() {
            FlowVariableKind::Formal(i) => Some(Root::formal(i)),
            FlowVariableKind::Global(g) => Some(Root::global(g)),
            _ => None,
        }
    }

    pub fn as_global(&self) -> Option<GlobalId> {
        self.is_global().then(|| GlobalId(self.local_str()))
    }

    pub fn is_global(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_LOCAL && (self.0 & GLOBAL_BIT) != 0
    }

    /// The vertex of the actual for parameter `formal` of the call at `insn`.
    pub fn call_actual(insn: InsnId, formal: FormalIndex) -> Self {
        FlowVariable::call_arg_packed(PackedCallArg::try_from_parts(insn, formal).unwrap())
    }

    pub fn as_local(&self) -> Option<Str> {
        self.is_local().then(|| self.local_str())
    }

    /// The `Str` of a `Local` or a `Global`, whose payload is the id `local` or `global` put
    /// there.
    fn local_str(&self) -> Str {
        Str::from_raw(self.0 as u32).expect("a Local holds a Str's id")
    }

    pub fn as_formal(&self) -> Option<FormalIndex> {
        self.formal()
    }

    pub fn as_call_arg(&self) -> Option<PackedCallArg> {
        if self.is_call_arg() {
            let val = self.0 & DATA_MASK;
            Some(PackedCallArg(val.to_be_bytes()))
        } else {
            None
        }
    }

    pub fn kind(&self) -> FlowVariableKind {
        match self.0 & TAG_MASK {
            TAG_UNINIT => FlowVariableKind::Uninit,
            TAG_LOCAL if self.is_global() => FlowVariableKind::Global(GlobalId(self.local_str())),
            TAG_LOCAL => FlowVariableKind::Local(self.local_str()),
            TAG_FORMAL => {
                let val = (self.0 & DATA_MASK) as i16;
                FlowVariableKind::Formal(FormalIndex::from(val))
            }
            TAG_CALL_ARG => {
                let val = self.0 & DATA_MASK;
                FlowVariableKind::CallArg(PackedCallArg(val.to_be_bytes()))
            }
            _ => unreachable!(),
        }
    }

    pub fn is_uninit(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_UNINIT
    }

    pub fn is_local(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_LOCAL && (self.0 & GLOBAL_BIT) == 0
    }

    pub fn is_formal(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_FORMAL
    }

    pub fn is_call_arg(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_CALL_ARG
    }

    /// Whether a `Local`'s or a `Global`'s [`Str`] was handed out, for variables read back from
    /// storage that bypassed the interner. True for every other variant.
    pub fn is_valid(&self) -> bool {
        (self.0 & TAG_MASK) != TAG_LOCAL || Str::from_raw(self.0 as u32).is_some()
    }
}

impl Debug for FlowVariable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind())
    }
}

impl Serialize for FlowVariable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.kind().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlowVariable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let kind = FlowVariableKind::deserialize(deserializer)?;
        Ok(FlowVariable::from(kind))
    }
}

impl FlowVariable {
    pub fn formal(&self) -> Option<FormalIndex> {
        if self.is_formal() {
            let val = (self.0 & DATA_MASK) as i16;
            Some(FormalIndex::from(val))
        } else {
            None
        }
    }

    /// Orders two flow variables by what they *denote*, not by their packed bits. This is usable as
    /// a sort comparator over a whole index table.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        use FlowVariableKind::*;
        match (self.kind(), other.kind()) {
            (Uninit, Uninit) => std::cmp::Ordering::Equal,
            (Local(a), Local(b)) => a.as_str().cmp(b.as_str()),
            (Formal(a), Formal(b)) => a.cmp(&b),
            (CallArg(a), CallArg(b)) => a.0.cmp(&b.0),
            (Global(a), Global(b)) => a.content_cmp(&b),
            // Different variants: order by the tag, which is what the derived `Ord` does too (a
            // global after a local, as its bit 61 makes it).
            _ => {
                let rank = |v: &FlowVariable| (v.0 & TAG_MASK, v.is_global());
                rank(self).cmp(&rank(other))
            }
        }
    }
}

impl Display for FlowVariable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            FlowVariableKind::Uninit => write!(f, "uninit"),
            FlowVariableKind::Local(name) => write!(f, "local({name})"),
            FlowVariableKind::Formal(index) => write!(f, "formal({index})"),
            FlowVariableKind::Global(g) => write!(f, "{g}"),
            FlowVariableKind::CallArg(packed) => {
                let CallArgId { insn_id, formal } = packed.try_into().unwrap();
                write!(f, "call-arg({}, {formal})", insn_id.id)
            }
        }
    }
}

impl TryFrom<cir::ParameterIdx> for FlowVariable {
    type Error = TryFromVariableError;
    #[inline]
    fn try_from(idx: cir::ParameterIdx) -> Result<FlowVariable, Self::Error> {
        match idx.try_into() {
            Ok(i) => Ok(FlowVariableKind::Formal(i).into()),
            Err(_) => Err(TryFromVariableError::Param),
        }
    }
}

#[derive(Error, Debug)]
pub enum TryFromVariableError {
    #[error("a global is named by its program's table, which codegen holds")]
    Global,
    #[error("parameter doesn't fit")]
    Param,
}

impl TryFrom<&cir::Variable> for FlowVariable {
    type Error = TryFromVariableError;
    #[inline]
    fn try_from(v: &cir::Variable) -> Result<FlowVariable, Self::Error> {
        match v {
            cir::Variable::Local(_) => {
                let name = format!("{v}");
                Ok(FlowVariableKind::Local(Str::from(name)).into())
            }
            cir::Variable::Param(idx) => (*idx).try_into(),
            cir::Variable::Global(_) => Err(TryFromVariableError::Global),
        }
    }
}

impl TryFrom<&cir::VariableRef> for FlowVariable {
    type Error = TryFromVariableError;
    /// If the variable has no version, tries to convert from a variable. Otherwise, *formats* the
    /// variable and returns a local. Every version var, in other words, becomes a local.
    #[inline]
    fn try_from(v: &cir::VariableRef) -> Result<FlowVariable, Self::Error> {
        let cir::VariableRef { variable, version } = v;
        match version {
            None => variable.try_into(),
            Some(version) => {
                let name = format!("{variable}_{version}");
                Ok(FlowVariableKind::Local(Str::from(name)).into())
            }
        }
    }
}

/// An access path: a variable and its path. The fact-level form of [`cir::AccessPath`].
///
/// The third field is padding: it makes the 4-byte [`Path`] fill the vertex's 16 bytes, so the
/// LSM provider, which stores and compares a value's raw bytes, never reads an uninitialized one.
/// Build a vertex with [`FlowVertex::new`]; patterns skip the pad with `..`.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Default, Serialize, Deserialize)]
pub struct FlowVertex(pub FlowVariable, pub Path, #[serde(skip)] pub VertexPad);

/// [`FlowVertex`]'s padding: four bytes that are always zero.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct VertexPad(u32);

impl FlowVertex {
    #[inline]
    pub const fn new(var: FlowVariable, path: Path) -> Self {
        FlowVertex(var, path, VertexPad(0))
    }

    /// Whether the handles in this vertex were handed out, for vertices read back from storage
    /// that bypassed the interners.
    pub fn is_valid(&self) -> bool {
        self.0.is_valid() && self.1.is_valid() && self.2 == VertexPad(0)
    }
}

impl std::hash::Hash for FlowVertex {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
        self.1.hash(state);
    }
}

impl Debug for FlowVertex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FlowVertex")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl FlowVertex {
    /// Orders by what the variable and path denote, not by their ids.
    pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .content_cmp(&other.0)
            .then_with(|| self.1.content_cmp(&other.1))
    }
}

impl Display for FlowVertex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let FlowVertex(var, path, ..) = self;
        write!(f, "{var}{path}")
    }
}

/// What an `id_handle!` over a content enum shares: building it from its content, reading the
/// content back, a content order, and `Display` and serde through the content.
macro_rules! interned_enum_impls {
    ($handle:ident, $kind:ident) => {
        impl From<$kind> for $handle {
            fn from(kind: $kind) -> Self {
                $handle::intern(&kind)
            }
        }

        impl $handle {
            /// The value this handle stands for, without a lock.
            pub fn kind(self) -> &'static $kind {
                self.get()
            }

            /// Orders by content, not by id, so a sort that feeds output is reproducible.
            pub fn content_cmp(&self, other: &Self) -> std::cmp::Ordering {
                if self == other {
                    return std::cmp::Ordering::Equal;
                }
                self.get().cmp(other.get())
            }
        }

        impl Display for $handle {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                Display::fmt(self.get(), f)
            }
        }

        impl Serialize for $handle {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.get().serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $handle {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                $kind::deserialize(deserializer).map(Into::into)
            }
        }
    };
}

/// What an indirect or virtual call site (a `callee_info` row) dispatches on, in its frontend's
/// own terms.
///
/// - `Java(simple_name, descriptor)` — a JVM / Dex virtual call.
/// - `C` — a C-style function-pointer call.
/// - `Lua(method)` — a Lua `recv:m(...)` call. There is no descriptor: a Lua method is uniquely
///   named within its class table.
/// - `Cil(simple_name, descriptor)` — a .NET `callvirt`, in its own key space so a Xamarin app's
///   Dex and CIL halves cannot collide.
///
/// [`CallDispatchKey`] is its interned handle.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize)]
pub enum CallDispatchKeyKind {
    /// JVM / Dex virtual call: (method simple name, method descriptor).
    Java(Symbol, Symbol),
    /// C-style function-pointer call.
    C,
    /// Lua metatable call: the method's simple name. No descriptor.
    Lua(Symbol),
    /// .NET virtual call: (method simple name, method descriptor).
    Cil(Symbol, Symbol),
}

impl Display for CallDispatchKeyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallDispatchKeyKind::Java(name, desc) => write!(f, "java_call<{name}{desc}>"),
            CallDispatchKeyKind::C => write!(f, "c_call"),
            CallDispatchKeyKind::Lua(name) => write!(f, "lua_call<{name}>"),
            CallDispatchKeyKind::Cil(name, desc) => write!(f, "cil_call<{name}{desc}>"),
        }
    }
}

immortal::id_handle! {
    /// An interned [`CallDispatchKeyKind`]: a 4-byte handle into a process-wide interner that
    /// never frees. Ordered by id, so sort with `content_cmp` wherever the order is printed.
    pub struct CallDispatchKey(CallDispatchKeyKind);
}

interned_enum_impls!(CallDispatchKey, CallDispatchKeyKind);

static C_DISPATCH: LazyLock<CallDispatchKey> = LazyLock::new(|| CallDispatchKeyKind::C.into());

impl CallDispatchKey {
    /// `C`'s handle. The first call interns it; later calls take no lock.
    pub fn c() -> CallDispatchKey {
        *C_DISPATCH
    }
}

impl TaintDirection {
    pub fn reversed(&self) -> Self {
        match self {
            TaintDirection::Forward => TaintDirection::Backward,
            TaintDirection::Backward => TaintDirection::Forward,
        }
    }
}

impl Display for TaintDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaintDirection::Forward => write!(f, "forward"),
            TaintDirection::Backward => write!(f, "backward"),
        }
    }
}

/// Denotes the direction of program execution. Forward is the normal execution direction; backward
/// is the reverse.
#[derive(
    Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize, Copy,
)]
pub enum TaintDirection {
    #[default]
    Forward,
    Backward,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
pub enum IntentKind {
    #[default]
    Activity,
    Receiver,
    StartedService,
    BoundService,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
pub enum IntentPairKind {
    #[default]
    Explicit,
    Implicit,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum FormalType {
    Copied,
    #[default]
    SharedFields,
    Shared,
}

impl From<cir::ParameterType> for FormalType {
    #[inline]
    fn from(ty: cir::ParameterType) -> Self {
        use cir::ParameterType::*;
        match ty {
            Copied => FormalType::Copied,
            Shared => FormalType::Shared,
            SharedFields => FormalType::SharedFields,
        }
    }
}

/// Returns true if the formal flows input
#[inline(always)]
pub fn isin(formal: i64) -> bool {
    formal == -3 || formal >= 0
}

/// Returns true if the formal flows output: whether a flow into `path` of the formal reaches the
/// caller's actual. For `SharedFields` that is S8 of `docs/ir.md`: the empty suffix and the
/// suffix `#value` move forward only, so neither a flow into the whole parameter nor one into its
/// value reaches the caller.
#[inline(always)]
pub fn isout(formal_index: &FormalIndex, formal_type: FormalType, path: &Path) -> bool {
    let i: i16 = **formal_index;
    if i >= 0 {
        match formal_type {
            FormalType::Copied => false,
            FormalType::Shared => true,
            FormalType::SharedFields => !path.is_empty() && !path.is_value(),
        }
    } else {
        true
    }
}

/// Prepends the components of `new_prefix` (head-first, as `Path::iter` yields them) onto the
/// already-interned `suffix`, reusing `suffix`'s shared backing structure. Semantically
/// equivalent to `Path::from_accesses(new_prefix.iter().chain(suffix.iter()).cloned())` but
/// without rebuilding and re-interning `suffix`.
///
/// Every `Path` in the system is built through [`Path::from_accesses`], which collapses runs of
/// adjacent [`cir::PathSegment::Offset`]s, so neither an interned `new_prefix` nor a `suffix` returned
/// by [`match_prefix`] (a tail of an interned path, possibly with an offset-adjusted head) ever
/// contains internal adjacent offsets. The *only* place `from_accesses` could merge is therefore
/// the `new_prefix`/`suffix` junction — which this handles explicitly — so the result is
/// identical to that rebuild while doing O(|new_prefix|) interns instead of
/// O(|new_prefix| + |suffix|) (and zero when `new_prefix` is empty, the common copy/load case).
#[inline]
fn prepend_onto(new_prefix: &Path, suffix: Path) -> Path {
    use cir::{Offset, PathSegment};
    // `new_prefix` is a short program path prefix (usually 0–2 components).
    let comps = new_prefix.segment_ids();
    if comps.is_empty() {
        return suffix; // result is exactly the shared suffix — no interning at all
    }
    // Junction rewriting, the equations of `ctadl_ir::cir::path` applied where the two meet.
    // Both sides are normal, so only the last component of `new_prefix` and the head of `suffix`
    // can combine, and at most once:
    // - two offsets sum, and a zero sum vanishes;
    // - `[k].#value` is `#value`, so an offset before a `#value` head is dropped;
    // - `#value.[k]` and `#value.#value` are `#value`, so after a `#value` the head is dropped.
    let (mut acc, upto) = match (comps.last().map(|s| s.get()), suffix.head()) {
        (Some(PathSegment::Offset(Offset(a))), Some(PathSegment::Offset(Offset(b)))) => {
            let sum = a + b;
            let rest = if sum == 0 {
                suffix.tail()
            } else {
                suffix.with_head(PathSegment::Offset(Offset(sum)))
            };
            (rest, comps.len() - 1)
        }
        (Some(PathSegment::Offset(_)), Some(PathSegment::Value)) => (suffix, comps.len() - 1),
        (Some(PathSegment::Value), Some(PathSegment::Offset(_) | PathSegment::Value)) => {
            (suffix.tail(), comps.len())
        }
        _ => (suffix, comps.len()),
    };
    // Prepend the remaining `new_prefix` components so iteration yields them head-first.
    for &comp in comps[..upto].iter().rev() {
        acc = Path::cons_id(comp, acc);
    }
    acc
}

/// Returns the suffix solving the equation path = prefix + suffix, if there is one. The suffix may
/// be empty. Otherwise returns none.
///
/// This supports offset arithmetic. For example, if `path = .x.[2]` and `prefix = .x.[1]`,
/// the suffix is `.[1]`.
#[inline]
pub fn match_prefix(path: &Path, prefix: &Path) -> Option<Path> {
    match_prefix_split(path, prefix).map(|(adjust, rest)| match adjust {
        Some(adj) => Path::cons(cir::PathSegment::Offset(Offset(adj)), rest),
        None => rest,
    })
}

/// [`match_prefix`]'s suffix without building it: `(adjust, rest)` for the suffix
/// `[adjust] · rest`, where `rest` is a tail of `path` and `[adjust]` is a nonzero offset segment,
/// present only when `adjust` is `Some`. Interns nothing.
#[inline]
pub fn match_prefix_split(path: &Path, prefix: &Path) -> Option<(Option<i64>, Path)> {
    use cir::PathSegment;
    let mut path_seq = *path;
    let mut prefix_seq = *prefix;

    if prefix_seq.is_empty() {
        return Some((None, path_seq));
    }

    // Check that all components except the last one match exactly
    // Invariant: prefix_seq.tail() is a non-empty seq
    while !prefix_seq.tail().is_empty() {
        if prefix_seq.head() != path_seq.head() {
            return None;
        }
        prefix_seq = prefix_seq.tail();
        path_seq = path_seq.tail();
    }
    // unwrap() safety follows from while condition
    let prefix_last = prefix_seq.head().unwrap();

    // There's one prefix component left. If there are no more path components, we fail to match.
    let path_last = path_seq.head()?;

    match (path_last, prefix_last) {
        // The last offsets match with a nonzero offset adjustment. An exact offset match
        // (an == pn) is handled by the branch below, which consumes the component: emitting
        // a leading `Offset(0)` here instead would make the substitution non-injective
        // (`.x` and `.x.[0]` are distinct paths that concatenate identically), letting a
        // backward walk pick a preimage whose forward image was a different path.
        (PathSegment::Offset(Offset(an)), PathSegment::Offset(Offset(pn))) if an != pn => {
            Some((Some(an - pn), path_seq.tail()))
        }
        // `[m].#value = #value`: an offset prefix is absorbed by a `#value` in the path, and
        // the suffix keeps the `#value`.
        (PathSegment::Value, PathSegment::Offset(_)) => Some((None, path_seq)),
        (a, p) if a == p => {
            // Exact match for the last prefix component
            Some((None, path_seq.tail()))
        }
        _ => None,
    }
}

/// Keeps track of the mapping of intern'd function names to index ID's, which are generated at
/// index time. This is a helper when doing codegen.
#[derive(Default, Debug, Clone, Builder)]
pub struct IdMap {
    function_id: BTreeMap<Function, FunctionId>,
    functions: Vec<Function>,
}

impl IdMap {
    pub fn new() -> Self {
        Self {
            function_id: Default::default(),
            functions: Default::default(),
        }
    }

    /// Clears the function ID map and resets counters
    pub fn clear(&mut self) {
        self.function_id.clear();
        self.functions.clear();
    }

    pub fn get_id_to_name_map(&self) -> BTreeMap<u32, String> {
        self.functions
            .iter()
            .enumerate()
            .map(|(i, f)| (i as u32, f.0.to_string()))
            .collect()
    }

    pub fn try_save<P: AsRef<std::path::Path>>(self, path: P) -> Result<(), Error> {
        schema::function_id::try_save(
            path,
            self.functions
                .into_iter()
                .enumerate()
                .map(|(i, v)| (FunctionId::new(i as u32), v)),
        )
        .err_context(|| "saving IdMap")?;
        Ok(())
    }

    pub fn try_load<P: AsRef<std::path::Path>>(path: P) -> Result<Self, Error> {
        let function_id = schema::function_id::try_load(path).err_context(|| "loading IdMap")?;
        let functions: Vec<_> = function_id.iter().map(|(_, v)| v.clone()).collect();
        Ok(Self {
            function_id: function_id.into_iter().map(|(v, k)| (k, v)).collect(),
            functions,
        })
    }

    pub fn get_function_id(&self, f: Function) -> Option<FunctionId> {
        if f.as_str() == GLOBAL_FRAME_NAME {
            return Some(GLOBAL_FRAME);
        }
        self.function_id.get(&f).copied()
    }

    /// The function `func_id` names; [`GLOBAL_FRAME`] is named [`GLOBAL_FRAME_NAME`].
    pub fn get_function(&self, func_id: FunctionId) -> Option<&Function> {
        if func_id == GLOBAL_FRAME {
            return Some(&GLOBAL_FRAME_FUNCTION);
        }
        self.functions.get(func_id.id as usize)
    }

    pub fn functions(&self) -> impl Iterator<Item = (FunctionId, &Function)> {
        self.functions
            .iter()
            .enumerate()
            .map(|(i, f)| (FunctionId::new(i as u32), f))
    }

    /// Adds a function or returns the id previously assigned for the function.
    pub fn get_or_add_function(&mut self, f: Function) -> FunctionId {
        if let Some(id) = self.get_function_id(f.clone()) {
            return id;
        }
        let i = FunctionId::new(self.functions.len().try_into().unwrap());
        assert!(
            i != GLOBAL_FRAME,
            "too many functions: the next id is the global frame's"
        );
        self.function_id.insert(f.clone(), i);
        self.functions.push(f);
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root is a formal or a global, and reads back as what it was, in memory and through
    /// the `summary` parquet schema.
    #[test]
    fn root_round_trips() {
        let g = GlobalId::new("Lcom/C;");
        let roots = [
            Root::formal(0i16.into()),
            Root::formal(RETURN_INDEX.into()),
            Root::formal((-3i16).into()),
            Root::global(g),
        ];
        assert_eq!(roots[1].as_formal(), Some(RETURN_INDEX.into()));
        assert_eq!(roots[3].as_global(), Some(g));
        assert!(roots[3].is_global() && !roots[0].is_global());
        assert!(roots[3].isout(FormalType::Copied, &Path::empty()));

        let dir = tempfile::tempdir().unwrap();
        let f = FunctionId::new(3);
        let p = Path::parse(".f").unwrap();
        let rows: Vec<_> = roots
            .iter()
            .flat_map(|d| roots.iter().map(move |s| (f, *d, p, *s, Path::empty())))
            .collect();
        schema::summary::try_save(dir.path(), rows.clone()).unwrap();
        assert_eq!(schema::summary::try_load(dir.path()).unwrap(), rows);
    }

    /// A global is a flow variable of its own kind, distinct from a local of its name, and
    /// survives the parquet encoding of a vertex.
    #[test]
    fn global_flow_variable_round_trips() {
        let g = FlowVariable::global(GlobalId::new("x"));
        let l = FlowVariable::local(Str::from("x"));
        assert_ne!(g, l);
        assert!(g.is_global() && !g.is_local() && g.is_valid());
        assert!(l.is_local() && !l.is_global());
        assert_eq!(g.as_global(), Some(GlobalId::new("x")));
        assert_eq!(g.as_root(), Some(Root::global(GlobalId::new("x"))));
        assert_eq!(l.content_cmp(&g), std::cmp::Ordering::Less);
        assert_eq!(format!("{g}"), "$x");

        let dir = tempfile::tempdir().unwrap();
        let rows = vec![(FunctionId::new(1), g, Path::empty(), l, Path::empty())];
        schema::assign::try_save(dir.path(), rows.clone()).unwrap();
        assert_eq!(schema::assign::try_load(dir.path()).unwrap(), rows);
    }

    /// The global frame is a function every `IdMap` names, and none hands out.
    #[test]
    fn global_frame_is_named_in_every_id_map() {
        let mut ids = IdMap::new();
        assert_eq!(
            ids.get_function(GLOBAL_FRAME).map(|f| f.as_str()),
            Some(GLOBAL_FRAME_NAME)
        );
        assert_eq!(
            ids.get_function_id(Function::from(Str::from(GLOBAL_FRAME_NAME))),
            Some(GLOBAL_FRAME)
        );
        let f = ids.get_or_add_function(Function::from(Str::from("f")));
        assert_ne!(f, GLOBAL_FRAME);
        assert!(ids.functions().all(|(id, _)| id != GLOBAL_FRAME));
    }

    /// `Path`'s normalization, `prepend_onto` and `match_prefix` agree with `cir::path` on random
    /// segment lists: offset sums, `[0]`, and `#value` absorbing offsets on both sides.
    #[test]
    fn agrees_with_cir_path() {
        use cir::{PathSegment as S, path as mp};
        let mut state = 0x9e37_79b9u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 33
        };
        let seg = |n: u64| match n % 7 {
            0 => S::field("a"),
            1 => S::field("b"),
            2 => S::Deref,
            3 => S::Value,
            k => S::offset((k as i64 - 5) * 4),
        };
        for _ in 0..20_000 {
            let a: Vec<S> = (0..next() % 5).map(|_| seg(next())).collect();
            let b: Vec<S> = (0..next() % 5).map(|_| seg(next())).collect();
            let (pa, pb) = (
                Path::from_accesses(a.clone()),
                Path::from_accesses(b.clone()),
            );
            let na = mp::normalize(a.clone());
            let nb = mp::normalize(b.clone());
            assert_eq!(pa.iter().cloned().collect::<Vec<_>>(), na, "{a:?}");
            let joined = prepend_onto(&pa, pb);
            assert_eq!(
                joined.iter().cloned().collect::<Vec<_>>(),
                mp::concat(&na, &nb),
                "{a:?} · {b:?}"
            );
            assert_eq!(joined, pa.concat(&pb));
            // `match_prefix` is a restriction of `strip_prefix`: when it answers, they agree.
            if let Some(t) = match_prefix(&pb, &pa) {
                assert_eq!(
                    Some(t.iter().cloned().collect::<Vec<_>>()),
                    mp::strip_prefix(&nb, &na),
                    "{nb:?} / {na:?}"
                );
            }
        }
    }

    /// Terse `Path` literal for tests. Named `parse_path` rather than `p` because these tests
    /// bind `p`, `q`, `r` as paths and a `let p` would shadow the helper.
    fn parse_path(s: &str) -> Path {
        Path::parse(s).unwrap_or_else(|e| panic!("test path {s:?} does not parse: {e}"))
    }

    #[test]
    fn str_is_a_four_byte_handle_serialized_by_content() {
        assert_eq!(std::mem::size_of::<Str>(), 4);
        assert_eq!(std::mem::size_of::<Option<Str>>(), 4);

        let s = Str::from("str serde round trip");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"str serde round trip\"");
        assert_eq!(serde_json::from_str::<Str>(&json).unwrap(), s);
        let f = Function(s);
        assert_eq!(serde_json::to_string(&f).unwrap(), json);

        let local = FlowVariable::local(s);
        assert_eq!(local.as_local(), Some(s));
        let back: FlowVariable =
            serde_json::from_str(&serde_json::to_string(&local).unwrap()).unwrap();
        assert_eq!(back, local);
    }

    #[test]
    fn call_handles_are_four_bytes_and_shown_by_content() {
        assert_eq!(std::mem::size_of::<Option<CallDispatchKey>>(), 4);

        // Interned in reverse content order, so ids and content disagree.
        let b: CallDispatchKey = CallDispatchKeyKind::Lua("call_handles_b".into()).into();
        let a: CallDispatchKey = CallDispatchKeyKind::Lua("call_handles_a".into()).into();
        assert_eq!(a.content_cmp(&b), std::cmp::Ordering::Less);
        assert_eq!(b.content_cmp(&a), std::cmp::Ordering::Greater);
        assert_eq!(a.to_string(), "lua_call<call_handles_a>");

        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(
            json,
            serde_json::to_string(&CallDispatchKeyKind::Lua("call_handles_a".into())).unwrap()
        );
        assert_eq!(serde_json::from_str::<CallDispatchKey>(&json).unwrap(), a);

        let c = CallDispatchKey::c();
        assert_eq!(c, CallDispatchKeyKind::C.into());
        assert_eq!(c.to_string(), "c_call");
    }

    #[test]
    fn path_handles_are_four_bytes() {
        assert_eq!(std::mem::size_of::<Path>(), 4);
        assert_eq!(std::mem::size_of::<Option<Path>>(), 4);
        assert_eq!(std::mem::size_of::<(Path, FormalIndex, Path)>(), 12);
        assert_eq!(std::mem::size_of::<FlowVertex>(), 16);
        assert_eq!(
            std::mem::size_of::<FlowVertex>(),
            std::mem::size_of::<FlowVariable>()
                + std::mem::size_of::<Path>()
                + std::mem::size_of::<VertexPad>()
        );
        // The empty path's bits are never a `PathId`'s.
        assert_eq!(PathId::from_raw(Path::EMPTY.0.get()), None);
        assert!(Path::EMPTY.is_valid());
        assert_eq!(Path::default(), Path::EMPTY);
    }

    #[test]
    fn path_suffixes_are_shared_and_hashes_fold() {
        let p = Path::parse(".a.b.[4].c").unwrap();
        let q = Path::parse(".x.b.[4].c").unwrap();
        assert_eq!(p.tail(), q.tail());
        assert_eq!(p.tail(), Path::parse(".b.[4].c").unwrap());
        assert_eq!(p.tail().tail().tail().tail(), Path::EMPTY);
        assert_eq!(p.len(), 4);
        let node = p.node().unwrap();
        assert_eq!(node.hash, suffix_hash(node.head, p.tail().suffix_hash()));
        assert_eq!(Path::EMPTY.suffix_hash(), EMPTY_PATH_HASH);
    }

    /// A file's node index, built from raw nodes, probes as the path interner does, which
    /// hashes the nodes themselves.
    #[test]
    fn path_base_index_hashes_as_the_interner() {
        let paths = [".a.b.[4].c", ".x", ".x.#value"].map(|s| Path::parse(s).unwrap());
        let nodes: Vec<PathNode> = paths.iter().map(|p| p.node().unwrap().clone()).collect();
        let raw: Vec<RawNode> = nodes
            .iter()
            .map(|n| [n.head.raw(), n.tail.map_or(0, PathId::raw), n.hash])
            .collect();
        assert_eq!(path_base_index(&raw), immortal::base_index(&nodes, 1));
    }

    /// `concat` prepends onto the shared suffix, and agrees with rebuilding the whole path; the
    /// prefix splits agree with rebuilding each key.
    #[test]
    fn concat_and_splits_agree_with_rebuilding() {
        use cir::PathSegment;
        let parts = [
            "",
            ".x",
            ".[2]",
            ".[-2]",
            ".x.[2]",
            ".[2].x",
            ".x.y",
            ".#value",
            ".x.#value",
            ".deref",
            ".[2].deref",
            ".x.[2].deref.#value",
            ".[3].#value",
        ];
        for a in parts {
            let pa = Path::parse(a).unwrap();
            for b in parts {
                let pb = Path::parse(b).unwrap();
                let want = Path::from_accesses(pa.iter().chain(pb.iter()).cloned());
                assert_eq!(pa.concat(&pb), want, "{a} · {b}");
            }
            let comps: Vec<PathSegment> = pa.iter().cloned().collect();
            for (k, (key, rest)) in pa.prefix_keys().into_iter().enumerate() {
                assert_eq!(key, Path::from_accesses(comps[..k].iter().cloned()));
                assert_eq!(key.concat(&rest), pa);
            }
            for (key, rest) in pa.prefix_keys_wild() {
                assert_eq!(key.concat(&rest), pa, "{a}");
            }
            if let Some((key, m)) = pa.split_trailing_offset() {
                assert_eq!(
                    key.concat(&Path::from_accesses([PathSegment::Offset(Offset(m))])),
                    pa
                );
            }
        }
    }

    #[test]
    fn path_content_cmp_ignores_intern_order() {
        // Interned longest-last and in reverse content order, so ids and content disagree.
        let names = ["path_cmp/z", "path_cmp/y", "path_cmp/x"];
        let paths: Vec<Path> = names
            .iter()
            .map(|n| Path::from_symbol_names([n]))
            .chain([
                Path::from_symbol_names(["path_cmp/x", "path_cmp/y"]),
                Path::from_accesses([
                    cir::PathSegment::field("path_cmp/x"),
                    cir::PathSegment::offset(3),
                ]),
                Path::EMPTY,
            ])
            .collect();
        let mut by_content = paths.clone();
        by_content.sort_by(Path::content_cmp);
        let mut by_segments = paths.clone();
        by_segments.sort_by(|a, b| a.iter().cmp(b.iter()));
        assert_eq!(by_content, by_segments);
        assert_eq!(by_content[0], Path::EMPTY);
        assert_eq!(
            by_content.iter().map(Path::to_string).collect::<Vec<_>>(),
            [
                "",
                ".path_cmp/x",
                ".path_cmp/x.path_cmp/y",
                ".path_cmp/x.[3]",
                ".path_cmp/y",
                ".path_cmp/z",
            ]
        );
    }

    #[test]
    fn path_serde_writes_the_cons_list() {
        let path = Path::parse(".foo.[8]").unwrap();
        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(
            json,
            r#"{"head":{"Field":"foo"},"tail":{"head":{"Offset":8},"tail":"Nil"}}"#
        );
        assert_eq!(serde_json::from_str::<Path>(&json).unwrap(), path);
        assert_eq!(serde_json::to_string(&Path::EMPTY).unwrap(), r#""Nil""#);
        assert_eq!(
            serde_json::from_str::<Path>(r#""Nil""#).unwrap(),
            Path::EMPTY
        );
        assert!(serde_json::from_str::<Path>(r#""Cons""#).is_err());

        let vertex = FlowVertex::new(FlowVariable::formal_index(FormalIndex::new(1)), path);
        let json = serde_json::to_string(&vertex).unwrap();
        assert_eq!(
            json,
            format!(
                "[{{\"Formal\":1}},{}]",
                serde_json::to_string(&path).unwrap()
            )
        );
        assert_eq!(serde_json::from_str::<FlowVertex>(&json).unwrap(), vertex);
    }

    #[test]
    fn symbol_segments_match_only_themselves() {
        // A symbol segment is an opaque name here: two symbols match when they are equal and
        // never otherwise. Nothing in this module reads a symbol's *spelling*, so a frontend that
        // wants two accesses to may-alias (a non-constant subscript and the element it might be,
        // say) must spell them the same path -- that choice belongs to the frontend, not here.
        // Offsets, which the IR does give arithmetic meaning, are covered by the tests below.
        use ctadl_ir::cir::PathSegment;
        let sym = |s: &str| Path::from_accesses([PathSegment::field(s)]);
        let elem = sym("[_elem_]"); // the lua frontend's non-constant key
        let c0 = sym("[0]");
        let c1 = sym("[1]");
        let field = sym("a"); // a struct member

        assert!(match_prefix(&elem, &elem).is_some());
        assert!(match_prefix(&c0, &c0).is_some());
        // Every distinct pair, bracketed or not, stays disjoint.
        for (path, prefix) in [
            (&c0, &elem),
            (&elem, &c0),
            (&c0, &c1),
            (&c1, &c0),
            (&field, &elem),
            (&elem, &field),
        ] {
            assert!(
                match_prefix(path, prefix).is_none(),
                "{} must not match {}",
                path.to_dot_string(),
                prefix.to_dot_string()
            );
        }
    }

    #[test]
    fn test_substitute_prefix() {
        let p: Path = Path::empty();
        assert_eq!(p, p.substitute_prefix(&p, &p).unwrap());

        let p = Path::from_symbol_names(["a", "c"]);
        let q = Path::from_symbol_names(["a"]);
        let r = Path::from_symbol_names(["b"]);
        let e = Path::from_symbol_names(["b", "c"]);

        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p = Path::from_symbol_names(["a", "b"]);
        let q = Path::from_symbol_names(["c", "d"]);
        assert!(p.substitute_prefix(&q, &Path::empty()).is_none());

        // Test case: p23.substitute_prefix(p2, p1) where p23=.[1], p2='', p1=.[1] -> .[2]
        // This tests offset merging when matching empty prefix
        use ctadl_ir::cir::{Offset, PathSegment};

        // Create p23 = .[1]
        let p23 = Path::from_accesses([PathSegment::Offset(Offset(1))]);

        // Create p2 = '' (empty path)
        let p2 = Path::empty();

        // Create p1 = .[1]
        let p1 = Path::from_accesses([PathSegment::Offset(Offset(1))]);

        let result = p23.substitute_prefix(&p2, &p1).unwrap();

        // Create expected = .[2]
        let expected = Path::from_accesses([PathSegment::Offset(Offset(2))]);

        assert_eq!(result, expected);

        // More offset arithmetic tests
        let p = parse_path(".x.[2]");
        let q = parse_path(".x.[1]");
        let r = parse_path(".y");
        let e = parse_path(".y.[1]");
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p = parse_path(".x.[1].f");
        let q = parse_path(".x");
        let r = parse_path(".y");
        let e = parse_path(".y.[1].f");
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());
    }

    /// An exact offset match must consume the component, exactly like an exact symbol match —
    /// not leave a residual `.[0]` suffix. Otherwise the forward transfer function along an
    /// offset junction maps `.x.[2]` (under prefix `.x.[2]`, new prefix `.y`) to `.y.[0]` while
    /// a backward walk from `.y` also reconstructs `.x.[2]`: two distinct successor states for
    /// one predecessor, so backward walks could pick a preimage whose forward image was a
    /// different path and slip past `paths()` feasibility gates the real forward chain failed.
    #[test]
    fn test_match_prefix_exact_offset_consumes_component() {
        use ctadl_ir::cir::{Offset, PathSegment};

        // Whole-path exact offset match: suffix is empty, not `[Offset(0)]`.
        let path = parse_path(".x.[2]");
        let prefix = parse_path(".x.[2]");
        let suffix = match_prefix(&path, &prefix).unwrap();
        assert!(suffix.is_empty());

        // Via substitute_prefix: result is `.y`, not `.y.[0]`.
        let new_prefix = parse_path(".y");
        assert_eq!(
            path.substitute_prefix(&prefix, &new_prefix),
            Some(parse_path(".y"))
        );

        // Exact offset match mid-path: the offset is consumed, suffix starts at `.f`.
        let path = parse_path(".x.[2].f");
        let suffix = match_prefix(&path, &prefix).unwrap();
        assert_eq!(
            suffix.iter().cloned().collect::<Vec<_>>(),
            vec![PathSegment::Field(ArcIntern::from("f"))]
        );
        assert_eq!(
            path.substitute_prefix(&prefix, &new_prefix),
            Some(parse_path(".y.f"))
        );

        // Round trip: forward then backward substitution is the identity, including the
        // exact-offset case.
        for (path_s, pre_s, np_s) in [
            (".x.[2]", ".x.[2]", ".y"),
            (".x.[2].f", ".x.[2]", ".y"),
            (".x.[2]", ".x.[2]", ".y.[3]"),
            (".x.[5]", ".x.[2]", ".y"),
        ] {
            let path = parse_path(path_s);
            let pre = parse_path(pre_s);
            let np = parse_path(np_s);
            let fwd = path.substitute_prefix(&pre, &np).unwrap();
            assert_eq!(
                fwd.substitute_prefix(&np, &pre),
                Some(path),
                "substitute_prefix({path_s}, {pre_s}, {np_s}) did not round-trip"
            );
        }

        // A nonzero adjustment merges offsets.
        let path = parse_path(".x.[5]");
        let suffix = match_prefix(&path, &prefix).unwrap();
        assert_eq!(
            suffix.iter().cloned().collect::<Vec<_>>(),
            vec![PathSegment::Offset(Offset(3))]
        );
    }

    /// Locks the sharing-preserving `substitute_prefix` (via `prepend_onto`) to the exact output
    /// of flattening `new_prefix ++ suffix` and rebuilding with `from_accesses`. The subtle case is the `new_prefix`/`suffix`
    /// junction offset-merge; the common case is an empty `new_prefix` (result is the shared
    /// suffix, zero interning). A drift here would change analysis results.
    #[test]
    fn test_substitute_prefix_matches_rebuild() {
        // Reference: flatten new_prefix ++ suffix and rebuild.
        fn rebuild(path: &Path, prefix: &Path, new_prefix: &Path) -> Option<Path> {
            match_prefix(path, prefix)
                .map(|suffix| Path::from_accesses(new_prefix.iter().chain(suffix.iter()).cloned()))
        }
        // A spread of prefixes/new-prefixes exercising: empty new_prefix (zero-intern path),
        // junction offset+offset merge, offset-next-to-symbol (no merge), empty suffix, and
        // non-matching prefix (None).
        let paths = [
            ".x.[5].g",
            ".x.[1].[2]",
            ".a.b.c",
            ".[3]",
            ".x",
            ".x.[4]",
            ".p.q.r.s",
        ];
        let prefixes = ["", ".x", ".x.[1]", ".a", ".[3]", ".x.[4]", ".p.q", ".nope"];
        let new_prefixes = ["", ".y", ".y.[3]", ".[7]", ".m.n", ".[0]"];
        for path_s in paths {
            let path = parse_path(path_s);
            for pre_s in prefixes {
                let pre = parse_path(pre_s);
                for np_s in new_prefixes {
                    let np = parse_path(np_s);
                    assert_eq!(
                        path.substitute_prefix(&pre, &np),
                        rebuild(&path, &pre, &np),
                        "substitute_prefix({path_s}, {pre_s}, {np_s}) diverged from rebuild"
                    );
                }
            }
        }
    }

    #[test]
    fn test_is_extension_of() {
        let empty = Path::empty();
        let deref = parse_path(".deref");
        let off_deref = parse_path(".[-40].deref");
        let foo = parse_path(".foo");

        // An empty port (bare `Argument(n)`) matches every path.
        assert!(empty.is_extension_of(&empty));
        assert!(deref.is_extension_of(&empty));
        assert!(off_deref.is_extension_of(&empty));

        // Exact match is an extension (zero-length suffix).
        assert!(deref.is_extension_of(&deref));

        // A strict extension: the port followed by one more component.
        let deref_foo = deref.concat(&foo); // .deref.foo
        assert!(deref_foo.is_extension_of(&deref));

        // The motivating case: the args-array element `.[-40].deref` is an
        // extension of the bare argument port but NOT of `.deref` -- the offset
        // precedes the deref, so it is not a suffix of `.deref`. This is why the
        // execve sink port must be the bare `Argument(1)`.
        assert!(!off_deref.is_extension_of(&deref));

        // A different leading component is not an extension.
        assert!(!deref.is_extension_of(&foo));

        // No offset arithmetic (unlike match_prefix): `.[8]` is not an extension
        // of `.[4]`.
        let off4 = parse_path(".[4]");
        let off8 = parse_path(".[8]");
        assert!(!off8.is_extension_of(&off4));
    }

    #[test]
    fn test_substitute_prefix_comprehensive() {
        // 1. Several offsets
        let p = parse_path(".x.[30]");
        let q = parse_path(".x.[10]");
        let r = parse_path(".y.[2]");
        let expected = parse_path(".y.[22]");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p = parse_path(".a.[100].b.[50]");
        let q = parse_path(".a.[60]");
        let r = parse_path(".c.[10]");
        let expected = parse_path(".c.[50].b.[50]"); // .c.[10] + ([100]-[60]) = .c.[50]
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 2. Empty prefix
        let p = parse_path(".x.[10]");
        let q: Path = Path::empty();
        let r = parse_path(".y.[5]");
        let expected = parse_path(".y.[5].x.[10]");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p = parse_path(".[10]");
        let q: Path = Path::empty();
        let r = parse_path(".[5]");
        let expected = parse_path(".[15]");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 3. Empty new_prefix
        let p = parse_path(".x.y");
        let q = parse_path(".x");
        let r: Path = Path::empty();
        let expected = parse_path(".y");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p = parse_path(".x.[10]");
        let q = parse_path(".x");
        let r: Path = Path::empty();
        let expected = parse_path(".[10]");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 4. Empty path
        let p: Path = Path::empty();
        let q: Path = Path::empty();
        let r = parse_path(".x.[10]");
        let expected = parse_path(".x.[10]");
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p: Path = Path::empty();
        let q = parse_path(".x");
        let r = parse_path(".y");
        assert_eq!(p.substitute_prefix(&q, &r), None);

        // Additional cases: Negative offsets
        let p = parse_path(".[10]");
        let q = parse_path(".[20]");
        let r = parse_path(".[5]");
        let expected = parse_path(".[-5]"); // [5] + ([10] - [20]) = [5] - [10] = [-5]
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));
    }

    #[test]
    fn test_path_serialization() {
        let path = Path::from_symbol_names(["foo", "bar.baz"]);
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.bar\\.baz");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_dots() {
        let path = Path::from_symbol_names(["foo.bar", "baz.qux"]);
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo\\.bar.baz\\.qux");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_offsets() {
        // Test path with numeric offsets
        // Create a path manually with mixed PathSegment types
        use ctadl_ir::cir::{Offset, PathSegment};
        let path = Path::from_accesses([
            PathSegment::Field(ArcIntern::from("foo")),
            PathSegment::Offset(Offset(42)),
            PathSegment::Field(ArcIntern::from("bar")),
        ]);

        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.[42].bar");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    /// `parse(to_dot_string(p)) == p` over paths whose symbol names look like offsets or contain
    /// the grammar's metacharacters: a bracketed symbol (`Symbol("[]")`, `Symbol("[_elem_]")`,
    /// `Symbol("[3]")`) must come back as that symbol, not as the empty path or an offset.
    #[test]
    fn test_path_round_trip_corpus() {
        use ctadl_ir::cir::{Offset, PathSegment};
        let sym = |s: &str| PathSegment::field(s);
        let off = |n: i64| PathSegment::Offset(Offset(n));

        for segs in [
            vec![],
            // Bracketed symbols the frontends emit: dex/jvm, lua/C, and lua/C again.
            vec![sym("[]")],
            vec![sym("[_elem_]")],
            vec![sym("[3]")],
            // ... and in context, which is how they actually occur.
            vec![sym("arr"), sym("[]")],
            vec![sym("t"), sym("[1]"), sym("[2]")],
            // Escapes.
            vec![sym("a.b")],
            vec![sym(r"a\b")],
            vec![sym("a"), sym("b")],
            // Offsets, including the sign and the model-port shape from `Argument(1).[8].deref`.
            vec![off(-1)],
            vec![off(42)],
            vec![off(8), sym("deref")],
            vec![sym("foo"), off(42), sym("bar")],
        ] {
            let path = Path::from_accesses(segs.clone());
            let printed = path.to_dot_string();
            let parsed = Path::parse(&printed)
                .unwrap_or_else(|e| panic!("{segs:?} printed as {printed:?}, which failed: {e}"));
            assert_eq!(path, parsed, "round trip through {printed:?}");
        }
    }

    /// The two ways `Path::parse` is deliberately not injective. Both are `from_accesses`
    /// normalizing -- a *semantic* property of `Path` (see its type docs), not a parse bug -- so
    /// they are pinned here rather than left to look like round-trip failures.
    #[test]
    fn test_path_parse_normalizes() {
        use ctadl_ir::cir::{Offset, PathSegment};

        // Adjacent offsets are summed: a path holds at most one offset in a row.
        assert_eq!(
            Path::parse(".[1].[2]").unwrap(),
            Path::parse(".[3]").unwrap()
        );
        assert_eq!(
            Path::parse(".[1].[2]").unwrap(),
            Path::from_accesses([PathSegment::Offset(Offset(3))])
        );

        // A zero offset is the identity on addresses and is dropped.
        assert_eq!(Path::parse(".x.[0]").unwrap(), Path::parse(".x").unwrap());
        assert_eq!(Path::parse(".[0]").unwrap(), Path::empty());

        // `Symbol("[3]")` (a lua/C array field) and `Offset(3)` stay distinct: the symbol's
        // leading `[` is escaped.
        assert_ne!(
            Path::from_accesses([PathSegment::field("[3]")]),
            Path::from_accesses([PathSegment::Offset(Offset(3))])
        );
        assert_eq!(
            Path::from_accesses([PathSegment::field("[3]")]).to_dot_string(),
            r".\[3]"
        );
        assert_eq!(
            Path::from_accesses([PathSegment::Offset(Offset(3))]).to_dot_string(),
            ".[3]"
        );
    }

    /// `Display` and `FromStr` are inverses: no `path(...)` wrapper.
    #[test]
    fn test_path_display_is_parseable() {
        let path = Path::parse(r".foo.[8].\[_elem_]").unwrap();
        assert_eq!(path.to_string(), r".foo.[8].\[_elem_]");
        assert_eq!(Path::parse(&path.to_string()).unwrap(), path);
        assert_eq!(Path::empty().to_string(), "");
    }

    #[test]
    fn test_path_parse_rejects_malformed() {
        use ctadl_ir::cir::PathSyntaxErrorKind;
        assert_eq!(
            Path::parse("foo").unwrap_err().kind,
            PathSyntaxErrorKind::MissingLeadingDot
        );
        assert_eq!(
            Path::parse(".a..b").unwrap_err().kind,
            PathSyntaxErrorKind::EmptySegment
        );
        assert_eq!(
            Path::parse(".[_elem_]").unwrap_err().kind,
            PathSyntaxErrorKind::InvalidOffset("_elem_".into())
        );
    }
}

#[cfg(test)]
mod prefix_key_tests {
    use super::*;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    /// Every `(path, prefix)` pair agrees with `match_prefix`: the suffix it returns is exactly
    /// what the keys of `path` reconstruct for `prefix`, and nothing else matches.
    #[test]
    fn prefix_keys_agree_with_match_prefix() {
        let paths: Vec<Path> = [
            "",
            ".x",
            ".y",
            ".x.y",
            ".x.[4]",
            ".x.[1]",
            ".x.[4].y",
            ".x.[1].y",
            ".[4]",
            ".[1]",
            ".[4].deref",
            ".[7].deref.[2]",
            ".x.y.z",
            ".deref",
            ".deref.[8]",
            ".deref.[8].x",
            ".#value",
            ".x.#value",
            ".[4].x.#value",
        ]
        .iter()
        .map(|s| p(s))
        .collect();
        for path in &paths {
            let keys = path.prefix_keys();
            let wild_keys = path.prefix_keys_wild();
            for prefix in &paths {
                let expected = match_prefix(path, prefix);
                // Reconstruct from the keys: an exact key equal to `prefix`, or a wild key whose
                // `prefix` is `key.[m]`.
                let mut found: Vec<Path> = Vec::new();
                for (key, rest) in &keys {
                    if key == prefix {
                        found.push(*rest);
                    }
                }
                for (key, rest) in &wild_keys {
                    if let Some((pk, m)) = prefix.split_trailing_offset()
                        && pk == *key
                        && let Some((adj, tail)) = rest.wild_rest(m)
                    {
                        found.push(Path::from_accesses(
                            std::iter::once(cir::PathSegment::Offset(cir::Offset(adj)))
                                .chain(tail.iter().cloned()),
                        ));
                    }
                }
                found.sort();
                found.dedup();
                assert_eq!(
                    found,
                    expected.into_iter().collect::<Vec<_>>(),
                    "path={} prefix={}",
                    path.to_dot_string(),
                    prefix.to_dot_string()
                );
            }
        }
    }
}
