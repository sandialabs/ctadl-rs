//! The primitives the canonical form is built from: resolving a reference, sorting a bag,
//! mapping a sequence, a flow's endpoints and the `executionOrder` check.
//!
//! Each is a `spec fn` that says what it means, an exec function proved to compute it, and
//! lemmas about the spec: a renumbered table resolves alike, a bag is sorted and loses nothing,
//! a sequence keeps its order, and endpoints see only the first and last steps.

use crate::types::*;
use std::cmp::Ordering;
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use crate::order::{by_view, equal_means_eq};
#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, le, sorted, total_order};

verus! {

// ---------------------------------------------------------------------------
// What each primitive means.

/// Entry `i` of a table, or `OutOfRange`.
pub open spec fn spec_resolve<T>(t: Seq<T>, i: int) -> Result<T, Error> {
    if 0 <= i < t.len() {
        Ok(t[i])
    } else {
        Err(Error::OutOfRange)
    }
}

/// `pi` maps `0..n` one to one onto `0..n`.
pub open spec fn is_permutation(pi: Seq<int>, n: nat) -> bool {
    &&& pi.len() == n
    &&& forall|i: int| 0 <= i < n ==> 0 <= #[trigger] pi[i] < n
    &&& forall|i: int, j: int| 0 <= i < j < n ==> #[trigger] pi[i] != #[trigger] pi[j]
}

/// `u` is `t` renumbered by `pi`: `t`'s entry `i` is `u`'s entry `pi[i]` (`u` is `t∘pi⁻¹`).
pub open spec fn renumbered_table<T>(t: Seq<T>, u: Seq<T>, pi: Seq<int>) -> bool {
    &&& is_permutation(pi, t.len())
    &&& u.len() == t.len()
    &&& forall|i: int| 0 <= i < t.len() ==> u[#[trigger] pi[i]] == t[i]
}

/// `x` inserted into `s` before the first element it is not above.
pub open spec fn insert_sorted<T>(ord: spec_fn(T, T) -> Ordering, x: T, s: Seq<T>) -> Seq<T>
    decreases s.len(),
{
    if s.len() == 0 {
        seq![x]
    } else if le(ord, x, s[0]) {
        seq![x] + s
    } else {
        seq![s[0]] + insert_sorted(ord, x, s.drop_first())
    }
}

/// A bag's canonical form: its elements sorted by `ord` (insertion sort), duplicates kept.
pub open spec fn spec_canon_bag<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>) -> Seq<T>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        insert_sorted(ord, s[0], spec_canon_bag(ord, s.drop_first()))
    }
}

/// A sequence's canonical form: `f` of each element, in order.
pub open spec fn spec_canon_seq<X, V>(xs: Seq<X>, f: spec_fn(X) -> V) -> Seq<V> {
    xs.map_values(f)
}

/// The values of `s` if every element is `Ok`; else the first error.
pub open spec fn spec_all_ok<V>(s: Seq<Result<V, Error>>) -> Result<Seq<V>, Error>
    decreases s.len(),
{
    if s.len() == 0 {
        Ok(Seq::empty())
    } else {
        match s[0] {
            Err(e) => Err(e),
            Ok(v) => match spec_all_ok(s.drop_first()) {
                Err(e) => Err(e),
                Ok(rest) => Ok(seq![v] + rest),
            },
        }
    }
}

/// The first and last elements, or `EmptyFlow`. One element is both.
pub open spec fn spec_endpoints<T>(s: Seq<T>) -> Result<(T, T), Error> {
    if s.len() == 0 {
        Err(Error::EmptyFlow)
    } else {
        Ok((s[0], s.last()))
    }
}

