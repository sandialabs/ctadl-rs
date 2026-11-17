//! `multiset_diff`'s properties, the ones its Verus proof states, tested on the crate's own code.

use std::collections::{BTreeMap, BTreeSet};

use multiset_diff::multiset_diff;
use proptest::prelude::*;

/// A sorted sequence of bytes. Half the elements are drawn from a few small values, so that
/// repeats, and values in both inputs, are common.
fn sorted() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop_oneof![0u8..8, any::<u8>()], 0..256).prop_map(|mut v| {
        v.sort();
        v
    })
}

/// What `multiset_diff` yields, in order, as `(value, count_a, count_b)`.
fn diff(a: &[u8], b: &[u8]) -> Vec<(u8, usize, usize)> {
    multiset_diff(a, b, Ord::cmp)
        .map(|d| (*d.row, d.count_a, d.count_b))
        .collect()
}

/// Each value's number of occurrences in `s`, counted without the core.
fn counts(s: &[u8]) -> BTreeMap<u8, usize> {
    let mut m = BTreeMap::new();
    for &x in s {
        *m.entry(x).or_default() += 1;
    }
    m
}

proptest! {
    #[test]
    fn self_diff_is_empty(a in sorted()) {
        prop_assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn swap_swaps_sides(a in sorted(), b in sorted()) {
        let swapped: Vec<_> = diff(&b, &a).into_iter().map(|(v, x, y)| (v, y, x)).collect();
        prop_assert_eq!(diff(&a, &b), swapped);
    }

    #[test]
    fn reports_exactly_count_differences(a in sorted(), b in sorted()) {
        let (ca, cb) = (counts(&a), counts(&b));
        let expected: Vec<_> = ca
            .keys()
            .chain(cb.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|v| (*v, ca.get(v).copied().unwrap_or(0), cb.get(v).copied().unwrap_or(0)))
            .filter(|&(_, x, y)| x != y)
            .collect();
        let mut got = diff(&a, &b);
        got.sort();
        prop_assert_eq!(got, expected);
    }

    #[test]
    fn no_value_twice(a in sorted(), b in sorted()) {
        let deltas = diff(&a, &b);
        prop_assert!(deltas.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
