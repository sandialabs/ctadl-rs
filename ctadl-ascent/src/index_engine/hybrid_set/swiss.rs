//! The Swiss-table machinery [`super::raw::RawTable`] runs on once a set is large: control
//! bytes, word-parallel group scans, quadratic group probing and hashbrown's sizing rules.
//!
//! `locals-trie-benchmark.md` §1 records the spec, which asks for the above-threshold representation to be "implemented like
//! a `hashbrown::HashTable` … not actually use the `HashTable` type or any built-in hashtable", so
//! this is the SwissTable design rebuilt here: **one control byte per bucket** holding the top 7
//! bits of the hash, scanned a whole **group** at a time with word-parallel arithmetic, over a
//! power-of-two bucket array held at ≤ 87.5 % load.
//!
//! ```text
//!   ctrl:  [ 0x2f | EMPTY | 0x71 | EMPTY | 0x03 | ... | mirror of the first 8 ]
//!   data:   ^ bucket i lives at ctrl - (i+1)*size_of::<T>()
//! ```
//!
//! This module is deliberately *only* the mechanism — there is no table type here. The table is
//! [`super::raw::RawTable`], which is a single structure that is a linear-probing table while it
//! is small and switches to this scheme when it is large, without ever being two types or holding
//! a tag. What the two regimes share — the pointer convention above, the allocation shape, the
//! element addressing, the iterator — lives there; what only the large one uses lives here.
//!
//! ## What is the same as hashbrown
//!
//! * **Layout.** One allocation: the element slots, padded up to the metadata alignment, followed
//!   by `buckets + GROUP_WIDTH` control bytes. The trailing [`GROUP_WIDTH`] bytes mirror the first
//!   ones, which is what lets a group be loaded at *any* bucket index without a wrap check. The
//!   element array is indexed backwards from the control pointer, exactly as hashbrown's
//!   `Bucket::from_base_index` does, so one pointer describes the whole table.
//! * **Sizing.** `buckets = (capacity * 8 / 7).next_power_of_two()`, and a table of `b` buckets
//!   holds `b / 8 * 7` elements — hashbrown's `capacity_to_buckets` / `bucket_mask_to_capacity`.
//!   The bucket counts therefore agree with [`super::super::locals_trie::hb_buckets`] at every
//!   size a set actually reaches, which a unit test pins against a real `hashbrown::HashSet`
//!   grown element by element.
//! * **Probing.** Quadratic probing over *groups* with triangular numbers
//!   (`pos += GROUP_WIDTH * i`), which visits every group exactly once on a power-of-two table.
//! * **h1/h2.** The bucket index comes from the low bits of the hash, the control byte from the
//!   top 7. A lookup compares one control byte per bucket and only touches an element when those
//!   7 bits already match, so a miss almost never dereferences the element array.
//!
//! ## What is deliberately different
//!
//! * **No tombstones.** A Datalog index only ever grows: nothing is removed from a group, so
//!   there is no `erase`, no `DELETED` control byte and no rehash-in-place. That removes the
//!   `fix_insert_slot` dance from the insert path (the first empty slot in the probe sequence
//!   *is* the insertion point) and lets `growth_left` be derived from the bucket count and the
//!   element count rather than stored — which is one of the two reasons the whole table fits in
//!   two words. Adding removal later means adding `DELETED` back; the code is written so that is
//!   the only change needed.
//! * **No small tables.** hashbrown supports 4-bucket tables, whose control array is *shorter*
//!   than a group and needs a wrap fixup on the insert path. Nothing here needs them — the small
//!   regime covers everything below the threshold — so the floor is one full group
//!   ([`MIN_BUCKETS`]) and that fixup does not exist. This is the one place the bucket count can
//!   differ from hashbrown's: hashbrown puts 1–3 elements in 4 buckets, this would put them in 8.
//! * **One group implementation.** hashbrown selects an SSE2, NEON or word-parallel group by
//!   target feature; this always uses the word-parallel one ([`Group`], 8 bytes at a time). On
//!   aarch64 — where this is measured — that is exactly what hashbrown itself compiles to, since
//!   its NEON group is also 8 wide. On x86-64 hashbrown would scan 16 buckets per step where this
//!   scans 8; see `locals-trie-benchmark.md` §11.

use std::mem;

/// Control byte for a bucket that has never held an element.
///
/// The high bit distinguishes it from a full bucket, whose control byte is the element's `h2` —
/// 7 bits with the top bit clear. With no removals those are the only two states, so
/// "is this bucket empty?" is a single high-bit test across a whole group.
pub(super) const EMPTY: u8 = 0b1111_1111;

/// Buckets scanned per probe step: the width of the word the control bytes are read through.
///
/// Also the alignment of the metadata in every regime, and therefore of the whole allocation —
/// the small regime's occupancy word is a `u64`, which is the same width and the same alignment.
/// That coincidence is what lets one layout function serve both.
pub const GROUP_WIDTH: usize = mem::size_of::<u64>();

