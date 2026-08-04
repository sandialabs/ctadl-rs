//! The Swiss-table machinery that [`super::raw::RawTable`] runs on once a set is large: control
//! bytes, word-parallel group scans, quadratic group probing, and hashbrown's sizing rules.
//!
//! The spec this implements asks for the above-threshold representation to be "implemented like a
//! `hashbrown::HashTable` … not actually use the `HashTable` type or any built-in hashtable". So
//! we rebuild the SwissTable design here: **one control byte per bucket**
//! holding the top 7 bits of the hash, scanned a whole **group** at a time with word-parallel
//! arithmetic, over a power-of-two bucket array held at 87.5% load or less.
//!
//! ```text
//!   ctrl:  [ 0x2f | EMPTY | 0x71 | EMPTY | 0x03 | ... | mirror of the first 8 ]
//!   data:   ^ bucket i lives at ctrl - (i+1)*size_of::<T>()
//! ```
//!
//! This module is deliberately *only* the mechanism; there is no table type here. The table is
//! [`super::raw::RawTable`], a single structure that is a linear-probing table while it is small
//! and switches to this scheme when it is large, without ever being two types or holding a tag.
//! Whatever the two regimes share lives there: the pointer convention above, the allocation shape,
//! the element addressing, and the iterator. Whatever only the large regime uses lives here.
//!
//! ## What is the same as hashbrown
//!
//! * **Layout.** One allocation, holding the element slots, padded up to the metadata alignment,
//!   followed by `buckets + GROUP_WIDTH` control bytes. The trailing [`GROUP_WIDTH`] bytes mirror
//!   the first ones, which is what lets us load a group at *any* bucket index without a wrap
//!   check. The element array is indexed backwards from the control pointer, exactly as
//!   hashbrown's `Bucket::from_base_index` does, so one pointer describes the whole table.
//! * **Sizing.** `buckets = (capacity * 8 / 7).next_power_of_two()`, and a table of `b` buckets
//!   holds `b / 8 * 7` elements. Those are hashbrown's `capacity_to_buckets` and
//!   `bucket_mask_to_capacity`. Our bucket counts therefore agree with
//!   [`super::super::locals_trie::hb_buckets`] at every size a set actually reaches, and a unit
//!   test pins that against a real `hashbrown::HashSet` grown element by element.
//! * **Probing.** Quadratic probing over *groups*, using triangular numbers
//!   (`pos += GROUP_WIDTH * i`). On a power-of-two table that visits every group exactly once.
//! * **h1/h2.** The bucket index comes from the low bits of the hash, and the control byte from
//!   the top 7. A lookup compares one control byte per bucket, and touches an element only when
//!   those 7 bits already match, so a miss almost never dereferences the element array.
//!
//! ## What is deliberately different
//!
//! * **No tombstones.** A Datalog index only ever grows. Nothing is removed from a group, so
//!   there is no `erase`, no `DELETED` control byte, and no rehash-in-place. That drops the
//!   `fix_insert_slot` dance from the insert path, because the first empty slot in the probe
//!   sequence *is* the insertion point. It also lets us derive `growth_left` from the bucket
//!   count and the element count rather than store it, which is one of the two reasons the whole
//!   table fits in two words. Adding removal later means adding `DELETED` back, and the code is
//!   written so that is the only change needed.
//! * **No small tables.** hashbrown supports 4-bucket tables, whose control array is *shorter*
//!   than a group and needs a wrap fixup on the insert path. We need none of that, because the
//!   small regime covers everything below the threshold. So our floor is one full group
//!   ([`MIN_BUCKETS`]) and the fixup does not exist. This is the one place our bucket count can
//!   differ from hashbrown's: hashbrown puts 1 to 3 elements in 4 buckets, and we would put them
//!   in 8.
//! * **One group implementation.** hashbrown selects an SSE2, NEON, or word-parallel group by
//!   target feature. We always use the word-parallel one ([`Group`], 8 bytes at a time). On
//!   aarch64, where we measure, that is exactly what hashbrown itself compiles to, because its
//!   NEON group is also 8 wide. On x86-64, hashbrown would scan 16 buckets per step where we scan
//!   8, so the timings we quote should not be assumed to carry there. The *bytes* are
//!   platform-independent, and 8 B per table smaller than hashbrown's on x86-64.

use std::mem;

/// Control byte for a bucket that has never held an element.
///
/// The high bit distinguishes it from a full bucket, whose control byte is the element's `h2`:
/// 7 bits with the top bit clear. With no removals those are the only two states, so "is this
/// bucket empty?" is a single high-bit test across a whole group.
pub(super) const EMPTY: u8 = 0b1111_1111;

/// How many buckets we scan per probe step. It is the width of the word we read the control bytes
/// through.
///
/// It is also the alignment of the metadata in every regime, and therefore of the whole
/// allocation, because the small regime's occupancy word is a `u64` of the same width and
/// alignment. That coincidence is what lets one layout function serve both.
pub const GROUP_WIDTH: usize = mem::size_of::<u64>();

/// The smallest bucket count the large regime ever allocates. It is one full group, so a group
/// load never runs off the end of a control array shorter than itself.
pub(super) const MIN_BUCKETS: usize = GROUP_WIDTH;

