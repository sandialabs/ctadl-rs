//! The difference of two sorted sequences, taken as multisets.
//!
//! The merge is verified with Verus, for inputs of any length: run
//! `verus --crate-type=lib --edition=2024 src/lib.rs`, or `nix build .#checks.<system>.verus`.
//! A plain `cargo build` erases the specs and proofs, leaving the code.
//!
//! What is proved:
//!
//! - `next` yields exactly `diff_from(ord, a, b, 0, 0)`, a spec-level statement of the merge,
//!   for any `T` and any `cmp` that computes a total order `ord` (`multiset_diff_by`).
//! - Of `diff_from`, the four properties the crate promises (in `proofs`):
//!   `theorem_self_diff_is_empty`, `theorem_swap_swaps_sides`,
//!   `theorem_reports_exactly_count_differences` and `theorem_no_value_twice`. Values are the
//!   same when `ord` calls them `Equal`; counts count those.
//! - `proofs::client` restates the properties with `==` and plain counts, for `u8` under
//!   `Ord::cmp`, over what the iterator yields.
//!
//! The specs and `proofs` are public so that another verified crate can use the theorems
//! about its own calls: verify this crate with `--export <file> --compile`, and that one with
//! `--import multiset_diff=<file> --extern multiset_diff=<rlib>`, as `checks.verus` does.

use std::cmp::Ordering;
use vstd::prelude::*;
// The iterator specs exist only when Verus compiles the crate.
#[cfg(verus_keep_ghost)]
use vstd::std_specs::iter::{IteratorSpec, IteratorSpecImpl};

pub mod proofs;

verus! {

/// A value whose number of occurrences differs between the two inputs, with
/// both numbers.
pub struct Delta<'a, T> {
    pub row: &'a T,
    pub count_a: usize,
    pub count_b: usize,
}

// ---------------------------------------------------------------------------
// The order. The specs are stated over a spec-level order `ord`, which the caller names and
// `cmp` must compute (`computes`): Verus knows of a closure only that what it returns meets
// its `ensures`, which cannot pin down an order by itself.

pub open spec fn eqv<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) == Ordering::Equal
}

pub open spec fn lt<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) == Ordering::Less
}

pub open spec fn le<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) != Ordering::Greater
}

/// `ord` is a total order: antisymmetric and transitive (reflexivity follows). Values it
/// calls `Equal` need not be identical.
pub open spec fn total_order<T>(ord: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|x: T, y: T|
        (#[trigger] ord(x, y) == Ordering::Less) <==> ord(y, x) == Ordering::Greater
    &&& forall|x: T, y: T| (#[trigger] ord(x, y) == Ordering::Equal) ==> ord(y, x) == Ordering::Equal
    &&& forall|x: T, y: T, z: T|
        #[trigger] le(ord, x, y) && #[trigger] le(ord, y, z) ==> le(ord, x, z)
}

/// `cmp` can be called on any two values, and what it returns is `ord` of them.
pub open spec fn computes<T, F: Fn(&T, &T) -> Ordering>(cmp: F, ord: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|x: &T, y: &T| #[trigger] cmp.requires((x, y))
    &&& forall|x: &T, y: &T, o: Ordering| #[trigger] cmp.ensures((x, y), o) ==> o == ord(*x, *y)
}

pub open spec fn sorted<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> le(ord, #[trigger] s[p], #[trigger] s[q])
}

/// The number of elements of `s[lo..hi]` equivalent to `v` under `ord`.
pub open spec fn count<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
) -> nat
    decreases hi - lo,
{
    if hi <= lo {
        0
    } else {
        count(ord, s, lo, hi - 1, v) + if eqv(ord, s[hi - 1], v) {
            1nat
        } else {
            0nat
        }
    }
}

/// The number of elements of `s` equivalent to `v` under `ord`.
pub open spec fn count_all<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, v: T) -> nat {
    count(ord, s, 0, s.len() as int, v)
}

// ---------------------------------------------------------------------------
// The merge, as a spec function. `next` is proved to yield exactly this.

