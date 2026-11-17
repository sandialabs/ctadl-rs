//! Verus proof of `multiset_diff`'s merge, for inputs of any length.
//!
//! This is a copy of the crate's merge (`../src/lib.rs`) with its proof: the crate cannot
//! depend on `vstd`, so Verus checks this file, and `same_code.py` checks that `next` here
//! is `next` there, less what Verus adds. Run it with
//! `verus --crate-type=lib --edition=2024 multiset_diff.rs`, or `nix build .#checks.<system>.verus`.
//!
//! What is proved:
//!
//! - `next` yields exactly `diff_from(ord, a, b, 0, 0)`, a spec-level statement of the merge,
//!   for any `T` and any `cmp` that computes a total order `ord` (`multiset_diff_by`).
//! - Of `diff_from`, the four properties the crate promises: `theorem_self_diff_is_empty`,
//!   `theorem_swap_swaps_sides`, `theorem_reports_exactly_count_differences` and
//!   `theorem_no_value_twice`. Values are the same when `ord` calls them `Equal`; counts
//!   count those.
//! - `diff` and `swap_swaps_sides` at the end restate the properties with `==` and plain
//!   counts, for `u8` under `Ord::cmp`, over what the iterator yields.
use std::cmp::Ordering;
use vstd::prelude::*;
use vstd::std_specs::iter::{IteratorSpec, IteratorSpecImpl};

verus! {

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
pub struct MultisetDiff<'a, T, F: Fn(&T, &T) -> Ordering> {
    a: &'a [T],
    b: &'a [T],
    cmp: F,
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
pub fn multiset_diff_by<'a, T, F>(
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
/// The signature of the crate's `multiset_diff`. Its postcondition can only name an order
/// `cmp` computes by choosing one, and no caller can show its inputs are sorted by that
/// choice; `multiset_diff_by` is the form callers can use.
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
// Consequences of the order laws.

proof fn lemma_refl<T>(ord: spec_fn(T, T) -> Ordering, x: T)
    requires
        total_order(ord),
    ensures
        eqv(ord, x, x),
{
    assert(ord(x, x) == Ordering::Less <==> ord(x, x) == Ordering::Greater);
}

proof fn lemma_eqv_symm<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T)
    requires
        total_order(ord),
        eqv(ord, x, y),
    ensures
        eqv(ord, y, x),
{
    assert(ord(x, y) == Ordering::Equal);
}

/// `le` and `lt` chain, and `eqv` is transitive.
proof fn lemma_trans<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T, z: T)
    requires
        total_order(ord),
    ensures
        le(ord, x, y) && le(ord, y, z) ==> le(ord, x, z),
        lt(ord, x, y) && le(ord, y, z) ==> lt(ord, x, z),
        le(ord, x, y) && lt(ord, y, z) ==> lt(ord, x, z),
        eqv(ord, x, y) && eqv(ord, y, z) ==> eqv(ord, x, z),
{
    // Each conclusion: `le(x, z)` by transitivity; the converse `le(z, x)` would give
    // `le(z, y)` or `le(y, x)` and so contradict a strict step, or establish `eqv`.
    assert(le(ord, x, y) && le(ord, y, z) ==> le(ord, x, z));
    assert(le(ord, z, x) && le(ord, x, y) ==> le(ord, z, y));
    assert(le(ord, y, z) && le(ord, z, x) ==> le(ord, y, x));
    assert(ord(x, y) == Ordering::Less <==> ord(y, x) == Ordering::Greater);
    assert(ord(y, z) == Ordering::Less <==> ord(z, y) == Ordering::Greater);
    assert(ord(x, z) == Ordering::Less <==> ord(z, x) == Ordering::Greater);
    assert(ord(z, x) == Ordering::Less <==> ord(x, z) == Ordering::Greater);
    assert(ord(y, x) == Ordering::Less <==> ord(x, y) == Ordering::Greater);
    assert(ord(z, y) == Ordering::Less <==> ord(y, z) == Ordering::Greater);
    if eqv(ord, x, y) && eqv(ord, y, z) {
        lemma_eqv_symm(ord, x, y);
        lemma_eqv_symm(ord, y, z);
        assert(le(ord, x, y) && le(ord, y, z));
        assert(le(ord, x, z));
        assert(le(ord, z, y) && le(ord, y, x));
        assert(le(ord, z, x));
        assert(ord(x, z) != Ordering::Less);
    }
}