/// Smallest bucket count the large regime ever allocates: one full group, so a group load never
/// runs off the end of a control array shorter than itself.
pub(super) const MIN_BUCKETS: usize = GROUP_WIDTH;

/// Broadcast a byte across a word: `0x2f` → `0x2f2f_2f2f_2f2f_2f2f`.
#[inline]
const fn repeat(byte: u8) -> u64 {
    u64::from_ne_bytes([byte; GROUP_WIDTH])
}

/// The low bits of the hash, which choose the bucket.
#[inline]
pub(super) const fn h1(hash: u64) -> usize {
    hash as usize
}

/// The top 7 bits of the hash, which become the bucket's control byte.
///
/// Taking them from the *top* matters: the bottom bits already chose the bucket, so a control
/// byte cut from the same end would carry no information a probe does not already have.
#[inline]
pub(super) const fn h2(hash: u64) -> u8 {
    (hash >> (u64::BITS - 7)) as u8
}

// ---------------------------------------------------------------------------
// Group: GROUP_WIDTH control bytes scanned in parallel inside one u64.
// ---------------------------------------------------------------------------

/// [`GROUP_WIDTH`] control bytes, loaded into a register and queried with word arithmetic.
#[derive(Copy, Clone)]
pub(super) struct Group(u64);

impl Group {
    /// Load the group starting at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr..ptr + GROUP_WIDTH` must be inside a control array. Alignment is not required: the
    /// load is unaligned, exactly as hashbrown's is, because probe positions are arbitrary.
    #[inline]
    pub(super) unsafe fn load(ptr: *const u8) -> Self {
        // SAFETY: the caller guarantees `GROUP_WIDTH` readable bytes at `ptr`; `read_unaligned`
        // imposes no alignment requirement of its own.
        Self(unsafe { ptr.cast::<u64>().read_unaligned() })
    }

    /// Buckets in this group whose control byte is `byte` — i.e. the candidate matches for a
    /// lookup. `byte` must have its high bit clear (it is an `h2`), so [`EMPTY`] never matches.
    ///
    /// The classic word-parallel byte compare: xor makes matching bytes zero, and
    /// `x.wrapping_sub(0x01…) & !x & 0x80…` has the high bit set exactly in the zero bytes. It
    /// can also report the high bit of a byte that is one *less* than a match, which is why the
    /// caller must still compare the element — a false positive costs one comparison, and
    /// [`EMPTY`] (`0xff`) cannot produce one for any `h2` (`≤ 0x7f`).
    #[inline]
    pub(super) fn match_byte(self, byte: u8) -> BitMask {
        debug_assert!(byte & 0x80 == 0);
        let cmp = self.0 ^ repeat(byte);
        BitMask((cmp.wrapping_sub(repeat(0x01)) & !cmp & repeat(0x80)).to_le())
    }

    /// Buckets in this group that are empty. With no tombstones, "not full" and "empty" are the
    /// same question and it is one masked test.
    #[inline]
    pub(super) fn match_empty(self) -> BitMask {
        BitMask((self.0 & repeat(0x80)).to_le())
    }

    /// Buckets in this group that hold an element.
    #[inline]
    pub(super) fn match_full(self) -> BitMask {
        BitMask((!self.0 & repeat(0x80)).to_le())
    }
}

/// One bit per matching bucket of a [`Group`], in the high bit of each byte — so a match on
/// bucket `i` is bit `(i << `[`BitMask::SHIFT`]`) + 7`, and `trailing_zeros() >> SHIFT` names the
/// lowest matching bucket.
///
/// That spacing is the one thing the two regimes' metadata do not agree on: the small regime's
/// occupancy word is one bit per slot, this is one byte per bucket. [`Self::SHIFT`] is the whole
/// of the difference, which is why the shared iterator can carry it as a number instead of a tag
/// (see [`super::raw::RawIter`]).
#[derive(Copy, Clone)]
pub(super) struct BitMask(pub(super) u64);

impl BitMask {
    /// Bit positions in a [`BitMask`] are bucket indices shifted left by this much.
    pub(super) const SHIFT: u32 = 3;

    #[inline]
    pub(super) fn any(self) -> bool {
        self.0 != 0
    }

    /// Offset within the group of the lowest matching bucket.
    #[inline]
    pub(super) fn lowest(self) -> Option<usize> {
        if self.0 == 0 {
            None
        } else {
            Some(self.0.trailing_zeros() as usize >> Self::SHIFT)
        }
    }
}

impl Iterator for BitMask {
    type Item = usize;
    #[inline]
    fn next(&mut self) -> Option<usize> {
        let bit = self.lowest()?;
        // Clear the lowest set bit.
        self.0 &= self.0 - 1;
        Some(bit)
    }
}

// ---------------------------------------------------------------------------
// Probe sequence.
// ---------------------------------------------------------------------------