/// The end of the run of elements of `s` from `i` that are equivalent to `v`.
pub open spec fn run_end<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, i: int, v: T) -> int
    decreases s.len() - i,
{
    if 0 <= i < s.len() && eqv(ord, s[i], v) {
        run_end(ord, s, i + 1, v)
    } else {
        i
    }
}

/// Whether the next value comes from `a`: `b` is used up, or `a[i] <= b[j]`.
pub open spec fn head_from_a<T>(
    ord: spec_fn(T, T) -> Ordering,
    a: Seq<T>,
    b: Seq<T>,
    i: int,
    j: int,
) -> bool {
    i < a.len() && (j >= b.len() || ord(a[i], b[j]) != Ordering::Greater)
}

/// The merge's next step from positions `i` and `j`: the value it looks at next (from `a` if
/// `head_from_a`), and the ends of that value's runs in `a` and `b`. The side the value comes
/// from starts its run past it, so a step always advances.
pub open spec fn step<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
) -> (&'a T, int, int) {
    if head_from_a(ord, a@, b@, i, j) {
        (&a@[i], run_end(ord, a@, i + 1, a@[i]), run_end(ord, b@, j, a@[i]))
    } else {
        (&b@[j], run_end(ord, a@, i, b@[j]), run_end(ord, b@, j + 1, b@[j]))
    }
}

/// What the merge yields from positions `i` in `a` and `j` in `b`.
pub open spec fn diff_from<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
) -> Seq<Delta<'a, T>>
    decreases a@.len() - i + b@.len() - j,
    via diff_from_decreases::<T>
{
    if !(0 <= i <= a@.len() && 0 <= j <= b@.len()) || (i == a@.len() && j == b@.len()) {
        seq![]
    } else {
        let (row, ia, jb) = step(ord, a, b, i, j);
        let rest = diff_from(ord, a, b, ia, jb);
        if ia - i != jb - j {
            seq![Delta { row, count_a: (ia - i) as usize, count_b: (jb - j) as usize }] + rest
        } else {
            rest
        }
    }
}

#[via_fn]
proof fn diff_from_decreases<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
) {
    if !(0 <= i <= a@.len() && 0 <= j <= b@.len()) || (i == a@.len() && j == b@.len()) {
    } else {
        lemma_step(ord, a, b, i, j);
    }
}

// ---------------------------------------------------------------------------
// Lemmas about runs and steps, which need no order laws.

proof fn lemma_run_end<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, i: int, v: T)
    requires
        0 <= i <= s.len(),
    ensures
        i <= run_end(ord, s, i, v) <= s.len(),
        forall|k: int| i <= k < run_end(ord, s, i, v) ==> eqv(ord, #[trigger] s[k], v),
        run_end(ord, s, i, v) < s.len() ==> !eqv(ord, s[run_end(ord, s, i, v)], v),
    decreases s.len() - i,
{
    if i < s.len() && eqv(ord, s[i], v) {
        lemma_run_end(ord, s, i + 1, v);
    }
}

proof fn lemma_step<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
        i < a@.len() || j < b@.len(),
    ensures
        ({
            let (row, ia, jb) = step(ord, a, b, i, j);
            &&& i <= ia <= a@.len()
            &&& j <= jb <= b@.len()
            &&& ia + jb > i + j
            &&& head_from_a(ord, a@, b@, i, j) ==> row == &a@[i] && ia > i
            &&& !head_from_a(ord, a@, b@, i, j) ==> row == &b@[j] && jb > j
        }),
{
    if head_from_a(ord, a@, b@, i, j) {
        lemma_run_end(ord, a@, i + 1, a@[i]);
        lemma_run_end(ord, b@, j, a@[i]);
    } else {
        lemma_run_end(ord, a@, i, b@[j]);
        lemma_run_end(ord, b@, j + 1, b@[j]);
    }
}

// ---------------------------------------------------------------------------
// The iterator: the code.

#[verifier::reject_recursive_types(T)]
struct MultisetDiff<'a, T, F: Fn(&T, &T) -> Ordering> {
    a: &'a [T],
    b: &'a [T],
    cmp: F,
    // Read only by the proof; zero-sized.
    #[allow(dead_code)]
    ord: Ghost<spec_fn(T, T) -> Ordering>,
    i: usize,
    j: usize,
}

