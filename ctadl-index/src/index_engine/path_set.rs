//! The admissible paths, frozen for the fixpoint: a per-index, read-only table that
//! answers "is `prefix · [adjust] · rest` a path?" without building `prefix · [adjust] · rest`,
//! and hands out each path's splits without interning them.
//!
//! The local propagation rules of the index engine extend a path across an edge and
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
//! The set is per index, not global: the global interner also holds prefix keys,
//! rests and other indexes' paths, so having a handle is not being admissible.

use std::fs::File;
use std::hash::Hasher;
use std::io::{BufWriter, Write};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ctadl_ir::cir::{Offset, PathSegment};
use hashbrown::HashMap;
use internment::ArcIntern;
use rustc_hash::FxBuildHasher;

use crate::error::{Error, ErrorContext};
use crate::facts::{
    EMPTY_PATH_HASH, Path, RawNode, SegmentId, install_path_base, path_base_index, suffix_hash,
    suffix_hash_raw,
};

/// The splits of one admissible path: see [`PathSet::splits`].
#[derive(Clone, Copy, Default)]
pub struct Splits<'a> {
    /// Every component-wise split `(key, rest)`, from `([], p)` to `(p, [])`.
    pub exact: &'a [(Path, Path)],
    /// The splits before an offset component, `(key, [n]·tail)`.
    pub wild: &'a [(Path, Path)],
}

/// The candidate `body[..n] · [junction] · base`: the first `n` segments of `body`, then the
/// offset segment `junction` when it is `Some`, then the existing path `base`. Normalized: no two
/// offsets are adjacent and no offset is zero.
struct Candidate {
    body: Path,
    n: usize,
    junction: Option<SegmentId>,
    base: Path,
}

impl Candidate {
    /// The suffix hash the candidate's node would store, folded from `base`'s.
    #[inline]
    fn hash(&self) -> u32 {
        let mut h = self.base.suffix_hash();
        if let Some(s) = self.junction {
            h = suffix_hash(s, h);
        }
        fold(self.body, self.n, h)
    }

    /// Whether `p` is the candidate: its first segments are the new part, and the tail after
    /// them is `base`, compared by handle.
    #[inline]
    fn is(&self, mut p: Path) -> bool {
        let mut body = self.body;
        for _ in 0..self.n {
            let (Some((a, p_tail)), Some((b, body_tail))) = (p.uncons_id(), body.uncons_id())
            else {
                return false;
            };
            if a != b {
                return false;
            }
            (p, body) = (p_tail, body_tail);
        }
        if let Some(s) = self.junction {
            match p.uncons_id() {
                Some((head, tail)) if head == s => p = tail,
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
    let (head, tail) = p.uncons_id().expect("fold past the end of a path");
    suffix_hash(head, fold(tail, n - 1, h))
}

/// A slice a [`PathSet`] owns, or borrows from a file mapped for the rest of the process.
struct Buf<T: 'static> {
    slice: NonNull<[T]>,
    owned: bool,
}

// SAFETY: a `Buf` is a `Box<[T]>` or a `&'static [T]`, each of which is `Send` and `Sync` when
// `T` is both.
unsafe impl<T: Send + Sync> Send for Buf<T> {}
// SAFETY: as for `Send`.
unsafe impl<T: Send + Sync> Sync for Buf<T> {}

impl<T> Buf<T> {
    fn owned(v: Vec<T>) -> Self {
        Buf {
            slice: NonNull::from(Box::leak(v.into_boxed_slice())),
            owned: true,
        }
    }

    fn mapped(s: &'static [T]) -> Self {
        Buf {
            slice: NonNull::from(s),
            owned: false,
        }
    }

    /// Heap bytes: the slice's when it owns it, else none.
    fn heap_bytes(&self) -> usize {
        if self.owned { size_of_val(&**self) } else { 0 }
    }
}

impl<T> Deref for Buf<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        // SAFETY: the slice is a leaked box this `Buf` frees only on drop, or `'static`.
        unsafe { self.slice.as_ref() }
    }
}

impl<T> Drop for Buf<T> {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: `owned` slices came from `Box::leak` in `Buf::owned`, and only this drop
            // frees them.
            drop(unsafe { Box::from_raw(self.slice.as_ptr()) });
        }
    }
}

/// The open-addressed member table: `members` as `bits | suffix hash << 32`, each at the first
/// free entry from its hash's low bits, in an array at most half full. 0 is a free entry.
fn member_table(members: impl ExactSizeIterator<Item = (u32, u32)>) -> Vec<u64> {
    let mut table = vec![0u64; (members.len() * 2).next_power_of_two().max(2)];
    let mask = table.len() - 1;
    for (raw, h) in members {
        let mut pos = h as usize & mask;
        while table[pos] != 0 {
            pos = (pos + 1) & mask;
        }
        table[pos] = raw as u64 | (h as u64) << 32;
    }
    table
}

