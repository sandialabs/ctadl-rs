//! The difference of two sorted sequences, taken as multisets.

use std::cmp::Ordering;

/// A value whose number of occurrences differs between the two inputs, with
/// both numbers.
pub struct Delta<'a, T> {
    pub row: &'a T,
    pub count_a: usize,
    pub count_b: usize,
}

/// Both inputs sorted by `cmp`. Yields each value whose counts differ, once, in `cmp` order.
///
/// `cmp` must be a total order, and both `a` and `b` must be sorted by it; the result is
/// unspecified otherwise. A value is yielded exactly when the number of elements of `a` equal
/// to it under `cmp` differs from the number in `b`; its `Delta` carries those two numbers as
/// `count_a` and `count_b` (either may be 0), and `row` refers to one of those elements.
pub fn multiset_diff<'a, T, F>(a: &'a [T], b: &'a [T], cmp: F) -> impl Iterator<Item = Delta<'a, T>>
where
    F: Fn(&T, &T) -> Ordering,
{
    let _ = (a, b, cmp);
    todo!();
    // The return type needs a concrete iterator; `!` is not one.
    #[allow(unreachable_code)]
    std::iter::empty()
}

#[cfg(kani)]
mod proofs;