/// `run_end` depends on `v` only up to `eqv`.
proof fn lemma_run_end_congr<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, i: int, v: T, w: T)
    requires
        total_order(ord),
        eqv(ord, v, w),
    ensures
        run_end(ord, s, i, v) == run_end(ord, s, i, w),
    decreases s.len() - i,
{
    if 0 <= i < s.len() {
        lemma_eqv_symm(ord, v, w);
        lemma_trans(ord, s[i], v, w);
        lemma_trans(ord, s[i], w, v);
        lemma_run_end_congr(ord, s, i + 1, v, w);
    }
}

// ---------------------------------------------------------------------------
// Property 1: diffing a sequence with itself yields nothing.

proof fn lemma_self_diff<'a, T>(ord: spec_fn(T, T) -> Ordering, a: &'a [T], i: int)
    requires
        total_order(ord),
        0 <= i <= a@.len(),
    ensures
        diff_from(ord, a, a, i, i) == Seq::<Delta<'a, T>>::empty(),
    decreases a@.len() - i,
{
    if i < a@.len() {
        lemma_refl(ord, a@[i]);
        assert(head_from_a(ord, a@, a@, i, i));
        assert(run_end(ord, a@, i, a@[i]) == run_end(ord, a@, i + 1, a@[i]));
        lemma_step(ord, a, a, i, i);
        let (_, ia, _) = step(ord, a, a, i, i);
        lemma_self_diff(ord, a, ia);
    }
}

pub proof fn theorem_self_diff_is_empty<'a, T>(ord: spec_fn(T, T) -> Ordering, a: &'a [T])
    requires
        total_order(ord),
    ensures
        diff_from(ord, a, a, 0, 0) == Seq::<Delta<'a, T>>::empty(),
{
    lemma_self_diff(ord, a, 0);
}

// ---------------------------------------------------------------------------
// Property 2: swapping the inputs yields the same values, in the same order, with the
// counts exchanged.