/// The admissible paths. See the module docs.
///
/// Members are numbered by rank, their order by handle. Each member's precomputed splits sit in
/// arrays indexed by rank, and a handle's rank is one word of the membership bitset plus a
/// popcount.
///
/// The arrays are built by [`PathSet::from_paths`], or mapped from a file that
/// [`PathSet::write_file`] wrote in another process, by [`PathSet::map_file`]. Lookups are the
/// same for both.
pub struct PathSet {
    /// Every non-empty member, by stored suffix hash: see [`member_table`].
    table: Buf<u64>,
    /// Bit `b` is set when the path whose [`Path::raw`] are `b` is a member.
    bits: Buf<u64>,
    /// The number of members in the words before each word of `bits`.
    ranks: Buf<u32>,
    /// Each member's trailing-offset split `(key, m)`; `m == 0` when it has none, since no path
    /// holds a zero offset.
    trailing_key: Buf<Path>,
    trailing_m: Buf<i64>,
    /// Member `r`'s exact splits are `exact[exact_at[r]..exact_at[r + 1]]`.
    exact_at: Buf<u32>,
    exact: Buf<(Path, Path)>,
    /// As `exact_at` and `exact`, for the wild splits.
    wild_at: Buf<u32>,
    wild: Buf<(Path, Path)>,
    /// The most segments any member has. A candidate with a longer new part is not a member.
    max_depth: usize,
    /// The segment of each offset a member holds, for the junction of a candidate. An offset
    /// with no segment here is in no member.
    offsets: HashMap<i64, SegmentId, FxBuildHasher>,
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
        let words = members.last().map_or(0, |p| p.raw() as usize / 64 + 1);
        let mut bits = vec![0u64; words];
        let mut trailing_key = Vec::with_capacity(members.len());
        let mut trailing_m = Vec::with_capacity(members.len());
        let mut exact_at = Vec::with_capacity(members.len() + 1);
        let mut exact = Vec::new();
        let mut wild_at = Vec::with_capacity(members.len() + 1);
        let mut wild = Vec::new();
        let mut max_depth = 0;
        let mut offsets = HashMap::default();
        for &p in &members {
            bits[p.raw() as usize / 64] |= 1 << (p.raw() % 64);
            exact_at.push(exact.len() as u32);
            exact.extend(p.prefix_keys());
            wild_at.push(wild.len() as u32);
            wild.extend(p.prefix_keys_wild());
            let (key, m) = p.split_trailing_offset().unwrap_or((Path::EMPTY, 0));
            trailing_key.push(key);
            trailing_m.push(m);
            let mut depth = 0;
            let mut q = p;
            while let Some((head, tail)) = q.uncons_id() {
                if let PathSegment::Offset(Offset(o)) = head.get() {
                    offsets.entry(*o).or_insert(head);
                }
                depth += 1;
                q = tail;
            }
            max_depth = usize::max(max_depth, depth);
        }
        exact_at.push(exact.len() as u32);
        wild_at.push(wild.len() as u32);
        let table = member_table(
            members
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| (p.raw(), p.suffix_hash()))
                .collect::<Vec<_>>()
                .into_iter(),
        );
        let ranks = ranks_of(&bits);
        PathSet {
            table: Buf::owned(table),
            bits: Buf::owned(bits),
            ranks: Buf::owned(ranks),
            trailing_key: Buf::owned(trailing_key),
            trailing_m: Buf::owned(trailing_m),
            exact_at: Buf::owned(exact_at),
            exact: Buf::owned(exact),
            wild_at: Buf::owned(wild_at),
            wild: Buf::owned(wild),
            max_depth,
            offsets,
            split_misses: AtomicUsize::new(0),
        }
    }

    /// The members, in rank order.
    fn members(&self) -> impl Iterator<Item = Path> + '_ {
        self.bits.iter().enumerate().flat_map(|(w, &word)| {
            (0..64).filter(move |b| word & (1 << b) != 0).map(move |b| {
                // SAFETY: a set bit is a member's bits.
                unsafe { Path::from_raw_unchecked(w as u32 * 64 + b) }
            })
        })
    }

    /// `p`'s rank, if it is a member.
    #[inline]
    fn rank(&self, p: Path) -> Option<usize> {
        let b = p.raw() as usize;
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
                let m = self.trailing_m[r];
                (m != 0).then_some((self.trailing_key[r], m))
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
        self.trailing_m.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trailing_m.is_empty()
    }

    #[inline]
    pub fn contains(&self, p: &Path) -> bool {
        self.rank(*p).is_some()
    }

    /// Heap bytes of the frozen table, its bitset and its splits. The paths themselves are in the
    /// global interner, and a mapped set's arrays are in its file.
    pub fn bytes(&self) -> usize {
        self.table.heap_bytes()
            + self.bits.heap_bytes()
            + self.ranks.heap_bytes()
            + self.trailing_key.heap_bytes()
            + self.trailing_m.heap_bytes()
            + self.exact_at.heap_bytes()
            + self.exact.heap_bytes()
            + self.wild_at.heap_bytes()
            + self.wild.heap_bytes()
            + self.offsets.allocation_size()
    }

    /// Whether the arrays are mapped from a file.
    pub fn is_mapped(&self) -> bool {
        !self.bits.owned
    }

    /// The member equal to `prefix · [adjust] · rest`, normalized as [`Path::from_accesses`]
    /// normalizes, if there is one. `[adjust]` is an offset segment, present only when `adjust`
    /// is `Some`. A miss is an ordinary answer.
    ///
    /// Folds the new segments onto `rest`'s stored hash and probes once, so it costs time in the
    /// length of `prefix`, not of the result, and neither interns nor allocates nor locks.
    #[inline]
    pub fn concat(&self, prefix: &Path, adjust: Option<i64>, rest: &Path) -> Option<Path> {
        // Offsets merge only at the junction: `prefix` and `rest` are each normalized. The
        // equations are those of `ctadl_ir::cir::path`.
        let mut len = 0;
        let mut last_off = None;
        let mut last_value = false;
        let mut p = *prefix;
        while let Some((head, tail)) = p.uncons_id() {
            len += 1;
            if tail.is_empty() {
                // Only the last segment is read, not just its handle.
                let head = head.get();
                last_off = match head {
                    PathSegment::Offset(Offset(o)) => Some(*o),
                    _ => None,
                };
                last_value = head.is_value();
            }
            p = tail;
        }
        let rest_off = rest.head_offset();
        let rest_value = rest.head().is_some_and(PathSegment::is_value);
        // The candidate, with its junction offset still a number.
        let (body, n, junction, base) = if last_value {
            // `#value.[k] = #value` and `#value.#value = #value`: the adjustment and a leading
            // offset or `#value` of `rest` are absorbed. `rest` is normal, so it has at most one
            // of them.
            (
                *prefix,
                len,
                None,
                if rest_off.is_some() || rest_value {
                    rest.tail()
                } else {
                    *rest
                },
            )
        } else if rest_value {
            // `[k].#value = #value`: the prefix's trailing offset and the adjustment are absorbed.
            (*prefix, len - last_off.is_some() as usize, None, *rest)
        } else if last_off.is_some() || adjust.is_some() {
            let sum = last_off.unwrap_or(0) + adjust.unwrap_or(0) + rest_off.unwrap_or(0);
            (
                *prefix,
                len - last_off.is_some() as usize,
                (sum != 0).then_some(sum),
                if rest_off.is_some() {
                    rest.tail()
                } else {
                    *rest
                },
            )
        } else {
            (*prefix, len, None, *rest)
        };
        if n == 0 && junction.is_none() {
            return self.contains(&base).then_some(base);
        }
        if n + junction.is_some() as usize > self.max_depth {
            return None;
        }
        let candidate = Candidate {
            body,
            n,
            // An offset no member holds is in no candidate that is a member.
            junction: match junction {
                Some(o) => Some(*self.offsets.get(&o)?),
                None => None,
            },
            base,
        };
        let h = candidate.hash();
        let mask = self.table.len() - 1;
        let mut pos = h as usize & mask;
        loop {
            // The table is at most half full, so a free entry ends every probe.
            let e = self.table[pos];
            if e == 0 {
                return None;
            }
            if (e >> 32) as u32 == h {
                // SAFETY: the table holds members' bits, which `from_paths` took from paths and
                // `map_file` checked are paths of the base it installed.
                let p = unsafe { Path::from_raw_unchecked(e as u32) };
                if candidate.is(p) {
                    return Some(p);
                }
            }
            pos = (pos + 1) & mask;
        }
    }

    /// The member equal to `p` with its prefix `prefix` replaced by `new_prefix`, as
    /// [`Path::substitute_prefix`] would build it, if there is one. A miss, whether `prefix` is
    /// not a prefix of `p` or the result is not a member, is an ordinary answer.
    #[inline]
    pub fn substitute_prefix(&self, p: &Path, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        let (adjust, rest) = crate::facts::match_prefix_split(p, prefix)?;
        self.concat(new_prefix, adjust, &rest)
    }

    /// Writes this set to `file` so that another process can map it ([`PathSet::map_file`]).
    ///
    /// The file numbers the segments and path nodes it holds, which are every member, every
    /// split key and rest, every trailing-offset key and all their tails. A node's tail comes
    /// before it, node 1 is a placeholder for [`Path::EMPTY`]'s bits, and each node's suffix
    /// hash is recomputed in the file's segment numbering. The arrays are this set's, in that
    /// numbering, with the members in the rank order it gives them.
    ///
    /// The file is native-endian and holds hashes of this build's, so it is for processes of
    /// the same executable on the same machine.
    pub fn write_file(&self, file: &std::path::Path) -> Result<(), Error> {
        let mut num = Numbering::default();
        let members: Vec<Path> = self.members().collect();
        let mut order: Vec<(u32, usize)> = members
            .iter()
            .enumerate()
            .map(|(rank, &p)| (num.number(p), rank))
            .collect();
        order.sort_unstable();

        let words = order.last().map_or(0, |&(raw, _)| raw as usize / 64 + 1);
        let mut bits = vec![0u64; words];
        let mut trailing_key = Vec::with_capacity(order.len());
        let mut trailing_m = Vec::with_capacity(order.len());
        let mut exact_at = Vec::with_capacity(order.len() + 1);
        let mut exact: Vec<[u32; 2]> = Vec::with_capacity(self.exact.len());
        let mut wild_at = Vec::with_capacity(order.len() + 1);
        let mut wild: Vec<[u32; 2]> = Vec::with_capacity(self.wild.len());
        for &(raw, rank) in &order {
            bits[raw as usize / 64] |= 1 << (raw % 64);
            exact_at.push(exact.len() as u32);
            let splits = self.splits(&members[rank]);
            for &(key, rest) in splits.exact {
                exact.push([num.number(key), num.number(rest)]);
            }
            wild_at.push(wild.len() as u32);
            for &(key, rest) in splits.wild {
                wild.push([num.number(key), num.number(rest)]);
            }
            trailing_key.push(num.number(self.trailing_key[rank]));
            trailing_m.push(self.trailing_m[rank]);
        }
        exact_at.push(exact.len() as u32);
        wild_at.push(wild.len() as u32);
        let ranks = ranks_of(&bits);
        let mut nodes = num.nodes;
        let table = member_table(
            order
                .iter()
                .filter(|&&(raw, _)| raw != Path::EMPTY.raw())
                .map(|&(raw, _)| (raw, nodes[raw as usize - 1][2]))
                .collect::<Vec<_>>()
                .into_iter(),
        );
        if nodes.len() == 1 {
            // No node but the placeholder, which needs a segment.
            nodes.clear();
        } else {
            nodes[0] = [1, 0, 0];
        }
        let node_index = if nodes.is_empty() {
            Vec::new()
        } else {
            path_base_index(&nodes)
        };
        let segments = encode_segments(&num.segments);

        let mut sections = [(0u64, 0u64); SECTIONS];
        let mut at = HEADER_BYTES as u64;
        let bodies: [(&[u8], usize); SECTIONS] = [
            (&segments, segments.len()),
            (plain_bytes(&nodes), nodes.len()),
            (plain_bytes(&node_index), node_index.len()),
            (plain_bytes(&table), table.len()),
            (plain_bytes(&bits), bits.len()),
            (plain_bytes(&ranks), ranks.len()),
            (plain_bytes(&trailing_key), trailing_key.len()),
            (plain_bytes(&trailing_m), trailing_m.len()),
            (plain_bytes(&exact_at), exact_at.len()),
            (plain_bytes(&exact), exact.len()),
            (plain_bytes(&wild_at), wild_at.len()),
            (plain_bytes(&wild), wild.len()),
        ];
        for (section, (body, len)) in sections.iter_mut().zip(&bodies) {
            *section = (at, *len as u64);
            at = (at + body.len() as u64).next_multiple_of(8);
        }
        let mut header: Vec<u64> = vec![
            MAGIC,
            VERSION,
            self.max_depth as u64,
            num.segments.len() as u64,
        ];
        header.extend(sections.iter().flat_map(|&(off, len)| [off, len]));
        debug_assert_eq!(header.len() * 8, HEADER_BYTES);

        let mut out = BufWriter::new(
            File::create(file)
                .map_err(Error::from)
                .err_context(|| format!("creating {}", file.display()))?,
        );
        (|| {
            out.write_all(plain_bytes(&header))?;
            let mut written = HEADER_BYTES as u64;
            for (&(off, _), (body, _)) in sections.iter().zip(&bodies) {
                out.write_all(&vec![0u8; (off - written) as usize])?;
                out.write_all(body)?;
                written = off + body.len() as u64;
            }
            Ok::<_, std::io::Error>(())
        })()
        .and_then(|()| out.flush())
        .and_then(|()| out.get_ref().sync_all())
        .map_err(Error::from)
        .err_context(|| format!("writing {}", file.display()))
    }

    /// Maps `file`, which [`PathSet::write_file`] wrote, and installs its segments and nodes as
    /// this process's path interners' base, so the file's paths are this process's paths. The
    /// map is kept for the rest of the process.
    ///
    /// Fails if the file does not read as a path set, or if this process has interned a path or
    /// a segment already. Checks that every path the arrays hold is one of the file's.
    ///
    /// The file must not change while the process runs.
    pub fn map_file(file: &std::path::Path) -> Result<PathSet, Error> {
        let bad = |msg: String| Error::Staged(format!("path set {}: {msg}", file.display()));
        let f = File::open(file)
            .map_err(Error::from)
            .err_context(|| format!("opening {}", file.display()))?;
        // SAFETY: the stage directory's files are written once, by rename, and this process's
        // contract is that its file does not change while it runs.
        let map = unsafe { memmap2::Mmap::map(&f) }
            .map_err(Error::from)
            .err_context(|| format!("mapping {}", file.display()))?;
        let map: &'static memmap2::Mmap = Box::leak(Box::new(map));
        let FileArrays {
            max_depth,
            segments,
            nodes,
            node_index,
            table,
            bits,
            ranks,
            trailing_key,
            trailing_m,
            exact_at,
            exact,
            wild_at,
            wild,
        } = FileArrays::parse(map).map_err(bad)?;
        let offset_segments: Vec<(i64, u32)> = segments
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                PathSegment::Offset(Offset(o)) => Some((*o, i as u32 + 1)),
                _ => None,
            })
            .collect();
        install_path_base(segments, nodes, node_index).map_err(bad)?;
        let offsets = offset_segments
            .into_iter()
            .map(|(o, raw)| (o, SegmentId::from_raw(raw).expect("a base segment")))
            .collect();
        // SAFETY: `Path` is a `repr(transparent)` `NonZeroU32`, and `parse` checked that every
        // value is the empty path's bits or a node's, which the base installed just now made a
        // path of this process. A `(Path, Path)` is laid out as a `[u32; 2]` is (asserted
        // beside `plain_bytes`).
        let (trailing_key, exact, wild) = unsafe {
            (
                std::slice::from_raw_parts(
                    trailing_key.as_ptr().cast::<Path>(),
                    trailing_key.len(),
                ),
                std::slice::from_raw_parts(exact.as_ptr().cast::<(Path, Path)>(), exact.len()),
                std::slice::from_raw_parts(wild.as_ptr().cast::<(Path, Path)>(), wild.len()),
            )
        };
        Ok(PathSet {
            table: Buf::mapped(table),
            bits: Buf::mapped(bits),
            ranks: Buf::mapped(ranks),
            trailing_key: Buf::mapped(trailing_key),
            trailing_m: Buf::mapped(trailing_m),
            exact_at: Buf::mapped(exact_at),
            exact: Buf::mapped(exact),
            wild_at: Buf::mapped(wild_at),
            wild: Buf::mapped(wild),
            max_depth,
            offsets,
            split_misses: AtomicUsize::new(0),
        })
    }
}