/// The `executionOrder` values present increase strictly along the steps. Absent values
/// constrain nothing: the order compared is the array's.
pub open spec fn spec_increasing(orders: Seq<Option<i64>>) -> bool {
    forall|i: int, j: int|
        0 <= i < j < orders.len() && (#[trigger] orders[i]) is Some && (#[trigger] orders[j]) is Some
            ==> orders[i]->Some_0 < orders[j]->Some_0
}

/// A step's `executionOrder`.
pub open spec fn step_order<A>(s: RawStepV<A>) -> Option<i64> {
    s.order
}

/// A result of the code, as its view.
pub open spec fn lift<C, V>(view: spec_fn(C) -> V, r: Result<C, Error>) -> Result<V, Error> {
    match r {
        Ok(c) => Ok(view(c)),
        Err(e) => Err(e),
    }
}

/// A result of the code holding a `Vec`, as the `Seq` of its elements' views.
pub open spec fn lift_vec<C, V>(view: spec_fn(C) -> V, r: Result<Vec<C>, Error>) -> Result<
    Seq<V>,
    Error,
> {
    match r {
        Ok(v) => Ok(v@.map_values(view)),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// The code.

/// Entry `i` of `table`.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn resolve<T>(table: &[T], i: usize) -> (r: Result<&T, Error>)
    ensures
        match r {
            Ok(x) => spec_resolve(table@, i as int) == Ok::<T, Error>(*x),
            Err(e) => spec_resolve(table@, i as int) == Err::<T, Error>(e),
        },
{
    unimplemented!()
}

/// `f` of each element of `xs`, in order, or the first error `f` returns. `f` computes `sf` on
/// views.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn canon_seq<X, C, V, F: Fn(&X) -> Result<C, Error>>(
    xs: &[X],
    f: &F,
    Ghost(view): Ghost<spec_fn(C) -> V>,
    Ghost(sf): Ghost<spec_fn(X) -> Result<V, Error>>,
) -> (r: Result<Vec<C>, Error>)
    requires
        forall|x: &X| #[trigger] f.requires((x,)),
        forall|x: &X, c: Result<C, Error>| #[trigger] f.ensures((x,), c) ==> lift(view, c) == sf(*x),
    ensures
        lift_vec(view, r) == spec_all_ok(spec_canon_seq(xs@, sf)),
{
    unimplemented!()
}

/// `xs` as a bag: sorted by the order `cmp` computes on views, duplicates kept.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn canon_bag<C, V, F: Fn(&C, &C) -> Ordering>(
    xs: Vec<C>,
    cmp: &F,
    Ghost(view): Ghost<spec_fn(C) -> V>,
    Ghost(ord): Ghost<spec_fn(V, V) -> Ordering>,
) -> (r: Vec<C>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        computes(*cmp, by_view(view, ord)),
    ensures
        r@.map_values(view) == spec_canon_bag(ord, xs@.map_values(view)),
        sorted(by_view(view, ord), r@),
{
    unimplemented!()
}

/// A thread flow's first and last steps.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn endpoints<A: Copy>(steps: Vec<CanonStep<A>>) -> (r: Result<(CanonStep<A>, CanonStep<A>), Error>)
    ensures
        match r {
            Ok((a, b)) => spec_endpoints(steps@.map_values(|s: CanonStep<A>| s@)) == Ok::<
                (CanonStepV<A>, CanonStepV<A>),
                Error,
            >((a@, b@)),
            Err(e) => spec_endpoints(steps@.map_values(|s: CanonStep<A>| s@)) == Err::<
                (CanonStepV<A>, CanonStepV<A>),
                Error,
            >(e),
        },
{
    unimplemented!()
}

/// Whether the steps' `executionOrder` values increase.
#[verifier::external_body]
#[allow(unused_variables)]
pub fn check_increasing<A>(steps: &[RawStep<A>]) -> (r: Result<(), Error>)
    ensures
        r == (if spec_increasing(steps@.map_values(|s: RawStep<A>| step_order(s@))) {
            Ok::<(), Error>(())
        } else {
            Err(Error::NotIncreasing)
        }),
{
    unimplemented!()
}

// ---------------------------------------------------------------------------
// Renumbering a table and every reference into it resolves alike; out of range stays an error.

pub proof fn lemma_resolve_renumbered<T>(t: Seq<T>, u: Seq<T>, pi: Seq<int>, i: int)
    requires
        renumbered_table(t, u, pi),
    ensures
        0 <= i < t.len() ==> spec_resolve(u, pi[i]) == spec_resolve(t, i),
        !(0 <= i < t.len()) ==> spec_resolve(t, i) == Err::<T, Error>(Error::OutOfRange)
            && spec_resolve(u, i) == Err::<T, Error>(Error::OutOfRange),
        spec_resolve(t, i) is Err <==> !(0 <= i < t.len()),
{
    admit();
}

// ---------------------------------------------------------------------------
// A bag is sorted and a permutation of its input, so any permutation of the input gives the
// same bag.

pub proof fn lemma_canon_bag<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>)
    requires
        total_order(ord),
    ensures
        sorted(ord, spec_canon_bag(ord, s)),
        spec_canon_bag(ord, s).to_multiset() == s.to_multiset(),
{
    admit();
}

pub proof fn lemma_canon_bag_unique<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        s.to_multiset() == t.to_multiset(),
    ensures
        spec_canon_bag(ord, s) == spec_canon_bag(ord, t),
{
    admit();
}

pub proof fn lemma_canon_bag_permuted<X, V>(
    ord: spec_fn(V, V) -> Ordering,
    f: spec_fn(X) -> V,
    xs: Seq<X>,
    ys: Seq<X>,
    pi: Seq<int>,
)
    requires
        total_order(ord),
        equal_means_eq(ord),
        is_permutation(pi, xs.len()),
        ys.len() == xs.len(),
        forall|k: int| 0 <= k < xs.len() ==> ys[#[trigger] pi[k]] == xs[k],
    ensures
        spec_canon_bag(ord, spec_canon_seq(ys, f)) == spec_canon_bag(ord, spec_canon_seq(xs, f)),
{
    admit();
}

// ---------------------------------------------------------------------------
// A sequence keeps its length and order, so swapping two elements `f` tells apart changes it.

pub proof fn lemma_canon_seq<X, V>(xs: Seq<X>, f: spec_fn(X) -> V)
    ensures
        spec_canon_seq(xs, f).len() == xs.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] spec_canon_seq(xs, f)[k] == f(xs[k]),
{
    admit();
}

pub proof fn lemma_canon_seq_swap<X, V>(xs: Seq<X>, f: spec_fn(X) -> V, j: int, k: int)
    requires
        0 <= j < k < xs.len(),
        f(xs[j]) != f(xs[k]),
    ensures
        spec_canon_seq(xs.update(j, xs[k]).update(k, xs[j]), f) != spec_canon_seq(xs, f),
{
    admit();
}

/// `spec_all_ok` is `Ok` of every value when every element is `Ok`, else the first error.
pub proof fn lemma_all_ok<V>(s: Seq<Result<V, Error>>)
    ensures
        spec_all_ok(s) is Ok <==> forall|k: int| 0 <= k < s.len() ==> (#[trigger] s[k]) is Ok,
        spec_all_ok(s) is Ok ==> spec_all_ok(s)->Ok_0.len() == s.len() && forall|k: int|
            0 <= k < s.len() ==> #[trigger] spec_all_ok(s)->Ok_0[k] == s[k]->Ok_0,
        spec_all_ok(s) is Err ==> exists|k: int|
            0 <= k < s.len() && (#[trigger] s[k]) is Err && s[k]->Err_0 == spec_all_ok(s)->Err_0
                && forall|j: int| 0 <= j < k ==> (#[trigger] s[j]) is Ok,
{
    admit();
}

// ---------------------------------------------------------------------------
// Endpoints are the first and last elements, err exactly on an empty sequence, and do not see
// anything between.

pub proof fn lemma_endpoints<T>(s: Seq<T>, t: Seq<T>)
    ensures
        spec_endpoints(s) is Err <==> s.len() == 0,
        s.len() == 0 ==> spec_endpoints(s) == Err::<(T, T), Error>(Error::EmptyFlow),
        s.len() > 0 ==> spec_endpoints(s) == Ok::<(T, T), Error>((s[0], s.last())),
        s.len() > 0 && t.len() > 0 && s[0] == t[0] && s.last() == t.last() ==> spec_endpoints(s)
            == spec_endpoints(t),
{
    admit();
}

} // verus!