pub open spec fn swapped<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    d: Seq<Delta<'a, T>>,
    e: Seq<Delta<'a, T>>,
) -> bool {
    &&& d.len() == e.len()
    &&& forall|k: int|
        0 <= k < d.len() ==> {
            &&& eqv(ord, *(#[trigger] d[k]).row, *e[k].row)
            &&& d[k].count_a == e[k].count_b
            &&& d[k].count_b == e[k].count_a
        }
}

proof fn lemma_swap<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
    ensures
        swapped(ord, diff_from(ord, a, b, i, j), diff_from(ord, b, a, j, i)),
    decreases a@.len() - i + b@.len() - j,
{
    if i == a@.len() && j == b@.len() {
        return;
    }
    lemma_step(ord, a, b, i, j);
    lemma_step(ord, b, a, j, i);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let (s, jb2, ia2) = step(ord, b, a, j, i);
    let ab = head_from_a(ord, a@, b@, i, j);
    let ba = head_from_a(ord, b@, a@, j, i);
    if ab && ba {
        // A tie: `a[i]` and `b[j]` are equivalent, and each side's run is the same whichever
        // of them it is measured against.
        assert(ord(a@[i], b@[j]) == Ordering::Less <==> ord(b@[j], a@[i])
            == Ordering::Greater);
        assert(ord(b@[j], a@[i]) == Ordering::Less <==> ord(a@[i], b@[j])
            == Ordering::Greater);
        assert(eqv(ord, a@[i], b@[j]));
        lemma_refl(ord, b@[j]);
        lemma_trans(ord, a@[i], b@[j], b@[j]);
        lemma_run_end_congr(ord, a@, i + 1, a@[i], b@[j]);
        lemma_run_end_congr(ord, b@, j, a@[i], b@[j]);
        lemma_refl(ord, a@[i]);
        lemma_eqv_symm(ord, a@[i], b@[j]);
        assert(run_end(ord, a@, i, b@[j]) == run_end(ord, a@, i + 1, b@[j]));
        assert(run_end(ord, b@, j, a@[i]) == run_end(ord, b@, j + 1, a@[i]));
        lemma_run_end_congr(ord, b@, j + 1, a@[i], b@[j]);
        assert(ia == ia2 && jb == jb2);
        assert(eqv(ord, *r, *s));
    } else if !ab && !ba {
        assert(ord(a@[i], b@[j]) == Ordering::Less <==> ord(b@[j], a@[i])
            == Ordering::Greater);
        assert(false);
    } else {
        // The same element comes first either way.
        lemma_refl(ord, *r);
        assert(ia == ia2 && jb == jb2 && r == s);
    }
    lemma_swap(ord, a, b, ia, jb);
    let d = diff_from(ord, a, b, i, j);
    let e = diff_from(ord, b, a, j, i);
    let d2 = diff_from(ord, a, b, ia, jb);
    let e2 = diff_from(ord, b, a, jb, ia);
    if ia - i != jb - j {
        assert(d == seq![Delta { row: r, count_a: (ia - i) as usize, count_b: (jb - j) as usize }] + d2);
        assert(e == seq![Delta { row: s, count_a: (jb - j) as usize, count_b: (ia - i) as usize }] + e2);
        assert forall|k: int| 0 <= k < d.len() implies {
            &&& eqv(ord, *(#[trigger] d[k]).row, *e[k].row)
            &&& d[k].count_a == e[k].count_b
            &&& d[k].count_b == e[k].count_a
        } by {
            if k > 0 {
                assert(d[k] == d2[k - 1]);
                assert(e[k] == e2[k - 1]);
            }
        }
    } else {
        assert(d == d2);
        assert(e == e2);
    }
}

pub proof fn theorem_swap_swaps_sides<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
    ensures
        swapped(ord, diff_from(ord, a, b, 0, 0), diff_from(ord, b, a, 0, 0)),
{
    lemma_swap(ord, a, b, 0, 0);
}

// ---------------------------------------------------------------------------
// Counting.

proof fn lemma_count_split<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    mid: int,
    hi: int,
    v: T,
)
    requires
        lo <= mid <= hi,
    ensures
        count(ord, s, lo, hi, v) == count(ord, s, lo, mid, v) + count(ord, s, mid, hi, v),
    decreases hi - mid,
{
    if mid < hi {
        lemma_count_split(ord, s, lo, mid, hi - 1, v);
    }
}

proof fn lemma_count_none<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
)
    requires
        forall|k: int| lo <= k < hi ==> !eqv(ord, #[trigger] s[k], v),
    ensures
        count(ord, s, lo, hi, v) == 0,
    decreases hi - lo,
{
    if lo < hi {
        lemma_count_none(ord, s, lo, hi - 1, v);
    }
}

proof fn lemma_count_all<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
)
    requires
        lo <= hi,
        forall|k: int| lo <= k < hi ==> eqv(ord, #[trigger] s[k], v),
    ensures
        count(ord, s, lo, hi, v) == hi - lo,
    decreases hi - lo,
{
    if lo < hi {
        lemma_count_all(ord, s, lo, hi - 1, v);
    }
}

proof fn lemma_count_congr<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
    w: T,
)
    requires
        total_order(ord),
        eqv(ord, v, w),
    ensures
        count(ord, s, lo, hi, v) == count(ord, s, lo, hi, w),
    decreases hi - lo,
{
    if lo < hi {
        lemma_eqv_symm(ord, v, w);
        lemma_trans(ord, s[hi - 1], v, w);
        lemma_trans(ord, s[hi - 1], w, v);
        lemma_count_congr(ord, s, lo, hi - 1, v, w);
    }
}

// ---------------------------------------------------------------------------
// Properties 3 and 4, on sorted inputs.

/// `x` is above every element of `a[..i]` and `b[..j]`.
pub open spec fn above<T>(
    ord: spec_fn(T, T) -> Ordering,
    a: Seq<T>,
    b: Seq<T>,
    i: int,
    j: int,
    x: T,
) -> bool {
    &&& forall|p: int| 0 <= p < i ==> #[trigger] lt(ord, a[p], x)
    &&& forall|q: int| 0 <= q < j ==> #[trigger] lt(ord, b[q], x)
}

/// Positions `i` and `j` split both inputs: everything before them is below everything after.
pub open spec fn cut<T>(
    ord: spec_fn(T, T) -> Ordering,
    a: Seq<T>,
    b: Seq<T>,
    i: int,
    j: int,
) -> bool {
    &&& forall|p: int| i <= p < a.len() ==> #[trigger] above(ord, a, b, i, j, a[p])
    &&& forall|q: int| j <= q < b.len() ==> #[trigger] above(ord, a, b, i, j, b[q])
}

/// What the merge's step from a cut establishes, on sorted inputs: the value it looks at is
/// above what came before, its runs hold exactly its equivalents, everything after them is
/// above it, and the ends of the runs are a cut again.
proof fn lemma_step_sorted<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
        i < a@.len() || j < b@.len(),
        cut(ord, a@, b@, i, j),
    ensures
        ({
            let (r, ia, jb) = step(ord, a, b, i, j);
            &&& above(ord, a@, b@, i, j, *r)
            &&& forall|p: int| i <= p < ia ==> #[trigger] eqv(ord, a@[p], *r)
            &&& forall|q: int| j <= q < jb ==> #[trigger] eqv(ord, b@[q], *r)
            &&& forall|p: int| ia <= p < a@.len() ==> #[trigger] lt(ord, *r, a@[p])
            &&& forall|q: int| jb <= q < b@.len() ==> #[trigger] lt(ord, *r, b@[q])
            &&& count_all(ord, a@, *r) == ia - i
            &&& count_all(ord, b@, *r) == jb - j
            &&& cut(ord, a@, b@, ia, jb)
        }),
{
    lemma_step(ord, a, b, i, j);
    let from_a = head_from_a(ord, a@, b@, i, j);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let v = *r;
    lemma_refl(ord, v);
    if from_a {
        assert(above(ord, a@, b@, i, j, a@[i]));
        lemma_run_end(ord, a@, i + 1, v);
        lemma_run_end(ord, b@, j, v);
    } else {
        assert(above(ord, a@, b@, i, j, b@[j]));
        lemma_run_end(ord, a@, i, v);
        lemma_run_end(ord, b@, j + 1, v);
    }
    // Everything at or past the run ends is above `v`.
    if ia < a@.len() {
        if from_a {
            assert(le(ord, a@[i], a@[ia]));
        } else {
            // `b[j]` came first, so it is below `a[i]`.
            assert(ord(b@[j], a@[i]) == Ordering::Less <==> ord(a@[i], b@[j])
                == Ordering::Greater);
            if i < ia {
                assert(le(ord, a@[i], a@[ia]));
            }
            lemma_trans(ord, v, a@[i], a@[ia]);
        }
        assert(!eqv(ord, a@[ia], v));
        assert(ord(v, a@[ia]) == Ordering::Equal ==> ord(a@[ia], v) == Ordering::Equal);
        assert(lt(ord, v, a@[ia]));
        assert forall|p: int| ia <= p < a@.len() implies #[trigger] lt(ord, v, a@[p]) by {
            if ia < p {
                assert(le(ord, a@[ia], a@[p]));
            } else {
                lemma_refl(ord, a@[p]);
            }
            lemma_trans(ord, v, a@[ia], a@[p]);
        }
    }
    if jb < b@.len() {
        if !from_a {
            assert(le(ord, b@[j], b@[jb]));
        } else {
            if j < b@.len() {
                if j < jb {
                    assert(le(ord, b@[j], b@[jb]));
                }
                lemma_trans(ord, v, b@[j], b@[jb]);
            }
        }
        assert(!eqv(ord, b@[jb], v));
        assert(ord(v, b@[jb]) == Ordering::Equal ==> ord(b@[jb], v) == Ordering::Equal);
        assert(lt(ord, v, b@[jb]));
        assert forall|q: int| jb <= q < b@.len() implies #[trigger] lt(ord, v, b@[q]) by {
            if jb < q {
                assert(le(ord, b@[jb], b@[q]));
            } else {
                lemma_refl(ord, b@[q]);
            }
            lemma_trans(ord, v, b@[jb], b@[q]);
        }
    }
    // The counts: nothing before the cut or past the runs is equivalent to `v`.
    assert forall|p: int| 0 <= p < i implies !eqv(ord, #[trigger] a@[p], v) by {
        assert(lt(ord, a@[p], v));
    }
    assert forall|p: int| ia <= p < a@.len() implies !eqv(ord, #[trigger] a@[p], v) by {
        assert(lt(ord, v, a@[p]));
        assert(ord(v, a@[p]) == Ordering::Less <==> ord(a@[p], v) == Ordering::Greater);
    }
    assert forall|q: int| 0 <= q < j implies !eqv(ord, #[trigger] b@[q], v) by {
        assert(lt(ord, b@[q], v));
    }
    assert forall|q: int| jb <= q < b@.len() implies !eqv(ord, #[trigger] b@[q], v) by {
        assert(lt(ord, v, b@[q]));
        assert(ord(v, b@[q]) == Ordering::Less <==> ord(b@[q], v) == Ordering::Greater);
    }
    lemma_count_split(ord, a@, 0, i, a@.len() as int, v);
    lemma_count_split(ord, a@, i, ia, a@.len() as int, v);
    lemma_count_none(ord, a@, 0, i, v);
    lemma_count_all(ord, a@, i, ia, v);
    lemma_count_none(ord, a@, ia, a@.len() as int, v);
    lemma_count_split(ord, b@, 0, j, b@.len() as int, v);
    lemma_count_split(ord, b@, j, jb, b@.len() as int, v);
    lemma_count_none(ord, b@, 0, j, v);
    lemma_count_all(ord, b@, j, jb, v);
    lemma_count_none(ord, b@, jb, b@.len() as int, v);
    // The run ends are a cut: the runs are equivalent to `v`, which is below what follows.
    assert forall|x: T| above(ord, a@, b@, i, j, x) && lt(ord, v, x) implies #[trigger] above(
        ord,
        a@,
        b@,
        ia,
        jb,
        x,
    ) by {
        assert forall|p: int| 0 <= p < ia implies #[trigger] lt(ord, a@[p], x) by {
            if p >= i {
                lemma_trans(ord, a@[p], v, x);
            }
        }
        assert forall|q: int| 0 <= q < jb implies #[trigger] lt(ord, b@[q], x) by {
            if q >= j {
                lemma_trans(ord, b@[q], v, x);
            }
        }
    }
    assert forall|p: int| ia <= p < a@.len() implies #[trigger] above(ord, a@, b@, ia, jb, a@[p]) by {
        assert(above(ord, a@, b@, i, j, a@[p]));
        assert(lt(ord, v, a@[p]));
    }
    assert forall|q: int| jb <= q < b@.len() implies #[trigger] above(ord, a@, b@, ia, jb, b@[q]) by {
        assert(above(ord, a@, b@, i, j, b@[q]));
        assert(lt(ord, v, b@[q]));
    }
}