/// A path set file's contents, in the file's numbering: see [`PathSet::write_file`].
struct FileArrays {
    max_depth: usize,
    segments: Vec<PathSegment>,
    nodes: &'static [RawNode],
    node_index: &'static [u64],
    table: &'static [u64],
    bits: &'static [u64],
    ranks: &'static [u32],
    trailing_key: &'static [u32],
    trailing_m: &'static [i64],
    exact_at: &'static [u32],
    exact: &'static [[u32; 2]],
    wild_at: &'static [u32],
    wild: &'static [[u32; 2]],
}

impl FileArrays {
    /// Reads a file's header and sections, and checks that the arrays agree in length and that
    /// every path they hold is the empty path or one of the file's nodes.
    fn parse(bytes: &'static [u8]) -> Result<FileArrays, String> {
        if bytes.len() < HEADER_BYTES {
            return Err(format!("{} bytes is too short", bytes.len()));
        }
        let header: Vec<u64> = bytes[..HEADER_BYTES]
            .chunks_exact(8)
            .map(|c| u64::from_ne_bytes(c.try_into().unwrap()))
            .collect();
        if header[0] != MAGIC || header[1] != VERSION {
            return Err("not a path set of this version".to_string());
        }
        let section = |i: usize| (header[4 + 2 * i], header[5 + 2 * i]);
        let file = FileArrays {
            max_depth: header[2] as usize,
            segments: decode_segments(plain_section(bytes, section(0))?, header[3] as usize)?,
            nodes: plain_section(bytes, section(1))?,
            node_index: plain_section(bytes, section(2))?,
            table: plain_section(bytes, section(3))?,
            bits: plain_section(bytes, section(4))?,
            ranks: plain_section(bytes, section(5))?,
            trailing_key: plain_section(bytes, section(6))?,
            trailing_m: plain_section(bytes, section(7))?,
            exact_at: plain_section(bytes, section(8))?,
            exact: plain_section(bytes, section(9))?,
            wild_at: plain_section(bytes, section(10))?,
            wild: plain_section(bytes, section(11))?,
        };
        let members = file.trailing_m.len();
        if file.trailing_key.len() != members
            || file.exact_at.len() != members + 1
            || file.wild_at.len() != members + 1
            || file.ranks.len() != file.bits.len()
            || !file.table.len().is_power_of_two()
            || file.table.iter().filter(|&&e| e != 0).count() >= file.table.len()
        {
            return Err("its arrays disagree in length".to_string());
        }
        // Every path the arrays hold must be the empty path or a node, so that a lookup reads
        // only the base.
        let nodes_end = file.nodes.len() as u32;
        let is_path = |raw: u32| raw == Path::EMPTY.raw() || (2..=nodes_end).contains(&raw);
        if !file.trailing_key.iter().copied().all(is_path)
            || !file
                .exact
                .iter()
                .chain(file.wild)
                .flatten()
                .copied()
                .all(is_path)
            || !file
                .table
                .iter()
                .all(|&e| e == 0 || (e as u32 != Path::EMPTY.raw() && is_path(e as u32)))
        {
            return Err("an array holds a path that is not in the file".to_string());
        }
        Ok(file)
    }
}