impl<'a, T, F: Fn(&T, &T) -> Ordering> MultisetDiff<'a, T, F> {
    #[verifier::type_invariant]
    closed spec fn inv(self) -> bool {
        &&& self.i <= self.a@.len()
        &&& self.j <= self.b@.len()
        &&& total_order(self.ord@)
        &&& computes(self.cmp, self.ord@)
    }

    closed spec fn rest(&self) -> Seq<Delta<'a, T>> {
        diff_from(self.ord@, self.a, self.b, self.i as int, self.j as int)
    }
}

/// `multiset_diff`, with the order `cmp` computes named, so that what it yields can be stated.
/// Only the proof calls it.
#[cfg(verus_keep_ghost)]
pub(crate) fn multiset_diff_by<'a, T, F>(
    a: &'a [T],
    b: &'a [T],
    cmp: F,
    Ghost(ord): Ghost<spec_fn(T, T) -> Ordering>,
) -> (r: impl Iterator<Item = Delta<'a, T>>)
    where
        F: Fn(&T, &T) -> Ordering,
    requires
        total_order(ord),
        computes(cmp, ord),
    ensures
        IteratorSpec::obeys_prophetic_iter_laws(&r),
        IteratorSpec::remaining(&r) == diff_from(ord, a, b, 0, 0),
        IteratorSpec::decrease(&r) is Some,
{
    MultisetDiff { a, b, cmp, ord: Ghost(ord), i: 0, j: 0 }
}

/// Both inputs sorted by `cmp`. Yields each value whose counts differ, once, in `cmp` order.
///
/// `cmp` must be a total order, and both `a` and `b` must be sorted by it; the result is
/// unspecified otherwise. A value is yielded exactly when the number of elements of `a` equal
/// to it under `cmp` differs from the number in `b`; its `Delta` carries those two numbers as
/// `count_a` and `count_b` (either may be 0), and `row` refers to one of those elements.
///
/// Its postcondition can only name an order `cmp` computes by choosing one, and no verified
/// caller can show its inputs are sorted by that choice; `multiset_diff_by` is the form
/// verified callers can use.
pub fn multiset_diff<'a, T, F>(a: &'a [T], b: &'a [T], cmp: F) -> (r: impl Iterator<Item = Delta<'a, T>>)
    where
        F: Fn(&T, &T) -> Ordering,
    requires
        exists|ord: spec_fn(T, T) -> Ordering| total_order(ord) && #[trigger] computes(cmp, ord),
    ensures
        IteratorSpec::obeys_prophetic_iter_laws(&r),
        IteratorSpec::remaining(&r) == diff_from(
            choose|ord: spec_fn(T, T) -> Ordering| total_order(ord) && #[trigger] computes(cmp, ord),
            a,
            b,
            0,
            0,
        ),
{
    debug_assert_sorted(a, b, &cmp);
    let ghost ord = choose|ord: spec_fn(T, T) -> Ordering| total_order(ord) && #[trigger] computes(cmp, ord);
    MultisetDiff { a, b, cmp, ord: Ghost(ord), i: 0, j: 0 }
}

impl<'a, T, F: Fn(&T, &T) -> Ordering> Iterator for MultisetDiff<'a, T, F> {
    type Item = Delta<'a, T>;