/// Properties 3 and 4 from a cut, and that every row is above the cut.
proof fn lemma_sorted<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
        cut(ord, a@, b@, i, j),
    ensures
        ({
            let d = diff_from(ord, a, b, i, j);
            &&& forall|k: int| 0 <= k < d.len() ==> {
                &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
                &&& d[k].count_b == count_all(ord, b@, *d[k].row)
                &&& d[k].count_a != d[k].count_b
                &&& above(ord, a@, b@, i, j, *d[k].row)
            }
            &&& forall|k1: int, k2: int|
                0 <= k1 < k2 < d.len() ==> lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row)
            &&& forall|v: T|
                #![trigger count_all(ord, a@, v)]
                #![trigger count_all(ord, b@, v)]
                count_all(ord, a@, v) != count_all(ord, b@, v)
                && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], v))
                && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], v))
                ==> exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v)
        }),
    decreases a@.len() - i + b@.len() - j,
{
    let d = diff_from(ord, a, b, i, j);
    if i == a@.len() && j == b@.len() {
        assert forall|v: T|
            count_all(ord, a@, v) != count_all(ord, b@, v)
            && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], v))
            && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], v))
            implies exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v) by {
            lemma_count_none(ord, a@, 0, i, v);
            lemma_count_none(ord, b@, 0, j, v);
        }
        return;
    }
    lemma_step(ord, a, b, i, j);
    lemma_step_sorted(ord, a, b, i, j);
    let from_a = head_from_a(ord, a@, b@, i, j);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let v = *r;
    lemma_sorted(ord, a, b, ia, jb);
    let rest = diff_from(ord, a, b, ia, jb);
    // Rows of the rest are above the runs, so above `v` and above the old cut.
    assert forall|k: int| 0 <= k < rest.len() implies lt(ord, v, *(#[trigger] rest[k]).row)
        && above(ord, a@, b@, i, j, *rest[k].row) by {
        assert(above(ord, a@, b@, ia, jb, *rest[k].row));
        if from_a {
            assert(lt(ord, a@[i], *rest[k].row));
        } else {
            assert(lt(ord, b@[j], *rest[k].row));
        }
    }
    // Completeness: a value with differing counts is `v`'s equivalent, or lies past the runs.
    assert forall|w: T|
        count_all(ord, a@, w) != count_all(ord, b@, w)
        && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], w))
        && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], w))
        implies exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, w) by {
        if eqv(ord, v, w) {
            lemma_count_congr(ord, a@, 0, a@.len() as int, v, w);
            lemma_count_congr(ord, b@, 0, b@.len() as int, v, w);
            assert(d[0].row == r);
        } else {
            assert forall|p: int| 0 <= p < ia implies !eqv(ord, #[trigger] a@[p], w) by {
                if p >= i {
                    assert(eqv(ord, a@[p], v));
                    lemma_eqv_symm(ord, a@[p], v);
                    lemma_trans(ord, v, a@[p], w);
                }
            }
            assert forall|q: int| 0 <= q < jb implies !eqv(ord, #[trigger] b@[q], w) by {
                if q >= j {
                    assert(eqv(ord, b@[q], v));
                    lemma_eqv_symm(ord, b@[q], v);
                    lemma_trans(ord, v, b@[q], w);
                }
            }
            let k = choose|k: int| 0 <= k < rest.len() && eqv(ord, *(#[trigger] rest[k]).row, w);
            if ia - i != jb - j {
                assert(d[k + 1] == rest[k]);
            } else {
                assert(d[k] == rest[k]);
            }
        }
    }
    if ia - i != jb - j {
        assert(d == seq![Delta { row: r, count_a: (ia - i) as usize, count_b: (jb - j) as usize }] + rest);
        assert forall|k: int| 0 <= k < d.len() implies {
            &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
            &&& d[k].count_b == count_all(ord, b@, *d[k].row)
            &&& d[k].count_a != d[k].count_b
            &&& above(ord, a@, b@, i, j, *d[k].row)
        } by {
            if k > 0 {
                assert(d[k] == rest[k - 1]);
            } else {
                assert(d[0].row == r);
            }
        }
        assert forall|k1: int, k2: int|
            0 <= k1 < k2 < d.len() implies lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row) by {
            assert(d[k2] == rest[k2 - 1]);
            if k1 > 0 {
                assert(d[k1] == rest[k1 - 1]);
            }
        }
    } else {
        assert(d == rest);
    }
}