/// The number of members in the words before each word of `bits`.
fn ranks_of(bits: &[u64]) -> Vec<u32> {
    let mut before = 0u32;
    bits.iter()
        .map(|word| {
            let rank = before;
            before += word.count_ones();
            rank
        })
        .collect()
}

/// The first eight bytes of a path set file.
const MAGIC: u64 = u64::from_le_bytes(*b"CTADLPS\0");
/// The file format's version, which a change to the layout or to a hash bumps.
const VERSION: u64 = 1;
/// Segments, nodes, node index, member table, bits, ranks, trailing keys, trailing offsets,
/// exact starts, exact splits, wild starts, wild splits.
const SECTIONS: usize = 12;
/// Magic, version, deepest member, segment count, then each section's byte offset and length
/// in elements.
const HEADER_BYTES: usize = (4 + 2 * SECTIONS) * 8;

/// A `(Path, Path)` is laid out as a `[u32; 2]` is, so a file's splits map as `Splits`'.
const _: () = assert!(
    size_of::<(Path, Path)>() == 8
        && std::mem::offset_of!((Path, Path), 0) == 0
        && std::mem::offset_of!((Path, Path), 1) == 4
);

/// Types any bytes are a value of, with no padding, which a file section holds.
trait Plain: Copy + 'static {}
impl Plain for u8 {}
impl Plain for u32 {}
impl Plain for u64 {}
impl Plain for i64 {}
impl Plain for [u32; 2] {}
impl Plain for [u32; 3] {}