/// Broadcast a byte across a word, turning `0x2f` into `0x2f2f_2f2f_2f2f_2f2f`.
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
/// Taking them from the *top* matters. The bottom bits already chose the bucket, so a control byte
/// cut from the same end would carry no information a probe does not already have.
#[inline]
pub(super) const fn h2(hash: u64) -> u8 {
    (hash >> (u64::BITS - 7)) as u8
}

// ---------------------------------------------------------------------------
// Group: GROUP_WIDTH control bytes, scanned in parallel inside one u64.
// ---------------------------------------------------------------------------

/// [`GROUP_WIDTH`] control bytes, loaded into a register and queried with word arithmetic.
#[derive(Copy, Clone)]
pub(super) struct Group(u64);

impl Group {
    /// Load the group starting at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr..ptr + GROUP_WIDTH` must be inside a control array. No alignment is required, because
    /// probe positions are arbitrary and so the load is unaligned, exactly as hashbrown's is.
    #[inline]
    pub(super) unsafe fn load(ptr: *const u8) -> Self {
        // SAFETY: the caller guarantees `GROUP_WIDTH` readable bytes at `ptr`, and
        // `read_unaligned` imposes no alignment requirement of its own.
        Self(unsafe { ptr.cast::<u64>().read_unaligned() })
    }

    /// The buckets in this group whose control byte is `byte`, which are the candidate matches
    /// for a lookup. `byte` must have its high bit clear, because it is an `h2`, so [`EMPTY`]
    /// never matches.
    ///
    /// This is the classic word-parallel byte compare. The xor makes matching bytes zero, and
    /// `x.wrapping_sub(0x01…) & !x & 0x80…` has the high bit set in exactly the zero bytes. It can
    /// also report the high bit of a byte that is one *less* than a match, which is why the caller
    /// must still compare the element. A false positive costs one comparison, and [`EMPTY`]
    /// (`0xff`) cannot produce one for any `h2`, which is at most `0x7f`.
    #[inline]
    pub(super) fn match_byte(self, byte: u8) -> BitMask {
        debug_assert!(byte & 0x80 == 0);
        let cmp = self.0 ^ repeat(byte);
        BitMask((cmp.wrapping_sub(repeat(0x01)) & !cmp & repeat(0x80)).to_le())
    }

    /// The buckets in this group that are empty. With no tombstones, "not full" and "empty" are
    /// the same question, and answering it is one masked test.
    #[inline]
    pub(super) fn match_empty(self) -> BitMask {
        BitMask((self.0 & repeat(0x80)).to_le())
    }

    /// The buckets in this group that hold an element.
    #[inline]
    pub(super) fn match_full(self) -> BitMask {
        BitMask((!self.0 & repeat(0x80)).to_le())
    }
}

/// One bit per matching bucket of a [`Group`], held in the high bit of each byte. So a match on
/// bucket `i` is bit `(i << `[`BitMask::SHIFT`]`) + 7`, and `trailing_zeros() >> SHIFT` names the
/// lowest matching bucket.
///
/// That spacing is the one thing the two regimes' metadata do not agree on. The small regime's
/// occupancy word is one bit per slot; this is one byte per bucket. [`Self::SHIFT`] is the whole
/// of the difference, which is why the shared iterator can carry it as a number instead of a tag.
/// See [`super::raw::RawIter`].
#[derive(Copy, Clone)]
pub(super) struct BitMask(pub(super) u64);

impl BitMask {
    /// Bit positions in a [`BitMask`] are bucket indices shifted left by this much.
    pub(super) const SHIFT: u32 = 3;

    #[inline]
    pub(super) fn any(self) -> bool {
        self.0 != 0
    }

    /// The offset within the group of the lowest matching bucket.
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

/// Quadratic probing over groups. It visits group `h1 & mask`, then steps by `GROUP_WIDTH`, then
/// by `2 * GROUP_WIDTH`, and so on. The steps are `GROUP_WIDTH` times the triangular numbers, and
/// the bucket count is a power of two, so this visits every group exactly once before repeating.
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

/// How many elements a table of `buckets` buckets may hold. The answer is 87.5% of them, so at
/// least one bucket in every group's worth stays empty and a probe sequence always terminates.
/// This is hashbrown's `bucket_mask_to_capacity`, minus the `buckets < 8` case that
/// [`MIN_BUCKETS`] rules out.
#[inline]
pub(super) const fn capacity_for(buckets: usize) -> usize {
    buckets / 8 * 7
}

/// How many buckets we need to hold `capacity` elements. This is hashbrown's
/// `capacity_to_buckets`, again without the small-table cases. A `capacity` of `0` asks for no
/// allocation at all.
#[inline]
pub(super) fn buckets_for(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // Note the truncating division, which is hashbrown's. Capacity 7 wants 8 buckets, not 16.
    (capacity * 8 / 7).next_power_of_two().max(MIN_BUCKETS)
}

/// How many control bytes a `buckets`-bucket table needs: one per bucket, plus the group mirror.
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
        // 87.5% load, power-of-two buckets, one group minimum.
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

    /// The word-parallel group scan must agree with a byte-at-a-time one. That includes the
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

        // `match_byte` may report false positives. So we require only that it be a *superset* of
        // the true matches, and that it never report an empty bucket.
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

        // A bit position is a bucket index shifted by `SHIFT`.
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