pub proof fn theorem_reports_exactly_count_differences<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        ({
            let d = diff_from(ord, a, b, 0, 0);
            &&& forall|k: int| 0 <= k < d.len() ==> {
                &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
                &&& d[k].count_b == count_all(ord, b@, *d[k].row)
                &&& d[k].count_a != d[k].count_b
            }
            &&& forall|v: T|
                #![trigger count_all(ord, a@, v)]
                #![trigger count_all(ord, b@, v)]
                count_all(ord, a@, v) != count_all(ord, b@, v)
                ==> exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v)
        }),
{
    lemma_sorted(ord, a, b, 0, 0);
}

pub proof fn theorem_no_value_twice<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        ({
            let d = diff_from(ord, a, b, 0, 0);
            forall|k1: int, k2: int|
                0 <= k1 < k2 < d.len() ==> lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row)
        }),
{
    lemma_sorted(ord, a, b, 0, 0);
}

// ---------------------------------------------------------------------------
// A client: `u8` under its usual order, with `cmp` a closure that calls `Ord::cmp`. Here the
// properties are stated over what the iterator yields, with equality for equivalence and
// `count_u8` for counts.

pub open spec fn u8_ord(x: u8, y: u8) -> Ordering {
    if x < y {
        Ordering::Less
    } else if x > y {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// The number of elements of `s` equal to `v`, counted without the merge.
pub open spec fn count_u8(s: Seq<u8>, v: u8) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        count_u8(s.drop_last(), v) + if s.last() == v {
            1nat
        } else {
            0nat
        }
    }
}