fn plain_bytes<T: Plain>(s: &[T]) -> &[u8] {
    // SAFETY: a `Plain` type has no padding, so every byte of `s` is initialized.
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast(), size_of_val(s)) }
}

/// The section of `bytes` at byte offset `off` holding `len` values of `T`.
fn plain_section<T: Plain>(
    bytes: &'static [u8],
    (off, len): (u64, u64),
) -> Result<&'static [T], String> {
    let start = usize::try_from(off).map_err(|e| e.to_string())?;
    let end = usize::try_from(len)
        .ok()
        .and_then(|len| len.checked_mul(size_of::<T>()))
        .and_then(|n| n.checked_add(start))
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| format!("a section of {len} values at {off} runs past the end"))?;
    let section = &bytes[start..end];
    if !(section.as_ptr() as usize).is_multiple_of(align_of::<T>()) {
        return Err(format!("the section at {off} is misaligned"));
    }
    // SAFETY: the section is in bounds and aligned, and any bytes are a `Plain` value.
    Ok(unsafe { std::slice::from_raw_parts(section.as_ptr().cast(), len as usize) })
}

/// Tags of an encoded segment.
const FIELD: u8 = 0;
const OFFSET: u8 = 1;
const DEREF: u8 = 2;
const VALUE: u8 = 3;