    fn next(&mut self) -> Option<Delta<'a, T>> {
        proof {
            use_type_invariant(&*self);
        }
        let a = self.a;
        let b = self.b;
        let mut i = self.i;
        let mut j = self.j;
        while i < a.len() || j < b.len()
            invariant
                *self == *old(self),
                a == self.a,
                b == self.b,
                self.i <= i <= a@.len(),
                self.j <= j <= b@.len(),
                total_order(self.ord@),
                computes(self.cmp, self.ord@),
                diff_from(self.ord@, a, b, i as int, j as int) == self.rest(),
            decreases a@.len() - i + b@.len() - j,
        {
            proof {
                lemma_step(self.ord@, a, b, i as int, j as int);
            }
            let from_a = if j >= b.len() {
                true
            } else if i >= a.len() {
                false
            } else {
                !matches!((self.cmp)(&a[i], &b[j]), Ordering::Greater)
            };
            let row: &'a T = if from_a { &a[i] } else { &b[j] };
            let mut ia = if from_a { i + 1 } else { i };
            let mut jb = if from_a { j } else { j + 1 };
            proof {
                lemma_run_end(self.ord@, a@, ia as int, *row);
                lemma_run_end(self.ord@, b@, jb as int, *row);
            }
            // Where the runs end, as the spec merge computes them.
            let ghost (_, ia_end, jb_end) = step(self.ord@, a, b, i as int, j as int);
            while ia < a.len() && matches!((self.cmp)(&a[ia], row), Ordering::Equal)
                invariant
                    a == self.a,
                    total_order(self.ord@),
                    computes(self.cmp, self.ord@),
                    ia <= a@.len(),
                    run_end(self.ord@, a@, ia as int, *row) == ia_end,
                decreases a@.len() - ia,
            {
                ia += 1;
            }
            while jb < b.len() && matches!((self.cmp)(&b[jb], row), Ordering::Equal)
                invariant
                    b == self.b,
                    total_order(self.ord@),
                    computes(self.cmp, self.ord@),
                    jb <= b@.len(),
                    run_end(self.ord@, b@, jb as int, *row) == jb_end,
                decreases b@.len() - jb,
            {
                jb += 1;
            }
            let count_a = ia - i;
            let count_b = jb - j;
            i = ia;
            j = jb;
            if count_a != count_b {
                let ghost old_rest = self.rest();
                self.i = i;
                self.j = j;
                assert(self.rest() =~= old_rest.drop_first());
                assert(Seq::new(self.rest().len(), |_k: int| true) =~= Seq::new(
                    old_rest.len(),
                    |_k: int| true,
                ).drop_first());
                return Some(Delta { row, count_a, count_b });
            }
        }
        self.i = i;
        self.j = j;
        None
    }
}

#[cfg(verus_keep_ghost)]
impl<'a, T, F: Fn(&T, &T) -> Ordering> IteratorSpecImpl for MultisetDiff<'a, T, F> {
    open spec fn obeys_prophetic_iter_laws(&self) -> bool {
        true
    }

    closed spec fn remaining(&self) -> Seq<Self::Item> {
        self.rest()
    }

    closed spec fn produces(&self) -> Seq<bool> {
        Seq::new(self.rest().len(), |_k: int| true)
    }

    closed spec fn decrease(&self) -> Option<nat> {
        Some((self.a@.len() - self.i + self.b@.len() - self.j) as nat)
    }

    open spec fn peek(&self, index: int) -> Option<Self::Item> {
        None
    }
}

// ---------------------------------------------------------------------------
// The sortedness check, in debug builds. Verus does not model `is_sorted_by`; this checks
// nothing the proof relies on, and assumes nothing.

#[verifier::external_body]
fn debug_assert_sorted<T, F: Fn(&T, &T) -> Ordering>(a: &[T], b: &[T], cmp: &F) {
    debug_assert!(a.is_sorted_by(|x, y| cmp(x, y) != Ordering::Greater));
    debug_assert!(b.is_sorted_by(|x, y| cmp(x, y) != Ordering::Greater));
}

} // verus!

#[cfg(test)]
mod tests {
    use super::multiset_diff;

    #[test]
    fn fixed_examples() {
        let a = [1u8, 1, 2];
        let b = [1u8, 2, 2, 3];
        let got: Vec<_> = multiset_diff(&a, &b, Ord::cmp)
            .map(|d| (*d.row, d.count_a, d.count_b))
            .collect();
        assert_eq!(got, [(1, 2, 1), (2, 1, 2), (3, 0, 1)]);
    }
}
