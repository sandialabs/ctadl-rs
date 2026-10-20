//! A split-block Bloom filter over a run's keys, so a lookup can skip the runs
//! that cannot hold its key without searching them. A filter is a slice of
//! [`Block`]s, which a run keeps in its own file.
//!
//! Each key sets one bit in each of the eight 32-bit words of one 256-bit block,
//! as Parquet's filters do: a probe reads a single cache line. At
//! [`BITS_PER_KEY`] the false-positive rate is about 1%.

use bytemuck::Pod;

const BITS_PER_KEY: usize = 10;
const BLOCK_BITS: usize = 256;
/// One odd multiplier per word of a block, from Parquet's split-block filter.
const SALT: [u32; 8] = [
    0x47b6_137b,
    0x4497_4d91,
    0x8824_ad5b,
    0xa2b7_289d,
    0x7054_95c7,
    0x2df1_424b,
    0x9efc_4947,
    0x5c6b_fb31,
];

/// One block of a filter.
pub(crate) type Block = [u32; 8];

/// The number of blocks in a filter sized for `keys` keys.
pub(crate) fn blocks_for(keys: usize) -> usize {
    (keys.max(1) * BITS_PER_KEY).div_ceil(BLOCK_BITS)
}

fn block(blocks: usize, hash: u64) -> usize {
    // The high half picks the block, by multiply-shift over the block count.
    (((hash >> 32) * blocks as u64) >> 32) as usize
}

fn masks(hash: u64) -> [u32; 8] {
    // The low half picks one bit per word.
    let low = hash as u32;
    SALT.map(|salt| 1 << (low.wrapping_mul(salt) >> 27))
}

/// Adds a key with this hash to the filter `blocks`.
pub(crate) fn insert(blocks: &mut [Block], hash: u64) {
    let block = block(blocks.len(), hash);
    for (word, mask) in blocks[block].iter_mut().zip(masks(hash)) {
        *word |= mask;
    }
}

/// False only if no key with this hash was added to the filter `blocks`.
#[inline]
pub(crate) fn may_contain(blocks: &[Block], hash: u64) -> bool {
    let block = &blocks[block(blocks.len(), hash)];
    block
        .iter()
        .zip(masks(hash))
        .all(|(word, mask)| word & mask != 0)
}

/// The hash a filter is keyed on: the key's bytes (see [`hash_bytes`]).
#[inline]
pub(crate) fn hash_key<K: Pod>(key: &K) -> u64 {
    hash_bytes(bytemuck::bytes_of(key))
}

/// A hash of `bytes`, a word at a time, through a multiply-rotate round and a
/// final avalanche, so both halves are well mixed.
#[inline]
pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    const K0: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = K0 ^ bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        let w = u64::from_ne_bytes(word.try_into().unwrap());
        h = (h ^ w).wrapping_mul(K0).rotate_left(31);
    }
    let rest = words.remainder();
    if !rest.is_empty() {
        let mut w = [0u8; 8];
        w[..rest.len()].copy_from_slice(rest);
        h = (h ^ u64::from_ne_bytes(w)).wrapping_mul(K0).rotate_left(31);
    }
    // The murmur3 finalizer.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_and_few_false_positives() {
        let keys = 10_000u64;
        let mut bloom = vec![[0; 8]; blocks_for(keys as usize)];
        for k in 0..keys {
            insert(&mut bloom, hash_key(&[k, k ^ 0xabcd]));
        }
        for k in 0..keys {
            assert!(may_contain(&bloom, hash_key(&[k, k ^ 0xabcd])));
        }
        let false_positives = (keys..keys * 11)
            .filter(|&k| may_contain(&bloom, hash_key(&[k, k ^ 0xabcd])))
            .count();
        let rate = false_positives as f64 / (keys * 10) as f64;
        assert!(rate < 0.03, "false-positive rate {rate}");
    }
}