pub open spec fn sorted_u8(s: Seq<u8>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> s[p] <= s[q]
}

proof fn lemma_u8_count(s: Seq<u8>, v: u8)
    ensures
        count_all(u8_spec_ord(), s, v) == count_u8(s, v),
    decreases s.len(),
{
    let ord = u8_spec_ord();
    if s.len() > 0 {
        let t = s.drop_last();
        lemma_u8_count(t, v);
        // `count` over `s[0..n-1]` is `count` over `t`.
        lemma_count_prefix(ord, s, t, v, t.len() as int);
    }
}

/// `count` over a prefix only looks at that prefix.
proof fn lemma_count_prefix<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>, v: T, hi: int)
    requires
        0 <= hi <= t.len(),
        t.len() <= s.len(),
        forall|k: int| 0 <= k < t.len() ==> s[k] == t[k],
    ensures
        count(ord, s, 0, hi, v) == count(ord, t, 0, hi, v),
    decreases hi,
{
    if hi > 0 {
        lemma_count_prefix(ord, s, t, v, hi - 1);
    }
}

pub open spec fn u8_spec_ord() -> spec_fn(u8, u8) -> Ordering {
    |x: u8, y: u8| u8_ord(x, y)
}

/// The four properties of `out`, the deltas of `a` and `b`.
pub open spec fn client_properties(a: Seq<u8>, b: Seq<u8>, out: Seq<(u8, usize, usize)>) -> bool {
    // reports_exactly_count_differences
    &&& forall|k: int|
        0 <= k < out.len() ==> {
            &&& (#[trigger] out[k]).1 == count_u8(a, out[k].0)
            &&& out[k].2 == count_u8(b, out[k].0)
            &&& out[k].1 != out[k].2
        }
    &&& forall|v: u8|
        count_u8(a, v) != count_u8(b, v) ==> exists|k: int|
            0 <= k < out.len() && (#[trigger] out[k]).0 == v
    // no_value_twice
    &&& forall|k1: int, k2: int|
        0 <= k1 < k2 < out.len() ==> (#[trigger] out[k1]).0 < (#[trigger] out[k2]).0
    // self_diff_is_empty
    &&& a == b ==> out.len() == 0
}

proof fn lemma_client_properties<'a>(a: &'a [u8], b: &'a [u8])
    requires
        sorted_u8(a@),
        sorted_u8(b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        client_properties(a@, b@, deltas_u8(diff_from(u8_spec_ord(), a, b, 0, 0))),
{
    let ord = u8_spec_ord();
    let all = diff_from(ord, a, b, 0, 0);
    let out = deltas_u8(all);
    assert(total_order(ord));
    assert(sorted(ord, a@));
    assert(sorted(ord, b@));
    theorem_reports_exactly_count_differences(ord, a, b);
    theorem_no_value_twice(ord, a, b);
    assert forall|v: u8| #[trigger] count_all(ord, a@, v) == count_u8(a@, v) by {
        lemma_u8_count(a@, v);
    }
    assert forall|v: u8| #[trigger] count_all(ord, b@, v) == count_u8(b@, v) by {
        lemma_u8_count(b@, v);
    }
    assert forall|k: int| 0 <= k < out.len() implies {
        &&& (#[trigger] out[k]).1 == count_u8(a@, out[k].0)
        &&& out[k].2 == count_u8(b@, out[k].0)
        &&& out[k].1 != out[k].2
    } by {
        assert(out[k].0 == *all[k].row);
        assert(count_all(ord, a@, *all[k].row) == count_u8(a@, *all[k].row));
        assert(count_all(ord, b@, *all[k].row) == count_u8(b@, *all[k].row));
    }
    // Diffing a sequence with itself: every delta's counts would be equal.
    if a@ == b@ && out.len() > 0 {
        assert(out[0].1 != out[0].2);
    }
    assert forall|v: u8| count_u8(a@, v) != count_u8(b@, v) implies exists|k: int|
        0 <= k < out.len() && (#[trigger] out[k]).0 == v by {
        assert(count_all(ord, a@, v) == count_u8(a@, v));
        assert(count_all(ord, b@, v) == count_u8(b@, v));
        let k = choose|k: int| 0 <= k < all.len() && eqv(ord, *(#[trigger] all[k]).row, v);
        assert(out[k].0 == *all[k].row);
    }
    assert forall|k1: int, k2: int| 0 <= k1 < k2 < out.len() implies (#[trigger] out[k1]).0 < (
    #[trigger] out[k2]).0 by {
        assert(out[k1].0 == *all[k1].row);
        assert(out[k2].0 == *all[k2].row);
        assert(lt(ord, *all[k1].row, *all[k2].row));
    }
}

/// Every delta the core yields for `a` and `b`, in order, as `(value, count_a, count_b)`.
fn diff(a: &[u8], b: &[u8]) -> (out: Vec<(u8, usize, usize)>)
    requires
        sorted_u8(a@),
        sorted_u8(b@),
    ensures
        out@ == deltas_u8(diff_from(u8_spec_ord(), a, b, 0, 0)),
        client_properties(a@, b@, out@),
{
    let ghost ord = u8_spec_ord();
    let cmp = |x: &u8, y: &u8| -> (o: Ordering)
        ensures
            o == u8_ord(*x, *y),
        { x.cmp(y) };
    assert(total_order(ord));
    assert(computes(cmp, ord));
    let _ = (a.len(), b.len());
    let ghost all = diff_from(ord, a, b, 0, 0);
    let mut it = multiset_diff_by(a, b, cmp, Ghost(ord));
    let mut out: Vec<(u8, usize, usize)> = Vec::new();
    loop
        invariant
            it.obeys_prophetic_iter_laws(),
            it.decrease() is Some,
            out.len() <= all.len(),
            it.remaining() == all.skip(out.len() as int),
            out@ == deltas_u8(all.take(out.len() as int)),
        ensures
            out@ == deltas_u8(all),
        decreases it.decrease()->0,
    {
        match it.next() {
            Some(d) => {
                proof {
                    assert(all.skip(out.len() as int).drop_first() =~= all.skip(out.len() + 1));
                    assert(deltas_u8(all.take(out.len() + 1)) =~= deltas_u8(
                        all.take(out.len() as int),
                    ).push((*d.row, d.count_a, d.count_b)));
                }
                out.push((*d.row, d.count_a, d.count_b));
            },
            None => {
                assert(all.skip(out.len() as int).len() == 0);
                assert(out.len() == all.len());
                assert(all.take(out.len() as int) =~= all);
                break ;
            },
        }
    }
    proof {
        lemma_client_properties(a, b);
    }
    out
}

/// What the core yields, as `diff` records it.
pub open spec fn deltas_u8<'a>(d: Seq<Delta<'a, u8>>) -> Seq<(u8, usize, usize)> {
    d.map_values(|e: Delta<'a, u8>| (*e.row, e.count_a, e.count_b))
}

/// Diffing `(b, a)` yields the same values, in the same order, as diffing `(a, b)`, each with
/// its two counts exchanged.
fn swap_swaps_sides(a: &[u8], b: &[u8])
    requires
        sorted_u8(a@),
        sorted_u8(b@),
{
    let ab = diff(a, b);
    let ba = diff(b, a);
    proof {
        let ord = u8_spec_ord();
        assert(total_order(ord));
        theorem_swap_swaps_sides(ord, a, b);
        let d = diff_from(ord, a, b, 0, 0);
        let e = diff_from(ord, b, a, 0, 0);
        assert forall|k: int| 0 <= k < d.len() implies *(#[trigger] d[k]).row == *e[k].row by {
            assert(eqv(ord, *d[k].row, *e[k].row));
        }
    }
    assert(ab.len() == ba.len());
    assert(forall|k: int|
        0 <= k < ab.len() ==> {
            &&& (#[trigger] ab[k]).0 == ba[k].0
            &&& ab[k].1 == ba[k].2
            &&& ab[k].2 == ba[k].1
        });
}

} // verus!