/// Each segment as its tag, then a field's name as its byte length (`u32`) and bytes, or an
/// offset as an `i64`.
fn encode_segments(segments: &[PathSegment]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in segments {
        match s {
            PathSegment::Field(name) => {
                out.push(FIELD);
                out.extend((name.len() as u32).to_ne_bytes());
                out.extend(name.as_bytes());
            }
            PathSegment::Offset(Offset(o)) => {
                out.push(OFFSET);
                out.extend(o.to_ne_bytes());
            }
            PathSegment::Deref => out.push(DEREF),
            PathSegment::Value => out.push(VALUE),
        }
    }
    out
}

fn decode_segments(mut bytes: &[u8], count: usize) -> Result<Vec<PathSegment>, String> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
        if bytes.len() < n {
            return Err("the segments end early".to_string());
        }
        let (head, rest) = bytes.split_at(n);
        *bytes = rest;
        Ok(head)
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(match take(&mut bytes, 1)?[0] {
            FIELD => {
                let len = u32::from_ne_bytes(take(&mut bytes, 4)?.try_into().unwrap());
                let name = std::str::from_utf8(take(&mut bytes, len as usize)?)
                    .map_err(|e| format!("a field name: {e}"))?;
                PathSegment::Field(ArcIntern::from(name))
            }
            OFFSET => PathSegment::Offset(Offset(i64::from_ne_bytes(
                take(&mut bytes, 8)?.try_into().unwrap(),
            ))),
            DEREF => PathSegment::Deref,
            VALUE => PathSegment::Value,
            tag => return Err(format!("a segment has tag {tag}")),
        });
    }
    if !bytes.is_empty() {
        return Err("bytes follow the segments".to_string());
    }
    Ok(out)
}

