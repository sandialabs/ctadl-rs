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
use crate::sort::sort_by;
#[cfg(verus_keep_ghost)]
use crate::sort::{lemma_le_total, lemma_sorted_unique};
#[cfg(verus_keep_ghost)]
use multiset_diff::sorted;
#[cfg(verus_keep_ghost)]
use order_laws::{computes, le, total_order};

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
pub fn resolve<T>(table: &[T], i: usize) -> (r: Result<&T, Error>)
    ensures
        match r {
            Ok(x) => spec_resolve(table@, i as int) == Ok::<T, Error>(*x),
            Err(e) => spec_resolve(table@, i as int) == Err::<T, Error>(e),
        },
{
    if i < table.len() {
        Ok(&table[i])
    } else {
        Err(Error::OutOfRange)
    }
}

/// `f` of each element of `xs`, in order, or the first error `f` returns. `f` computes `sf` on
/// views.
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
    let ghost s = spec_canon_seq(xs@, sf);
    let mut out: Vec<C> = Vec::new();
    let mut i: usize = 0;
    while i < xs.len()
        invariant
            i <= xs@.len(),
            out@.len() == i,
            s == spec_canon_seq(xs@, sf),
            forall|x: &X| #[trigger] f.requires((x,)),
            forall|x: &X, c: Result<C, Error>| #[trigger]
                f.ensures((x,), c) ==> lift(view, c) == sf(*x),
            forall|k: int|
                0 <= k < i ==> (#[trigger] s[k]) is Ok && s[k]->Ok_0 == view(out@[k]),
        decreases xs@.len() - i,
    {
        match f(&xs[i]) {
            Err(e) => {
                proof {
                    assert(s[i as int] == sf(xs@[i as int]));
                    assert(s[i as int] == Err::<V, Error>(e));
                    lemma_all_ok(s);
                    assert(spec_all_ok(s) is Err);
                    let k = choose|k: int|
                        0 <= k < s.len() && (#[trigger] s[k]) is Err && s[k]->Err_0
                            == spec_all_ok(s)->Err_0 && forall|j: int|
                            0 <= j < k ==> (#[trigger] s[j]) is Ok;
                    if k < i {
                        assert(s[k] is Ok);
                    } else if k > i {
                        assert(s[i as int] is Ok);
                    }
                }
                return Err(e);
            },
            Ok(c) => {
                out.push(c);
            },
        }
        i += 1;
    }
    proof {
        lemma_all_ok(s);
        assert(spec_all_ok(s) is Ok);
        assert(out@.map_values(view) =~= spec_all_ok(s)->Ok_0);
    }
    Ok(out)
}

/// `xs` as a bag: sorted by the order `cmp` computes on views, duplicates kept.
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
    let ghost xs0 = xs@;
    proof {
        lemma_by_view_total(view, ord);
    }
    let r = sort_by(xs, cmp, Ghost(by_view(view, ord)));
    proof {
        let rv = r@.map_values(view);
        let xv = xs0.map_values(view);
        lemma_map_multiset(r@, xs0, view);
        assert(sorted(ord, rv)) by {
            assert forall|p: int, q: int| 0 <= p < q < rv.len() implies le(
                ord,
                #[trigger] rv[p],
                #[trigger] rv[q],
            ) by {
                assert(le(by_view(view, ord), r@[p], r@[q]));
            }
        }
        lemma_canon_bag(ord, xv);
        lemma_sorted_unique(ord, rv, spec_canon_bag(ord, xv));
    }
    r
}

/// A thread flow's first and last steps.
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
    let ghost s = steps@;
    let mut steps = steps;
    let n = steps.len();
    if n == 0 {
        return Err(Error::EmptyFlow);
    }
    let last = steps.pop().unwrap();
    let first = if n == 1 {
        copy_step(&last)
    } else {
        steps.swap_remove(0)
    };
    proof {
        let sv = s.map_values(|x: CanonStep<A>| x@);
        assert(sv[0] == s[0]@);
        assert(sv.last() == s[s.len() - 1]@);
    }
    Ok((first, last))
}

