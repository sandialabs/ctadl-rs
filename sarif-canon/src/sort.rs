//! A verified merge sort.

use std::cmp::Ordering;
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use crate::order::equal_means_eq;
#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, sorted, total_order};

verus! {

/// `v`'s elements sorted by the order `cmp` computes, none dropped or merged. Not `std`'s sort,
/// whose output is not specified to Verus.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn sort_by<T, F: Fn(&T, &T) -> Ordering>(
    v: Vec<T>,
    cmp: &F,
    Ghost(ord): Ghost<spec_fn(T, T) -> Ordering>,
) -> (r: Vec<T>)
    requires
        total_order(ord),
        computes(*cmp, ord),
    ensures
        sorted(ord, r@),
        r@.to_multiset() == v@.to_multiset(),
{
    unimplemented!()
}

/// Under a total order whose `Equal` is equality, a multiset has one sorted sequence.
pub proof fn lemma_sorted_unique<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        sorted(ord, s),
        sorted(ord, t),
        s.to_multiset() == t.to_multiset(),
    ensures
        s == t,
{
    admit();
}

} // verus!