/// The file numbering of segments and path nodes that [`PathSet::write_file`] builds.
struct Numbering {
    /// The file's bits of each path this process has numbered, by its bits here.
    node_of: HashMap<u32, u32, FxBuildHasher>,
    /// The file's id of each segment, by its id here.
    segment_of: HashMap<u32, u32, FxBuildHasher>,
    /// Segment `i` has id `i + 1`.
    segments: Vec<PathSegment>,
    /// Node `i` has bits `i + 1`; node 0 stands for the empty path's bits.
    nodes: Vec<RawNode>,
    stack: Vec<Path>,
}

impl Default for Numbering {
    fn default() -> Self {
        Numbering {
            node_of: HashMap::default(),
            segment_of: HashMap::default(),
            segments: Vec::new(),
            nodes: vec![[0; 3]],
            stack: Vec::new(),
        }
    }
}

impl Numbering {
    /// `p`'s bits in the file, numbering it and its tails first if they are new.
    fn number(&mut self, p: Path) -> u32 {
        let mut q = p;
        while !q.is_empty() && !self.node_of.contains_key(&q.raw()) {
            self.stack.push(q);
            q = q.tail();
        }
        let (mut tail, mut tail_hash) = if q.is_empty() {
            (0, EMPTY_PATH_HASH)
        } else {
            let raw = self.node_of[&q.raw()];
            (raw, self.nodes[raw as usize - 1][2])
        };
        while let Some(r) = self.stack.pop() {
            let (head, _) = r.uncons_id().expect("a non-empty path");
            let segments = &mut self.segments;
            let head = *self.segment_of.entry(head.raw()).or_insert_with(|| {
                segments.push(head.get().clone());
                segments.len() as u32
            });
            let hash = suffix_hash_raw(head, tail_hash);
            self.nodes.push([head, tail, hash]);
            tail = self.nodes.len() as u32;
            tail_hash = hash;
            self.node_of.insert(r.raw(), tail);
        }
        if tail == 0 { Path::EMPTY.raw() } else { tail }
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
            ".#value",
            ".x.#value",
            ".deref",
            ".[2].deref",
            ".x.[2].deref.#value",
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
        for path in paths {
            for pre in paths {
                for np in paths {
                    let want = p(path)
                        .substitute_prefix(&p(pre), &p(np))
                        .filter(|q| set.contains(q));
                    assert_eq!(
                        set.substitute_prefix(&p(path), &p(pre), &p(np)),
                        want,
                        "{path} [{pre} := {np}]"
                    );
                }
            }
        }
    }

    /// `#value` absorbs itself, in the path set and in [`Path::substitute_prefix`] alike, and
    /// nothing else follows it.
    #[test]
    fn value_absorbs_itself() {
        let v = Path::from_accesses([PathSegment::Value]);
        let x = p(".x");
        let xv = x.concat(&v);
        let set = PathSet::from_paths([Path::EMPTY, x, v, xv, p(".x.deref")]);
        assert_eq!(set.concat(&xv, None, &v), Some(xv));
        assert_eq!(set.concat(&v, None, &v), Some(v));
        assert_eq!(set.concat(&x, None, &v), Some(xv));
        assert_eq!(set.concat(&v, None, &p(".deref")), None);
        assert_eq!(xv.substitute_prefix(&x, &xv), Some(xv));
    }