/// Whether the steps' `executionOrder` values increase.
pub fn check_increasing<A>(steps: &[RawStep<A>]) -> (r: Result<(), Error>)
    ensures
        r == (if spec_increasing(steps@.map_values(|s: RawStep<A>| step_order(s@))) {
            Ok::<(), Error>(())
        } else {
            Err(Error::NotIncreasing)
        }),
{
    let ghost os = steps@.map_values(|s: RawStep<A>| step_order(s@));
    // The last value present so far, which is the greatest.
    let mut prev: Option<i64> = None;
    let mut i: usize = 0;
    while i < steps.len()
        invariant
            i <= steps@.len(),
            os == steps@.map_values(|s: RawStep<A>| step_order(s@)),
            forall|a: int, b: int|
                0 <= a < b < i && (#[trigger] os[a]) is Some && (#[trigger] os[b]) is Some
                    ==> os[a]->Some_0 < os[b]->Some_0,
            prev is Some ==> exists|k: int| 0 <= k < i && #[trigger] os[k] == prev,
            prev is Some ==> forall|k: int|
                0 <= k < i && (#[trigger] os[k]) is Some ==> os[k]->Some_0 <= prev->Some_0,
            prev is None ==> forall|k: int| 0 <= k < i ==> (#[trigger] os[k]) is None,
        decreases steps@.len() - i,
    {
        assert(os[i as int] == steps@[i as int].order);
        if let Some(o) = steps[i].order {
            match prev {
                Some(p) if o <= p => {
                    proof {
                        let k = choose|k: int| 0 <= k < i && #[trigger] os[k] == prev;
                        assert(os[k] is Some && os[i as int] is Some);
                        assert(!spec_increasing(os));
                    }
                    return Err(Error::NotIncreasing);
                },
                _ => {},
            }
            prev = Some(o);
            proof {
                assert(os[i as int] == prev);
            }
        }
        i += 1;
    }
    Ok(())
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
    if 0 <= i < t.len() {
        assert(0 <= pi[i] < t.len());
    }
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
    decreases s.len(),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    if s.len() == 0 {
        assert(spec_canon_bag(ord, s) =~= s);
    } else {
        let rest = spec_canon_bag(ord, s.drop_first());
        lemma_canon_bag(ord, s.drop_first());
        lemma_insert_sorted(ord, s[0], rest);
        lemma_cons_multiset(s);
    }
}

pub proof fn lemma_canon_bag_unique<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        s.to_multiset() == t.to_multiset(),
    ensures
        spec_canon_bag(ord, s) == spec_canon_bag(ord, t),
{
    lemma_canon_bag(ord, s);
    lemma_canon_bag(ord, t);
    lemma_sorted_unique(ord, spec_canon_bag(ord, s), spec_canon_bag(ord, t));
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
    let fx = spec_canon_seq(xs, f);
    let fy = spec_canon_seq(ys, f);
    assert forall|k: int| 0 <= k < fx.len() implies fy[#[trigger] pi[k]] == fx[k] by {
        assert(0 <= pi[k] < xs.len());
        assert(ys[pi[k]] == xs[k]);
    }
    lemma_permuted_multiset(fx, fy, pi);
    lemma_canon_bag_unique(ord, fy, fx);
}

// ---------------------------------------------------------------------------
// A sequence keeps its length and order, so swapping two elements `f` tells apart changes it.

pub proof fn lemma_canon_seq<X, V>(xs: Seq<X>, f: spec_fn(X) -> V)
    ensures
        spec_canon_seq(xs, f).len() == xs.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] spec_canon_seq(xs, f)[k] == f(xs[k]),
{
}

pub proof fn lemma_canon_seq_swap<X, V>(xs: Seq<X>, f: spec_fn(X) -> V, j: int, k: int)
    requires
        0 <= j < k < xs.len(),
        f(xs[j]) != f(xs[k]),
    ensures
        spec_canon_seq(xs.update(j, xs[k]).update(k, xs[j]), f) != spec_canon_seq(xs, f),
{
    let ys = xs.update(j, xs[k]).update(k, xs[j]);
    assert(spec_canon_seq(ys, f)[j] == f(xs[k]));
    assert(spec_canon_seq(xs, f)[j] == f(xs[j]));
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
    decreases s.len(),
{
    if s.len() == 0 {
        return;
    }
    let t = s.drop_first();
    lemma_all_ok(t);
    assert forall|k: int| 0 < k < s.len() implies s[k] == #[trigger] t[k - 1] by {}
    match s[0] {
        Err(e) => {
            assert(spec_all_ok(s) == Err::<Seq<V>, Error>(e));
        },
        Ok(v) => {
            match spec_all_ok(t) {
                Err(e) => {
                    assert(spec_all_ok(s) == Err::<Seq<V>, Error>(e));
                    let k = choose|k: int|
                        0 <= k < t.len() && (#[trigger] t[k]) is Err && t[k]->Err_0
                            == spec_all_ok(t)->Err_0 && forall|j: int|
                            0 <= j < k ==> (#[trigger] t[j]) is Ok;
                    assert(s[k + 1] == t[k]);
                    assert forall|j: int| 0 <= j < k + 1 implies (#[trigger] s[j]) is Ok by {
                        if j > 0 {
                            assert(t[j - 1] is Ok);
                        }
                    }
                },
                Ok(rest) => {
                    assert(spec_all_ok(s) == Ok::<Seq<V>, Error>(seq![v] + rest));
                    assert forall|k: int| 0 <= k < s.len() implies (#[trigger] s[k]) is Ok by {
                        if k > 0 {
                            assert(t[k - 1] is Ok);
                        }
                    }
                    assert forall|k: int| 0 <= k < s.len() implies #[trigger] spec_all_ok(
                        s,
                    )->Ok_0[k] == s[k]->Ok_0 by {
                        if k > 0 {
                            assert(rest[k - 1] == t[k - 1]->Ok_0);
                        }
                    }
                },
            }
        },
    }
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
}

// ---------------------------------------------------------------------------
// Helpers.

/// A copy of a step, which the derived `Clone` gives no spec for.
pub fn copy_step<A: Copy>(s: &CanonStep<A>) -> (r: CanonStep<A>)
    ensures
        r@ == s@,
{
    let location = match &s.location {
        Some(l) => Some(copy_location(l)),
        None => None,
    };
    CanonStep { kinds: copy_atoms(&s.kinds), location }
}

fn copy_location<A: Copy>(l: &CanonLocation<A>) -> (r: CanonLocation<A>)
    ensures
        r@ == l@,
{
    let physical = match &l.physical {
        Some(p) => Some(copy_physical(p)),
        None => None,
    };
    CanonLocation { physical, logical: copy_atoms(&l.logical) }
}

pub fn copy_physical<A: Copy>(p: &Physical<A>) -> (r: Physical<A>)
    ensures
        r == *p,
{
    let address = match &p.address {
        Some(a) => Some(Address { absolute: a.absolute, relative: a.relative, kind: a.kind }),
        None => None,
    };
    let g = &p.region;
    Physical {
        base: p.base,
        uri: p.uri,
        region: Region {
            start_line: g.start_line,
            start_column: g.start_column,
            end_line: g.end_line,
            end_column: g.end_column,
            byte_offset: g.byte_offset,
            byte_length: g.byte_length,
        },
        address,
    }
}

pub fn copy_atoms<A: Copy>(v: &[A]) -> (r: Vec<A>)
    ensures
        r@ == v@,
{
    let mut r: Vec<A> = Vec::new();
    let mut i: usize = 0;
    while i < v.len()
        invariant
            i <= v@.len(),
            r@ == v@.subrange(0, i as int),
        decreases v@.len() - i,
    {
        r.push(v[i]);
        i += 1;
        proof {
            assert(r@ =~= v@.subrange(0, i as int));
        }
    }
    proof {
        assert(r@ =~= v@);
    }
    r
}

/// An order on values is a total order on anything compared by its value.
proof fn lemma_by_view_total<C, V>(view: spec_fn(C) -> V, ord: spec_fn(V, V) -> Ordering)
    requires
        total_order(ord),
    ensures
        total_order(by_view(view, ord)),
{
    let o = by_view(view, ord);
    assert forall|x: C, y: C| (#[trigger] o(x, y) == Ordering::Less) <==> o(y, x) == Ordering::Greater by {
        assert(ord(view(x), view(y)) == Ordering::Less <==> ord(view(y), view(x)) == Ordering::Greater);
    }
    assert forall|x: C, y: C| (#[trigger] o(x, y) == Ordering::Equal) implies o(y, x) == Ordering::Equal by {
        assert(ord(view(x), view(y)) == Ordering::Equal);
    }
    assert forall|x: C, y: C, z: C| #[trigger] le(o, x, y) && #[trigger] le(o, y, z) implies le(o, x, z) by {
        assert(le(ord, view(x), view(y)) && le(ord, view(y), view(z)));
    }
}

/// A sequence's multiset is its head's plus its tail's.
proof fn lemma_cons_multiset<T>(s: Seq<T>)
    requires
        s.len() > 0,
    ensures
        s.to_multiset() == s.drop_first().to_multiset().insert(s[0]),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    assert(s.remove(0) =~= s.drop_first());
    assert(s.to_multiset().count(s[0]) > 0) by {
        assert(s.contains(s[0]));
    }
    assert(s.to_multiset() =~= s.to_multiset().remove(s[0]).insert(s[0]));
}

/// Inserting into a sorted sequence keeps it sorted and adds the element to its multiset.
proof fn lemma_insert_sorted<T>(ord: spec_fn(T, T) -> Ordering, x: T, s: Seq<T>)
    requires
        total_order(ord),
        sorted(ord, s),
    ensures
        sorted(ord, insert_sorted(ord, x, s)),
        insert_sorted(ord, x, s).to_multiset() == s.to_multiset().insert(x),
        insert_sorted(ord, x, s).len() == s.len() + 1,
    decreases s.len(),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    let r = insert_sorted(ord, x, s);
    if s.len() == 0 {
        assert(r =~= Seq::<T>::empty().push(x));
        assert(Seq::<T>::empty().to_multiset() =~= s.to_multiset()) by {
            assert(s =~= Seq::<T>::empty());
        }
    } else if le(ord, x, s[0]) {
        assert(r == seq![x] + s);
        lemma_cons_multiset(r);
        assert(r.drop_first() =~= s);
        assert forall|p: int, q: int| 0 <= p < q < r.len() implies le(
            ord,
            #[trigger] r[p],
            #[trigger] r[q],
        ) by {
            if p == 0 {
                if q > 1 {
                    assert(le(ord, s[0], s[q - 1]));
                }
            } else {
                assert(le(ord, s[p - 1], s[q - 1]));
            }
        }
    } else {
        let t = s.drop_first();
        assert(sorted(ord, t)) by {
            assert forall|p: int, q: int| 0 <= p < q < t.len() implies le(
                ord,
                #[trigger] t[p],
                #[trigger] t[q],
            ) by {
                assert(le(ord, s[p + 1], s[q + 1]));
            }
        }
        lemma_insert_sorted(ord, x, t);
        let rest = insert_sorted(ord, x, t);
        assert(r == seq![s[0]] + rest);
        lemma_cons_multiset(r);
        assert(r.drop_first() =~= rest);
        lemma_cons_multiset(s);
        assert(r.to_multiset() =~= s.to_multiset().insert(x));
        lemma_le_total(ord, x, s[0]);
        // Every element of `rest` is `x` or an element of `t`, so at least `s[0]`.
        assert forall|q: int| 0 <= q < rest.len() implies le(ord, s[0], #[trigger] rest[q]) by {
            assert(rest.contains(rest[q]));
            assert(rest.to_multiset().count(rest[q]) > 0);
            if rest[q] != x {
                assert(t.to_multiset().count(rest[q]) > 0);
                assert(t.contains(rest[q]));
                let m = choose|m: int| 0 <= m < t.len() && t[m] == rest[q];
                assert(le(ord, s[0], s[m + 1]));
            }
        }
        assert forall|p: int, q: int| 0 <= p < q < r.len() implies le(
            ord,
            #[trigger] r[p],
            #[trigger] r[q],
        ) by {
            if p == 0 {
                assert(r[q] == rest[q - 1]);
            } else {
                assert(le(ord, rest[p - 1], rest[q - 1]));
            }
        }
    }
}

/// Mapping two sequences with equal multisets gives sequences with equal multisets.
proof fn lemma_map_multiset<X, V>(s: Seq<X>, t: Seq<X>, f: spec_fn(X) -> V)
    requires
        s.to_multiset() == t.to_multiset(),
    ensures
        s.map_values(f).to_multiset() == t.map_values(f).to_multiset(),
    decreases s.len(),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    assert(s.len() == s.to_multiset().len());
    assert(t.len() == t.to_multiset().len());
    if s.len() == 0 {
        assert(s.map_values(f) =~= t.map_values(f));
        return;
    }
    let x = s[0];
    assert(s.contains(x));
    assert(t.to_multiset().count(x) > 0);
    assert(t.contains(x));
    let k = choose|k: int| 0 <= k < t.len() && t[k] == x;
    let t1 = t.remove(k);
    lemma_cons_multiset(s);
    assert(s.drop_first().to_multiset() =~= t1.to_multiset()) by {
        assert(t1.to_multiset() == t.to_multiset().remove(x));
    }
    lemma_map_multiset(s.drop_first(), t1, f);
    let sm = s.map_values(f);
    let tm = t.map_values(f);
    lemma_cons_multiset(sm);
    assert(sm.drop_first() =~= s.drop_first().map_values(f));
    assert(tm.remove(k) =~= t1.map_values(f));
    assert(tm.to_multiset().count(f(x)) > 0) by {
        assert(tm[k] == f(x));
        assert(tm.contains(f(x)));
    }
    assert(tm.remove(k).to_multiset() == tm.to_multiset().remove(f(x)));
    assert(tm.to_multiset() =~= tm.to_multiset().remove(f(x)).insert(f(x)));
}