/// Quadratic probing over groups: visit group `h1 & mask`, then step by `GROUP_WIDTH`,
/// `2 * GROUP_WIDTH`, … Because the steps are `GROUP_WIDTH` times the triangular numbers and the
/// bucket count is a power of two, this visits every group exactly once before repeating.
pub(super) struct ProbeSeq {
    pub(super) pos: usize,
    stride: usize,
}

impl ProbeSeq {
    #[inline]
    pub(super) fn new(hash: u64, bucket_mask: usize) -> Self {
        Self {
            pos: h1(hash) & bucket_mask,
            stride: 0,
        }
    }

    #[inline]
    pub(super) fn move_next(&mut self, bucket_mask: usize) {
        debug_assert!(
            self.stride <= bucket_mask,
            "probed every group without finding a slot; the table has no empty bucket"
        );
        self.stride += GROUP_WIDTH;
        self.pos = (self.pos + self.stride) & bucket_mask;
    }
}

// ---------------------------------------------------------------------------
// Sizing.
// ---------------------------------------------------------------------------

/// Elements a table of `buckets` buckets may hold: 87.5 % of them, so at least one bucket in
/// every group's worth stays empty and a probe sequence always terminates. hashbrown's
/// `bucket_mask_to_capacity`, minus the `buckets < 8` case [`MIN_BUCKETS`] rules out.
#[inline]
pub(super) const fn capacity_for(buckets: usize) -> usize {
    buckets / 8 * 7
}

/// Buckets needed to hold `capacity` elements: hashbrown's `capacity_to_buckets`, again without
/// the small-table cases. `0` asks for no allocation at all.
#[inline]
pub(super) fn buckets_for(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // Note the truncating division, which is hashbrown's: capacity 7 wants 8 buckets, not 16.
    (capacity * 8 / 7).next_power_of_two().max(MIN_BUCKETS)
}

/// Control bytes a `buckets`-bucket table needs: one per bucket plus the group mirror.
#[inline]
pub(super) const fn ctrl_len(buckets: usize) -> usize {
    buckets + GROUP_WIDTH
}

#[cfg(test)]
mod tests {
    use super::{BitMask, EMPTY, GROUP_WIDTH, Group, buckets_for, capacity_for, h2};

    #[test]
    fn sizing_follows_hashbrowns_rules() {
        assert_eq!(buckets_for(0), 0);
        // 87.5 % load, power-of-two buckets, one group minimum.
        for (capacity, buckets) in [
            (1usize, 8usize),
            (7, 8),
            (8, 16),
            (14, 16),
            (15, 32),
            (28, 32),
            (56, 64),
            (64, 128),
            (112, 128),
            (113, 256),
        ] {
            assert_eq!(buckets_for(capacity), buckets, "capacity {capacity}");
            assert!(
                capacity_for(buckets) >= capacity,
                "capacity {capacity} does not fit {buckets} buckets"
            );
        }
    }

    /// The word-parallel group scan agrees with a byte-at-a-time one, including the
    /// `SHIFT`-spaced bit positions the shared iterator relies on.
    #[test]
    fn group_scans_agree_with_a_byte_loop() {
        let bytes: [u8; GROUP_WIDTH] = [0x00, EMPTY, 0x71, EMPTY, 0x03, 0x71, EMPTY, 0x7f];
        // SAFETY: `bytes` is exactly `GROUP_WIDTH` readable bytes.
        let group = unsafe { Group::load(bytes.as_ptr()) };

        let full: Vec<usize> = group.match_full().collect();
        let expected: Vec<usize> = (0..GROUP_WIDTH).filter(|&i| bytes[i] != EMPTY).collect();
        assert_eq!(full, expected);

        let empty: Vec<usize> = group.match_empty().collect();
        assert_eq!(
            empty,
            (0..GROUP_WIDTH)
                .filter(|&i| bytes[i] == EMPTY)
                .collect::<Vec<_>>()
        );
        assert!(group.match_empty().any());

        // `match_byte` may report false positives, so it is only required to be a *superset* of
        // the true matches — and never to report an empty bucket.
        for byte in [0x00u8, 0x03, 0x71, 0x7f] {
            let got: Vec<usize> = group.match_byte(byte).collect();
            for (i, &b) in bytes.iter().enumerate() {
                if b == byte {
                    assert!(got.contains(&i), "byte {byte:#x} missed bucket {i}");
                }
            }
            for &i in &got {
                assert_ne!(bytes[i], EMPTY, "byte {byte:#x} matched an empty bucket");
            }
        }

        // Bit positions are bucket indices shifted by `SHIFT`.
        assert_eq!(
            group.match_full().0.trailing_zeros() as usize >> BitMask::SHIFT,
            0
        );
    }

    #[test]
    fn h2_never_collides_with_empty() {
        for hash in [
            0u64,
            1,
            u64::MAX,
            0x8000_0000_0000_0000,
            0x1234_5678_9abc_def0,
        ] {
            assert_eq!(h2(hash) & 0x80, 0, "h2 must leave the high bit clear");
            assert_ne!(h2(hash), EMPTY);
        }
    }
}