    /// `#value` absorbs adjacent offsets on both sides (`p.[k].#value = p.#value`,
    /// `p.#value.[k] = p.#value`), at the junction as everywhere.
    #[test]
    fn value_absorbs_offsets() {
        let x = p(".x");
        let xv = p(".x.#value");
        let set = PathSet::from_paths([Path::EMPTY, x, xv, p(".x.[8]")]);
        assert_eq!(set.concat(&p(".x.[8]"), None, &p(".#value")), Some(xv));
        assert_eq!(set.concat(&x, Some(8), &p(".#value")), Some(xv));
        assert_eq!(set.concat(&xv, Some(8), &Path::EMPTY), Some(xv));
        assert_eq!(set.concat(&xv, None, &p(".[8]")), Some(xv));
        assert_eq!(set.concat(&xv, Some(-8), &p(".[8]")), Some(xv));
        assert_eq!(p(".x.[8].#value"), xv);
    }

    /// The segments of the file's path whose bits are `raw`.
    fn file_path(file: &FileArrays, raw: u32) -> Path {
        let mut segments = Vec::new();
        let mut r = raw;
        while r != 0 && r != Path::EMPTY.raw() {
            let [head, tail, _] = file.nodes[r as usize - 1];
            segments.push(file.segments[head as usize - 1].clone());
            r = tail;
        }
        Path::from_accesses(segments)
    }

    /// A unit test's process has interned paths, so it cannot install a base: this reads the
    /// file's arrays back and compares them with the set by content.
    #[test]
    fn a_written_file_reads_back_by_content() {
        let set = PathSet::from_paths(
            [
                "",
                ".x",
                ".x.y",
                ".x.[8]",
                ".x.[8].y",
                ".[4]",
                ".[4].deref",
                ".x.#value",
                ".a.b.c.[2].d",
                ".a.[-3].#value",
            ]
            .map(p),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("path_set.bin");
        set.write_file(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // Copied to aligned memory that outlives the test, as a map does.
        let words = Vec::leak(vec![0u64; bytes.len().div_ceil(8)]);
        // SAFETY: `words` has at least `bytes.len()` bytes, and any bytes are `u8`s.
        let aligned: &'static mut [u8] =
            unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), bytes.len()) };
        aligned.copy_from_slice(&bytes);
        let file = FileArrays::parse(aligned).unwrap();

        assert_eq!(file.max_depth, set.max_depth);
        let mut rank = 0;
        for (w, &word) in file.bits.iter().enumerate() {
            for b in (0..64).filter(|b| word & (1 << b) != 0) {
                let q = file_path(&file, (w * 64 + b) as u32);
                assert!(set.contains(&q), "{q}");
                let splits = |at: &[u32], pairs: &[[u32; 2]]| -> Vec<(Path, Path)> {
                    pairs[at[rank] as usize..at[rank + 1] as usize]
                        .iter()
                        .map(|&[key, rest]| (file_path(&file, key), file_path(&file, rest)))
                        .collect()
                };
                assert_eq!(splits(file.exact_at, file.exact), set.splits(&q).exact);
                assert_eq!(splits(file.wild_at, file.wild), set.splits(&q).wild);
                let m = file.trailing_m[rank];
                assert_eq!(
                    (m != 0).then(|| (file_path(&file, file.trailing_key[rank]), m)),
                    set.split_trailing_offset(&q),
                    "{q}"
                );
                rank += 1;
            }
        }
        assert_eq!(rank, set.len());
        // Each node's hash folds from its tail's in the file's segment numbering.
        for &[head, tail, hash] in &file.nodes[1..] {
            let tail_hash = if tail == 0 {
                EMPTY_PATH_HASH
            } else {
                file.nodes[tail as usize - 1][2]
            };
            assert_eq!(hash, suffix_hash_raw(head, tail_hash));
        }
        // The table holds each non-empty member under its node's hash.
        let entries: Vec<u64> = file.table.iter().copied().filter(|&e| e != 0).collect();
        assert_eq!(entries.len(), set.len() - 1);
        for e in entries {
            assert_eq!((e >> 32) as u32, file.nodes[e as usize % (1 << 32) - 1][2]);
        }
        // The node index finds every node but the placeholder.
        assert_eq!(
            file.node_index.iter().filter(|&&e| e != 0).count(),
            file.nodes.len() - 1
        );
    }

    #[test]
    fn a_file_that_is_not_a_path_set_does_not_parse() {
        let words = Vec::leak(vec![0u64; HEADER_BYTES / 8]);
        // SAFETY: as in `a_written_file_reads_back_by_content`.
        let bytes: &'static [u8] =
            unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), HEADER_BYTES) };
        assert!(FileArrays::parse(bytes).is_err());
        assert!(FileArrays::parse(&bytes[..8]).is_err());
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