/// A permutation keeps the multiset.
proof fn lemma_permuted_multiset<T>(xs: Seq<T>, ys: Seq<T>, pi: Seq<int>)
    requires
        is_permutation(pi, xs.len()),
        ys.len() == xs.len(),
        forall|k: int| 0 <= k < xs.len() ==> ys[#[trigger] pi[k]] == xs[k],
    ensures
        xs.to_multiset() == ys.to_multiset(),
    decreases xs.len(),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    let n = xs.len() as int;
    if n == 0 {
        assert(xs =~= ys);
        return;
    }
    let m = pi[n - 1];
    assert(0 <= m < n);
    let xs1 = xs.drop_last();
    let ys1 = ys.remove(m);
    let pi1 = Seq::new((n - 1) as nat, |k: int| if pi[k] < m { pi[k] } else { pi[k] - 1 });
    assert forall|k: int| 0 <= k < n - 1 implies #[trigger] pi[k] != m by {
        assert(pi[k] != pi[n - 1]);
    }
    assert(is_permutation(pi1, xs1.len())) by {
        assert forall|i: int| 0 <= i < n - 1 implies 0 <= #[trigger] pi1[i] < n - 1 by {
            assert(0 <= pi[i] < n && pi[i] != m);
        }
        assert forall|i: int, j: int| 0 <= i < j < n - 1 implies #[trigger] pi1[i]
            != #[trigger] pi1[j] by {
            assert(pi[i] != pi[j]);
            assert(pi[i] != m && pi[j] != m);
        }
    }
    assert forall|k: int| 0 <= k < xs1.len() implies ys1[#[trigger] pi1[k]] == xs1[k] by {
        assert(ys[pi[k]] == xs[k]);
        assert(0 <= pi[k] < n && pi[k] != m);
    }
    lemma_permuted_multiset(xs1, ys1, pi1);
    assert(xs =~= xs1.push(xs[n - 1]));
    assert(ys[m] == xs[n - 1]);
    assert(ys.to_multiset().count(ys[m]) > 0) by {
        assert(ys.contains(ys[m]));
    }
    assert(ys1.to_multiset() == ys.to_multiset().remove(ys[m]));
    assert(ys.to_multiset() =~= ys.to_multiset().remove(ys[m]).insert(ys[m]));
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ghost;

    fn by_value(a: &u32, b: &u32) -> Ordering {
        a.cmp(b)
    }

    /// A deterministic generator of small numbers, so duplicates are common.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % u64::from(bound)) as u32
        }
    }

    fn canon_step(kind: u32) -> CanonStep<u32> {
        CanonStep {
            kinds: vec![kind],
            location: Some(CanonLocation {
                physical: Some(Physical {
                    base: Some(1),
                    uri: Some(2),
                    region: Region {
                        start_line: Some(3),
                        start_column: None,
                        end_line: Some(4),
                        end_column: None,
                        byte_offset: None,
                        byte_length: None,
                    },
                    address: Some(Address {
                        absolute: Some(5),
                        relative: None,
                        kind: Some(6),
                    }),
                }),
                logical: vec![7, 8],
            }),
        }
    }

    #[test]
    fn sort_by_matches_std() {
        let mut g = Lcg(1);
        for len in 0..=64 {
            for _ in 0..8 {
                let v: Vec<u32> = (0..len).map(|_| g.next(len.max(1) / 2 + 1)).collect();
                let mut want = v.clone();
                want.sort();
                assert_eq!(sort_by(v, &by_value, ghost()), want);
            }
        }
    }

    #[test]
    fn canon_bag_sorts_and_keeps_duplicates() {
        let r = canon_bag::<u32, u32, _>(vec![3u32, 1, 3, 2], &by_value, ghost(), ghost());
        assert_eq!(r, vec![1, 2, 3, 3]);
    }

    #[test]
    fn canon_seq_keeps_order_and_stops_at_first_error() {
        let f = |x: &u32| {
            if *x == 0 {
                Err(Error::OutOfRange)
            } else if *x == 9 {
                Err(Error::EmptyFlow)
            } else {
                Ok(*x * 10)
            }
        };
        let ok = canon_seq::<u32, u32, u32, _>(&[3u32, 1, 2], &f, ghost(), ghost());
        assert_eq!(ok, Ok(vec![30, 10, 20]));
        let err = canon_seq::<u32, u32, u32, _>(&[3u32, 0, 9], &f, ghost(), ghost());
        assert_eq!(err, Err(Error::OutOfRange));
    }

    #[test]
    fn endpoints_cases() {
        assert_eq!(endpoints::<u32>(vec![]), Err(Error::EmptyFlow));
        let one = endpoints(vec![canon_step(1)]);
        assert_eq!(one, Ok((canon_step(1), canon_step(1))));
        let three = endpoints(vec![canon_step(1), canon_step(2), canon_step(3)]);
        assert_eq!(three, Ok((canon_step(1), canon_step(3))));
    }

    #[test]
    fn resolve_cases() {
        let table = [10u32, 20];
        assert_eq!(resolve(&table, 1), Ok(&20));
        assert_eq!(resolve(&table, 2), Err(Error::OutOfRange));
        assert_eq!(resolve::<u32>(&[], 0), Err(Error::OutOfRange));
    }
}
