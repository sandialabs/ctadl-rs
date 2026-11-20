//! The order on the canonical types, built from an order on atoms.
//!
//! Each canonical type `X` has a spec order `ord_X(oa)`, defined from an atom order `oa`, and an
//! exec `cmp_X` that computes it. Records compare field by field in declaration order;
//! `None` comes before `Some`; sequences compare element by element, a proper prefix first; a
//! thread flow's `Steps` come before its `Endpoints`. The orders are stated on the views
//! (`types`), so `Equal` means equal contents.

use crate::types::*;
use std::cmp::Ordering;
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use order_laws::{
    computes, eqv, le, lemma_lex_equal, lemma_lex_total, lemma_trans, lex, lex_ord, lt, total_order,
};

verus! {

// ---------------------------------------------------------------------------
// Building blocks.

/// `ord` calls two values `Equal` exactly when they are the same value.
pub open spec fn equal_means_eq<T>(ord: spec_fn(T, T) -> Ordering) -> bool {
    forall|x: T, y: T| #[trigger] ord(x, y) == Ordering::Equal <==> x == y
}

/// The first comparison, unless it is `Equal`; then the second.
pub open spec fn then(first: Ordering, second: Ordering) -> Ordering {
    if first == Ordering::Equal {
        second
    } else {
        first
    }
}

pub open spec fn ord_int(x: int, y: int) -> Ordering {
    if x < y {
        Ordering::Less
    } else if x > y {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// `None` first, then `Some` by `ord`.
pub open spec fn ord_option<T>(ord: spec_fn(T, T) -> Ordering, x: Option<T>, y: Option<T>) -> Ordering {
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => ord(a, b),
    }
}

pub open spec fn ord_opt_i64(x: Option<i64>, y: Option<i64>) -> Ordering {
    ord_option(|a: i64, b: i64| ord_int(a as int, b as int), x, y)
}

pub open spec fn ord_opt_atom<A>(oa: spec_fn(A, A) -> Ordering, x: Option<A>, y: Option<A>) -> Ordering {
    ord_option(oa, x, y)
}

pub open spec fn ord_opt_atoms<A>(
    oa: spec_fn(A, A) -> Ordering,
    x: Option<Seq<A>>,
    y: Option<Seq<A>>,
) -> Ordering {
    ord_option(|s: Seq<A>, t: Seq<A>| lex(oa, s, t), x, y)
}

pub open spec fn ord_opt_keyeds<A>(
    oa: spec_fn(A, A) -> Ordering,
    x: Option<Seq<KeyedV<A>>>,
    y: Option<Seq<KeyedV<A>>>,
) -> Ordering {
    ord_option(|s: Seq<KeyedV<A>>, t: Seq<KeyedV<A>>| lex(ord_keyed(oa), s, t), x, y)
}

/// An order on views, as an order on the values they view.
pub open spec fn by_view<C, V>(view: spec_fn(C) -> V, ord: spec_fn(V, V) -> Ordering) -> spec_fn(
    C,
    C,
) -> Ordering {
    |x: C, y: C| ord(view(x), view(y))
}

// ---------------------------------------------------------------------------
// The order on each canonical type.

pub open spec fn ord_region() -> spec_fn(Region, Region) -> Ordering {
    |x: Region, y: Region|
        then(
            ord_opt_i64(x.start_line, y.start_line),
            then(
                ord_opt_i64(x.start_column, y.start_column),
                then(
                    ord_opt_i64(x.end_line, y.end_line),
                    then(
                        ord_opt_i64(x.end_column, y.end_column),
                        then(
                            ord_opt_i64(x.byte_offset, y.byte_offset),
                            ord_opt_i64(x.byte_length, y.byte_length),
                        ),
                    ),
                ),
            ),
        )
}

pub open spec fn ord_address<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(Address<A>, Address<A>) -> Ordering {
    |x: Address<A>, y: Address<A>|
        then(
            ord_opt_i64(x.absolute, y.absolute),
            then(ord_opt_i64(x.relative, y.relative), ord_opt_atom(oa, x.kind, y.kind)),
        )
}

pub open spec fn ord_physical<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(Physical<A>, Physical<A>) -> Ordering {
    |x: Physical<A>, y: Physical<A>|
        then(
            ord_opt_atom(oa, x.base, y.base),
            then(
                ord_opt_atom(oa, x.uri, y.uri),
                then(
                    ord_region()(x.region, y.region),
                    ord_option(ord_address(oa), x.address, y.address),
                ),
            ),
        )
}

pub open spec fn ord_keyed<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(KeyedV<A>, KeyedV<A>) -> Ordering {
    |x: KeyedV<A>, y: KeyedV<A>| then(oa(x.key, y.key), lex(oa, x.values, y.values))
}

pub open spec fn ord_props<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(PropsV<A>, PropsV<A>) -> Ordering {
    |x: PropsV<A>, y: PropsV<A>|
        then(
            ord_opt_atoms(oa, x.labels, y.labels),
            then(
                ord_opt_atom(oa, x.vertex, y.vertex),
                then(
                    ord_opt_keyeds(oa, x.vertices, y.vertices),
                    then(
                        ord_opt_atoms(oa, x.source_fns, y.source_fns),
                        then(
                            ord_opt_atoms(oa, x.sink_fns, y.sink_fns),
                            then(
                                ord_opt_keyeds(oa, x.tainted_formals, y.tainted_formals),
                                ord_opt_atom(oa, x.import, y.import),
                            ),
                        ),
                    ),
                ),
            ),
        )
}

pub open spec fn ord_location<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(
    CanonLocationV<A>,
    CanonLocationV<A>,
) -> Ordering {
    |x: CanonLocationV<A>, y: CanonLocationV<A>|
        then(ord_option(ord_physical(oa), x.physical, y.physical), lex(oa, x.logical, y.logical))
}

pub open spec fn ord_step<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(CanonStepV<A>, CanonStepV<A>) -> Ordering {
    |x: CanonStepV<A>, y: CanonStepV<A>|
        then(lex(oa, x.kinds, y.kinds), ord_option(ord_location(oa), x.location, y.location))
}

pub open spec fn ord_thread_flow<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(
    CanonThreadFlowV<A>,
    CanonThreadFlowV<A>,
) -> Ordering {
    |x: CanonThreadFlowV<A>, y: CanonThreadFlowV<A>|
        match (x, y) {
            (CanonThreadFlowV::Steps(s), CanonThreadFlowV::Steps(t)) => lex(ord_step(oa), s, t),
            (CanonThreadFlowV::Steps(_), CanonThreadFlowV::Endpoints(_, _)) => Ordering::Less,
            (CanonThreadFlowV::Endpoints(_, _), CanonThreadFlowV::Steps(_)) => Ordering::Greater,
            (CanonThreadFlowV::Endpoints(a, b), CanonThreadFlowV::Endpoints(c, d)) => then(
                ord_step(oa)(a, c),
                ord_step(oa)(b, d),
            ),
        }
}

pub open spec fn ord_code_flow<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(
    CanonCodeFlowV<A>,
    CanonCodeFlowV<A>,
) -> Ordering {
    |x: CanonCodeFlowV<A>, y: CanonCodeFlowV<A>| lex(ord_thread_flow(oa), x.thread_flows, y.thread_flows)
}

pub open spec fn ord_result<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(
    CanonResultV<A>,
    CanonResultV<A>,
) -> Ordering {
    |x: CanonResultV<A>, y: CanonResultV<A>|
        then(
            ord_opt_atom(oa, x.rule, y.rule),
            then(
                oa(x.kind, y.kind),
                then(
                    oa(x.level, y.level),
                    then(
                        lex(ord_location(oa), x.locations, y.locations),
                        then(
                            lex(ord_code_flow(oa), x.code_flows, y.code_flows),
                            ord_props(oa)(x.props, y.props),
                        ),
                    ),
                ),
            ),
        )
}

/// `ord_result` on the results the code holds, through their views: what `cmp_result`
/// computes, and what `multiset_diff` is given.
pub open spec fn ord_result_exec<A>(oa: spec_fn(A, A) -> Ordering) -> spec_fn(
    CanonResult<A>,
    CanonResult<A>,
) -> Ordering {
    by_view(|c: CanonResult<A>| c@, ord_result(oa))
}

// ---------------------------------------------------------------------------
// Combinators the orders above are built from, as closures, so that a lemma about one applies
// wherever it occurs.

/// A total order whose `Equal` is equality.
pub open spec fn strict<T>(ord: spec_fn(T, T) -> Ordering) -> bool {
    total_order(ord) && equal_means_eq(ord)
}

pub open spec fn ord_i64() -> spec_fn(i64, i64) -> Ordering {
    |a: i64, b: i64| ord_int(a as int, b as int)
}

pub open spec fn ord_opt<T>(ord: spec_fn(T, T) -> Ordering) -> spec_fn(Option<T>, Option<T>) -> Ordering {
    |x: Option<T>, y: Option<T>| ord_option(ord, x, y)
}

/// Pairs, by their first components, then their second.
pub open spec fn ord_pair<U, V>(o1: spec_fn(U, U) -> Ordering, o2: spec_fn(V, V) -> Ordering) -> spec_fn(
    (U, V),
    (U, V),
) -> Ordering {
    |p: (U, V), q: (U, V)| then(o1(p.0, q.0), o2(p.1, q.1))
}

// ---------------------------------------------------------------------------
// Each combinator keeps `strict`.

proof fn lemma_strict_i64()
    ensures
        strict(ord_i64()),
{
    let o = ord_i64();
    assert forall|x: i64, y: i64| #[trigger] o(x, y) == Ordering::Equal <==> x == y by {}
    assert forall|x: i64, y: i64, z: i64| #[trigger] le(o, x, y) && #[trigger] le(o, y, z) implies le(
        o,
        x,
        z,
    ) by {}
}

proof fn lemma_strict_opt<T>(ord: spec_fn(T, T) -> Ordering)
    requires
        strict(ord),
    ensures
        strict(ord_opt(ord)),
{
    let o = ord_opt(ord);
    assert forall|x: Option<T>, y: Option<T>|
        (#[trigger] o(x, y) == Ordering::Less) <==> o(y, x) == Ordering::Greater by {
        if x is Some && y is Some {
            let (a, b) = (x->Some_0, y->Some_0);
            assert(ord(a, b) == Ordering::Less <==> ord(b, a) == Ordering::Greater);
        }
    }
    assert forall|x: Option<T>, y: Option<T>| #[trigger] o(x, y) == Ordering::Equal <==> x == y by {
        if x is Some && y is Some {
            assert(ord(x->Some_0, y->Some_0) == Ordering::Equal <==> x->Some_0 == y->Some_0);
        }
    }
    assert forall|x: Option<T>, y: Option<T>, z: Option<T>|
        #[trigger] le(o, x, y) && #[trigger] le(o, y, z) implies le(o, x, z) by {
        if x is Some && y is Some && z is Some {
            lemma_trans(ord, x->Some_0, y->Some_0, z->Some_0);
        }
    }
}

proof fn lemma_strict_pair<U, V>(o1: spec_fn(U, U) -> Ordering, o2: spec_fn(V, V) -> Ordering)
    requires
        strict(o1),
        strict(o2),
    ensures
        strict(ord_pair(o1, o2)),
{
    let o = ord_pair(o1, o2);
    assert forall|p: (U, V), q: (U, V)|
        (#[trigger] o(p, q) == Ordering::Less) <==> o(q, p) == Ordering::Greater by {
        assert(o1(p.0, q.0) == Ordering::Less <==> o1(q.0, p.0) == Ordering::Greater);
        assert(o1(q.0, p.0) == Ordering::Less <==> o1(p.0, q.0) == Ordering::Greater);
        assert(o1(p.0, q.0) == Ordering::Equal <==> p.0 == q.0);
        assert(o1(q.0, p.0) == Ordering::Equal <==> q.0 == p.0);
        assert(o2(p.1, q.1) == Ordering::Less <==> o2(q.1, p.1) == Ordering::Greater);
    }
    assert forall|p: (U, V), q: (U, V)| #[trigger] o(p, q) == Ordering::Equal <==> p == q by {
        assert(o1(p.0, q.0) == Ordering::Equal <==> p.0 == q.0);
        assert(o2(p.1, q.1) == Ordering::Equal <==> p.1 == q.1);
    }
    assert forall|p: (U, V), q: (U, V), r: (U, V)|
        #[trigger] le(o, p, q) && #[trigger] le(o, q, r) implies le(o, p, r) by {
        lemma_trans(o1, p.0, q.0, r.0);
        lemma_trans(o2, p.1, q.1, r.1);
        assert(o1(p.0, q.0) == Ordering::Equal <==> p.0 == q.0);
        assert(o1(q.0, r.0) == Ordering::Equal <==> q.0 == r.0);
        assert(o1(p.0, r.0) == Ordering::Equal <==> p.0 == r.0);
    }
}

/// An order through a key: total, and `Equal` exactly when the keys are equal.
proof fn lemma_pull<T, K>(o: spec_fn(K, K) -> Ordering, key: spec_fn(T) -> K, ord: spec_fn(T, T) -> Ordering)
    requires
        strict(o),
        forall|x: T, y: T| #[trigger] ord(x, y) == o(key(x), key(y)),
    ensures
        total_order(ord),
        forall|x: T, y: T| #[trigger] ord(x, y) == Ordering::Equal <==> key(x) == key(y),
{
    assert forall|x: T, y: T| (#[trigger] ord(x, y) == Ordering::Less) <==> ord(y, x) == Ordering::Greater by {
        assert(o(key(x), key(y)) == Ordering::Less <==> o(key(y), key(x)) == Ordering::Greater);
    }
    assert forall|x: T, y: T| #[trigger] ord(x, y) == Ordering::Equal <==> key(x) == key(y) by {
        assert(o(key(x), key(y)) == Ordering::Equal <==> key(x) == key(y));
    }
    assert forall|x: T, y: T, z: T| #[trigger] le(ord, x, y) && #[trigger] le(ord, y, z) implies le(
        ord,
        x,
        z,
    ) by {
        assert(le(o, key(x), key(y)) && le(o, key(y), key(z)) ==> le(o, key(x), key(z)));
    }
}

// ---------------------------------------------------------------------------
// The lexicographic order keeps `strict`.

proof fn lemma_strict_lex<T>(ord: spec_fn(T, T) -> Ordering)
    requires
        strict(ord),
    ensures
        strict(lex_ord(ord)),
{
    lemma_lex_total(ord);
    let l = lex_ord(ord);
    assert forall|s: Seq<T>, t: Seq<T>| #[trigger] l(s, t) == Ordering::Equal <==> s == t by {
        lemma_lex_equal(ord, s, t);
        if l(s, t) == Ordering::Equal {
            assert forall|k: int| 0 <= k < s.len() implies s[k] == t[k] by {
                assert(eqv(ord, s[k], t[k]));
            }
            assert(s =~= t);
        }
        if s == t {
            assert forall|k: int| 0 <= k < s.len() implies eqv(ord, #[trigger] s[k], t[k]) by {
                assert(ord(s[k], s[k]) == Ordering::Equal);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Each order is a total order whose `Equal` is equality, given that the atom order is.
//
// Each is `ord_pair`s of its fields' orders, through a key that lists the fields.

pub proof fn lemma_ord_region()
    ensures
        total_order(ord_region()),
        equal_means_eq(ord_region()),
{
    let oi = ord_opt(ord_i64());
    lemma_strict_i64();
    lemma_strict_opt(ord_i64());
    let o = ord_pair(oi, ord_pair(oi, ord_pair(oi, ord_pair(oi, ord_pair(oi, oi)))));
    lemma_strict_pair(oi, oi);
    lemma_strict_pair(oi, ord_pair(oi, oi));
    lemma_strict_pair(oi, ord_pair(oi, ord_pair(oi, oi)));
    lemma_strict_pair(oi, ord_pair(oi, ord_pair(oi, ord_pair(oi, oi))));
    lemma_strict_pair(oi, ord_pair(oi, ord_pair(oi, ord_pair(oi, ord_pair(oi, oi)))));
    let key = |r: Region|
        (
            r.start_line,
            (r.start_column, (r.end_line, (r.end_column, (r.byte_offset, r.byte_length)))),
        );
    lemma_pull(o, key, ord_region());
}

pub proof fn lemma_ord_address<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_address(oa)),
        equal_means_eq(ord_address(oa)),
{
    let oi = ord_opt(ord_i64());
    lemma_strict_i64();
    lemma_strict_opt(ord_i64());
    lemma_strict_opt(oa);
    lemma_strict_pair(oi, ord_opt(oa));
    lemma_strict_pair(oi, ord_pair(oi, ord_opt(oa)));
    let key = |x: Address<A>| (x.absolute, (x.relative, x.kind));
    lemma_pull(ord_pair(oi, ord_pair(oi, ord_opt(oa))), key, ord_address(oa));
}

pub proof fn lemma_ord_physical<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_physical(oa)),
        equal_means_eq(ord_physical(oa)),
{
    lemma_ord_region();
    lemma_ord_address(oa);
    lemma_strict_opt(oa);
    lemma_strict_opt(ord_address(oa));
    let (oo, oad) = (ord_opt(oa), ord_opt(ord_address(oa)));
    lemma_strict_pair(ord_region(), oad);
    lemma_strict_pair(oo, ord_pair(ord_region(), oad));
    lemma_strict_pair(oo, ord_pair(oo, ord_pair(ord_region(), oad)));
    let key = |x: Physical<A>| (x.base, (x.uri, (x.region, x.address)));
    lemma_pull(ord_pair(oo, ord_pair(oo, ord_pair(ord_region(), oad))), key, ord_physical(oa));
}

pub proof fn lemma_ord_keyed<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_keyed(oa)),
        equal_means_eq(ord_keyed(oa)),
{
    lemma_strict_lex(oa);
    lemma_strict_pair(oa, lex_ord(oa));
    let key = |x: KeyedV<A>| (x.key, x.values);
    lemma_pull(ord_pair(oa, lex_ord(oa)), key, ord_keyed(oa));
}

pub proof fn lemma_ord_props<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_props(oa)),
        equal_means_eq(ord_props(oa)),
{
    lemma_ord_keyed(oa);
    lemma_strict_lex(oa);
    lemma_strict_lex(ord_keyed(oa));
    lemma_strict_opt(oa);
    lemma_strict_opt(lex_ord(oa));
    lemma_strict_opt(lex_ord(ord_keyed(oa)));
    let (o1, os, ok) = (ord_opt(oa), ord_opt(lex_ord(oa)), ord_opt(lex_ord(ord_keyed(oa))));
    lemma_strict_pair(ok, o1);
    lemma_strict_pair(os, ord_pair(ok, o1));
    lemma_strict_pair(os, ord_pair(os, ord_pair(ok, o1)));
    lemma_strict_pair(ok, ord_pair(os, ord_pair(os, ord_pair(ok, o1))));
    lemma_strict_pair(o1, ord_pair(ok, ord_pair(os, ord_pair(os, ord_pair(ok, o1)))));
    lemma_strict_pair(os, ord_pair(o1, ord_pair(ok, ord_pair(os, ord_pair(os, ord_pair(ok, o1))))));
    let o = ord_pair(os, ord_pair(o1, ord_pair(ok, ord_pair(os, ord_pair(os, ord_pair(ok, o1))))));
    let key = |x: PropsV<A>|
        (
            x.labels,
            (x.vertex, (x.vertices, (x.source_fns, (x.sink_fns, (x.tainted_formals, x.import))))),
        );
    lemma_pull(o, key, ord_props(oa));
}

pub proof fn lemma_ord_location<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_location(oa)),
        equal_means_eq(ord_location(oa)),
{
    lemma_ord_physical(oa);
    lemma_strict_opt(ord_physical(oa));
    lemma_strict_lex(oa);
    lemma_strict_pair(ord_opt(ord_physical(oa)), lex_ord(oa));
    let key = |x: CanonLocationV<A>| (x.physical, x.logical);
    lemma_pull(ord_pair(ord_opt(ord_physical(oa)), lex_ord(oa)), key, ord_location(oa));
}

pub proof fn lemma_ord_step<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_step(oa)),
        equal_means_eq(ord_step(oa)),
{
    lemma_ord_location(oa);
    lemma_strict_opt(ord_location(oa));
    lemma_strict_lex(oa);
    lemma_strict_pair(lex_ord(oa), ord_opt(ord_location(oa)));
    let key = |x: CanonStepV<A>| (x.kinds, x.location);
    lemma_pull(ord_pair(lex_ord(oa), ord_opt(ord_location(oa))), key, ord_step(oa));
}

pub proof fn lemma_ord_thread_flow<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_thread_flow(oa)),
        equal_means_eq(ord_thread_flow(oa)),
{
    // `Steps` before `Endpoints`: the key's first component is `None` for `Steps`.
    let st = ord_step(oa);
    lemma_ord_step(oa);
    lemma_strict_pair(st, st);
    lemma_strict_opt(ord_pair(st, st));
    lemma_strict_lex(st);
    lemma_strict_opt(lex_ord(st));
    lemma_strict_pair(ord_opt(ord_pair(st, st)), ord_opt(lex_ord(st)));
    let o = ord_pair(ord_opt(ord_pair(st, st)), ord_opt(lex_ord(st)));
    let key = |x: CanonThreadFlowV<A>|
        match x {
            CanonThreadFlowV::Steps(s) => (None, Some(s)),
            CanonThreadFlowV::Endpoints(a, b) => (Some((a, b)), None),
        };
    assert forall|x: CanonThreadFlowV<A>, y: CanonThreadFlowV<A>| #[trigger]
        ord_thread_flow(oa)(x, y) == o(key(x), key(y)) by {
        if let (CanonThreadFlowV::Steps(s), CanonThreadFlowV::Steps(t)) = (x, y) {
            assert(o(key(x), key(y)) == then(Ordering::Equal, lex(st, s, t)));
        }
    }
    lemma_pull(o, key, ord_thread_flow(oa));
}

pub proof fn lemma_ord_code_flow<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_code_flow(oa)),
        equal_means_eq(ord_code_flow(oa)),
{
    lemma_ord_thread_flow(oa);
    lemma_strict_lex(ord_thread_flow(oa));
    let key = |x: CanonCodeFlowV<A>| x.thread_flows;
    lemma_pull(lex_ord(ord_thread_flow(oa)), key, ord_code_flow(oa));
}

pub proof fn lemma_ord_result<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_result(oa)),
        equal_means_eq(ord_result(oa)),
{
    lemma_ord_location(oa);
    lemma_ord_code_flow(oa);
    lemma_ord_props(oa);
    lemma_strict_opt(oa);
    lemma_strict_lex(ord_location(oa));
    lemma_strict_lex(ord_code_flow(oa));
    let (ol, oc, op) = (lex_ord(ord_location(oa)), lex_ord(ord_code_flow(oa)), ord_props(oa));
    lemma_strict_pair(oc, op);
    lemma_strict_pair(ol, ord_pair(oc, op));
    lemma_strict_pair(oa, ord_pair(ol, ord_pair(oc, op)));
    lemma_strict_pair(oa, ord_pair(oa, ord_pair(ol, ord_pair(oc, op))));
    lemma_strict_pair(ord_opt(oa), ord_pair(oa, ord_pair(oa, ord_pair(ol, ord_pair(oc, op)))));
    let o = ord_pair(ord_opt(oa), ord_pair(oa, ord_pair(oa, ord_pair(ol, ord_pair(oc, op)))));
    let key = |x: CanonResultV<A>|
        (x.rule, (x.kind, (x.level, (x.locations, (x.code_flows, x.props)))));
    lemma_pull(o, key, ord_result(oa));
}

/// The order on results as the code holds them: total, and `Equal` exactly when the views are
/// equal (two `Vec`s with equal contents are not known to be `==`).
pub proof fn lemma_ord_result_exec<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_result_exec(oa)),
        forall|x: CanonResult<A>, y: CanonResult<A>|
            #[trigger] ord_result_exec(oa)(x, y) == Ordering::Equal <==> x@ == y@,
{
    lemma_ord_result(oa);
    lemma_pull(ord_result(oa), |c: CanonResult<A>| c@, ord_result_exec(oa));
}

// ---------------------------------------------------------------------------
// The comparisons, each given a `cmp` that computes the atom order `oa`.

/// One step of a lexicographic comparison: `lex` of the suffixes from `i`.
proof fn lemma_lex_skip<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>, i: int)
    requires
        0 <= i <= s.len(),
        i <= t.len(),
    ensures
        lex(ord, s, t) == lex(ord, s.skip(0), t.skip(0)),
        lex(ord, s.skip(i), t.skip(i)) == if i == s.len() {
            if i == t.len() {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        } else if i == t.len() {
            Ordering::Greater
        } else if ord(s[i], t[i]) == Ordering::Equal {
            lex(ord, s.skip(i + 1), t.skip(i + 1))
        } else {
            ord(s[i], t[i])
        },
{
    assert(s.skip(0) =~= s);
    assert(t.skip(0) =~= t);
    if i < s.len() && i < t.len() {
        assert(s.skip(i)[0] == s[i]);
        assert(t.skip(i)[0] == t[i]);
        assert(s.skip(i).drop_first() =~= s.skip(i + 1));
        assert(t.skip(i).drop_first() =~= t.skip(i + 1));
    }
}

fn is_equal(c: Ordering) -> (b: bool)
    ensures
        b == (c == Ordering::Equal),
{
    matches!(c, Ordering::Equal)
}

fn cmp_opt_i64(x: &Option<i64>, y: &Option<i64>) -> (o: Ordering)
    ensures
        o == ord_opt_i64(*x, *y),
{
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => {
            if *a < *b {
                Ordering::Less
            } else if *a > *b {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        },
    }
}

fn cmp_opt_atom<A, F: Fn(&A, &A) -> Ordering>(
    x: &Option<A>,
    y: &Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_opt_atom(oa, *x, *y),
{
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp(a, b),
    }
}

fn cmp_atoms<A, F: Fn(&A, &A) -> Ordering>(
    x: &[A],
    y: &[A],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == lex(oa, x@, y@),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(oa, x@, y@, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            lex(oa, x@, y@) == lex(oa, x@.skip(i as int), y@.skip(i as int)),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(oa, x@, y@, i as int);
        }
        let c = cmp(&x[i], &y[i]);
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(oa, x@, y@, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn cmp_opt_atoms<A, F: Fn(&A, &A) -> Ordering>(
    x: &Option<Vec<A>>,
    y: &Option<Vec<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_opt_atoms(oa, opt_seq(*x), opt_seq(*y)),
{
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_atoms(a, b, cmp, Ghost(oa)),
    }
}

/// `lex` over `Vec`s of keyed entries, given their views `xs`, `ys`.
fn cmp_keyeds<A, F: Fn(&A, &A) -> Ordering>(
    x: &[Keyed<A>],
    y: &[Keyed<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
    Ghost(xs): Ghost<Seq<KeyedV<A>>>,
    Ghost(ys): Ghost<Seq<KeyedV<A>>>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
        xs.len() == x@.len(),
        ys.len() == y@.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
        forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
    ensures
        o == lex(ord_keyed(oa), xs, ys),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(ord_keyed(oa), xs, ys, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            xs.len() == x@.len(),
            ys.len() == y@.len(),
            forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
            forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
            lex(ord_keyed(oa), xs, ys) == lex(ord_keyed(oa), xs.skip(i as int), ys.skip(i as int)),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(ord_keyed(oa), xs, ys, i as int);
        }
        let c = cmp_keyed(&x[i], &y[i], cmp, Ghost(oa));
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(ord_keyed(oa), xs, ys, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn cmp_opt_keyeds<A, F: Fn(&A, &A) -> Ordering>(
    x: &Option<Vec<Keyed<A>>>,
    y: &Option<Vec<Keyed<A>>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_opt_keyeds(oa, opt_keyed(*x), opt_keyed(*y)),
{
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_keyeds(
            a,
            b,
            cmp,
            Ghost(oa),
            Ghost(opt_keyed(*x)->Some_0),
            Ghost(opt_keyed(*y)->Some_0),
        ),
    }
}

/// `lex` over `Vec`s of locations, given their views `xs`, `ys`.
fn cmp_locations<A, F: Fn(&A, &A) -> Ordering>(
    x: &[CanonLocation<A>],
    y: &[CanonLocation<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
    Ghost(xs): Ghost<Seq<CanonLocationV<A>>>,
    Ghost(ys): Ghost<Seq<CanonLocationV<A>>>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
        xs.len() == x@.len(),
        ys.len() == y@.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
        forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
    ensures
        o == lex(ord_location(oa), xs, ys),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(ord_location(oa), xs, ys, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            xs.len() == x@.len(),
            ys.len() == y@.len(),
            forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
            forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
            lex(ord_location(oa), xs, ys) == lex(
                ord_location(oa),
                xs.skip(i as int),
                ys.skip(i as int),
            ),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(ord_location(oa), xs, ys, i as int);
        }
        let c = cmp_location(&x[i], &y[i], cmp, Ghost(oa));
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(ord_location(oa), xs, ys, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

/// `lex` over `Vec`s of steps, given their views `xs`, `ys`.
fn cmp_steps<A, F: Fn(&A, &A) -> Ordering>(
    x: &[CanonStep<A>],
    y: &[CanonStep<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
    Ghost(xs): Ghost<Seq<CanonStepV<A>>>,
    Ghost(ys): Ghost<Seq<CanonStepV<A>>>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
        xs.len() == x@.len(),
        ys.len() == y@.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
        forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
    ensures
        o == lex(ord_step(oa), xs, ys),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(ord_step(oa), xs, ys, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            xs.len() == x@.len(),
            ys.len() == y@.len(),
            forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
            forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
            lex(ord_step(oa), xs, ys) == lex(ord_step(oa), xs.skip(i as int), ys.skip(i as int)),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(ord_step(oa), xs, ys, i as int);
        }
        let c = cmp_step(&x[i], &y[i], cmp, Ghost(oa));
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(ord_step(oa), xs, ys, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

/// `lex` over `Vec`s of thread flows, given their views `xs`, `ys`.
fn cmp_thread_flows<A, F: Fn(&A, &A) -> Ordering>(
    x: &[CanonThreadFlow<A>],
    y: &[CanonThreadFlow<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
    Ghost(xs): Ghost<Seq<CanonThreadFlowV<A>>>,
    Ghost(ys): Ghost<Seq<CanonThreadFlowV<A>>>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
        xs.len() == x@.len(),
        ys.len() == y@.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
        forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
    ensures
        o == lex(ord_thread_flow(oa), xs, ys),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(ord_thread_flow(oa), xs, ys, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            xs.len() == x@.len(),
            ys.len() == y@.len(),
            forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
            forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
            lex(ord_thread_flow(oa), xs, ys) == lex(
                ord_thread_flow(oa),
                xs.skip(i as int),
                ys.skip(i as int),
            ),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(ord_thread_flow(oa), xs, ys, i as int);
        }
        let c = cmp_thread_flow(&x[i], &y[i], cmp, Ghost(oa));
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(ord_thread_flow(oa), xs, ys, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

/// `lex` over `Vec`s of code flows, given their views `xs`, `ys`.
fn cmp_code_flows<A, F: Fn(&A, &A) -> Ordering>(
    x: &[CanonCodeFlow<A>],
    y: &[CanonCodeFlow<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
    Ghost(xs): Ghost<Seq<CanonCodeFlowV<A>>>,
    Ghost(ys): Ghost<Seq<CanonCodeFlowV<A>>>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
        xs.len() == x@.len(),
        ys.len() == y@.len(),
        forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
        forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
    ensures
        o == lex(ord_code_flow(oa), xs, ys),
{
    let mut i: usize = 0;
    proof {
        lemma_lex_skip(ord_code_flow(oa), xs, ys, 0);
    }
    while i < x.len() && i < y.len()
        invariant
            i <= x.len(),
            i <= y.len(),
            computes(*cmp, oa),
            xs.len() == x@.len(),
            ys.len() == y@.len(),
            forall|k: int| 0 <= k < xs.len() ==> #[trigger] xs[k] == x@[k]@,
            forall|k: int| 0 <= k < ys.len() ==> #[trigger] ys[k] == y@[k]@,
            lex(ord_code_flow(oa), xs, ys) == lex(
                ord_code_flow(oa),
                xs.skip(i as int),
                ys.skip(i as int),
            ),
        decreases x.len() - i,
    {
        proof {
            lemma_lex_skip(ord_code_flow(oa), xs, ys, i as int);
        }
        let c = cmp_code_flow(&x[i], &y[i], cmp, Ghost(oa));
        if !is_equal(c) {
            return c;
        }
        i += 1;
    }
    proof {
        lemma_lex_skip(ord_code_flow(oa), xs, ys, i as int);
    }
    if i < x.len() {
        Ordering::Greater
    } else if i < y.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

pub fn cmp_region(x: &Region, y: &Region) -> (o: Ordering)
    ensures
        o == ord_region()(*x, *y),
{
    let c = cmp_opt_i64(&x.start_line, &y.start_line);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_i64(&x.start_column, &y.start_column);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_i64(&x.end_line, &y.end_line);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_i64(&x.end_column, &y.end_column);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_i64(&x.byte_offset, &y.byte_offset);
    if !is_equal(c) {
        return c;
    }
    cmp_opt_i64(&x.byte_length, &y.byte_length)
}

pub fn cmp_address<A, F: Fn(&A, &A) -> Ordering>(
    x: &Address<A>,
    y: &Address<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_address(oa)(*x, *y),
{
    let c = cmp_opt_i64(&x.absolute, &y.absolute);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_i64(&x.relative, &y.relative);
    if !is_equal(c) {
        return c;
    }
    cmp_opt_atom(&x.kind, &y.kind, cmp, Ghost(oa))
}

pub fn cmp_physical<A, F: Fn(&A, &A) -> Ordering>(
    x: &Physical<A>,
    y: &Physical<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_physical(oa)(*x, *y),
{
    let c = cmp_opt_atom(&x.base, &y.base, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_atom(&x.uri, &y.uri, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_region(&x.region, &y.region);
    if !is_equal(c) {
        return c;
    }
    match (&x.address, &y.address) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_address(a, b, cmp, Ghost(oa)),
    }
}

pub fn cmp_keyed<A, F: Fn(&A, &A) -> Ordering>(
    x: &Keyed<A>,
    y: &Keyed<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_keyed(oa)(x@, y@),
{
    let c = cmp(&x.key, &y.key);
    if !is_equal(c) {
        return c;
    }
    cmp_atoms(&x.values, &y.values, cmp, Ghost(oa))
}

pub fn cmp_props<A, F: Fn(&A, &A) -> Ordering>(
    x: &Props<A>,
    y: &Props<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_props(oa)(x@, y@),
{
    let c = cmp_opt_atoms(&x.labels, &y.labels, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_atom(&x.vertex, &y.vertex, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_keyeds(&x.vertices, &y.vertices, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_atoms(&x.source_fns, &y.source_fns, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_atoms(&x.sink_fns, &y.sink_fns, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp_opt_keyeds(&x.tainted_formals, &y.tainted_formals, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    cmp_opt_atom(&x.import, &y.import, cmp, Ghost(oa))
}

pub fn cmp_location<A, F: Fn(&A, &A) -> Ordering>(
    x: &CanonLocation<A>,
    y: &CanonLocation<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_location(oa)(x@, y@),
{
    let c = match (&x.physical, &y.physical) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_physical(a, b, cmp, Ghost(oa)),
    };
    if !is_equal(c) {
        return c;
    }
    cmp_atoms(&x.logical, &y.logical, cmp, Ghost(oa))
}

pub fn cmp_step<A, F: Fn(&A, &A) -> Ordering>(
    x: &CanonStep<A>,
    y: &CanonStep<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_step(oa)(x@, y@),
{
    let c = cmp_atoms(&x.kinds, &y.kinds, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    match (&x.location, &y.location) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_location(a, b, cmp, Ghost(oa)),
    }
}

pub fn cmp_thread_flow<A, F: Fn(&A, &A) -> Ordering>(
    x: &CanonThreadFlow<A>,
    y: &CanonThreadFlow<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_thread_flow(oa)(x@, y@),
{
    match (x, y) {
        (CanonThreadFlow::Steps(s), CanonThreadFlow::Steps(t)) => cmp_steps(
            s,
            t,
            cmp,
            Ghost(oa),
            Ghost(x@->Steps_0),
            Ghost(y@->Steps_0),
        ),
        (CanonThreadFlow::Steps(_), CanonThreadFlow::Endpoints(_, _)) => Ordering::Less,
        (CanonThreadFlow::Endpoints(_, _), CanonThreadFlow::Steps(_)) => Ordering::Greater,
        (CanonThreadFlow::Endpoints(a, b), CanonThreadFlow::Endpoints(c, d)) => {
            let first = cmp_step(a, c, cmp, Ghost(oa));
            if let Ordering::Equal = first {
                cmp_step(b, d, cmp, Ghost(oa))
            } else {
                first
            }
        },
    }
}

pub fn cmp_code_flow<A, F: Fn(&A, &A) -> Ordering>(
    x: &CanonCodeFlow<A>,
    y: &CanonCodeFlow<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_code_flow(oa)(x@, y@),
{
    cmp_thread_flows(
        &x.thread_flows,
        &y.thread_flows,
        cmp,
        Ghost(oa),
        Ghost(x@.thread_flows),
        Ghost(y@.thread_flows),
    )
}

pub fn cmp_result<A, F: Fn(&A, &A) -> Ordering>(
    x: &CanonResult<A>,
    y: &CanonResult<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (o: Ordering)
    requires
        computes(*cmp, oa),
    ensures
        o == ord_result(oa)(x@, y@),
{
    let c = cmp_opt_atom(&x.rule, &y.rule, cmp, Ghost(oa));
    if !is_equal(c) {
        return c;
    }
    let c = cmp(&x.kind, &y.kind);
    if !is_equal(c) {
        return c;
    }
    let c = cmp(&x.level, &y.level);
    if !is_equal(c) {
        return c;
    }
    let c = cmp_locations(
        &x.locations,
        &y.locations,
        cmp,
        Ghost(oa),
        Ghost(x@.locations),
        Ghost(y@.locations),
    );
    if !is_equal(c) {
        return c;
    }
    let c = cmp_code_flows(
        &x.code_flows,
        &y.code_flows,
        cmp,
        Ghost(oa),
        Ghost(x@.code_flows),
        Ghost(y@.code_flows),
    );
    if !is_equal(c) {
        return c;
    }
    cmp_props(&x.props, &y.props, cmp, Ghost(oa))
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ghost;

    // Mirrors of the canonical types whose derived `Ord` is the documented order: fields in
    // declaration order, `None` first, sequences lexicographic with a proper prefix first,
    // `Steps` before `Endpoints`.
    type MRegion = [Option<i64>; 6];
    type MAddress = (Option<i64>, Option<i64>, Option<u32>);
    type MPhysical = (Option<u32>, Option<u32>, MRegion, Option<MAddress>);
    type MKeyed = (u32, Vec<u32>);
    type MProps = (
        Option<Vec<u32>>,
        Option<u32>,
        Option<Vec<MKeyed>>,
        Option<Vec<u32>>,
        Option<Vec<u32>>,
        Option<Vec<MKeyed>>,
        Option<u32>,
    );
    type MLocation = (Option<MPhysical>, Vec<u32>);
    type MStep = (Vec<u32>, Option<MLocation>);

    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum MThreadFlow {
        Steps(Vec<MStep>),
        Endpoints(MStep, MStep),
    }

    type MResult = (
        Option<u32>,
        u32,
        u32,
        Vec<MLocation>,
        Vec<Vec<MThreadFlow>>,
        MProps,
    );

    fn m_region(r: &Region) -> MRegion {
        [
            r.start_line,
            r.start_column,
            r.end_line,
            r.end_column,
            r.byte_offset,
            r.byte_length,
        ]
    }

    fn m_keyed(k: &Keyed<u32>) -> MKeyed {
        (k.key, k.values.clone())
    }

    fn m_keyeds(v: &Option<Vec<Keyed<u32>>>) -> Option<Vec<MKeyed>> {
        v.as_ref().map(|v| v.iter().map(m_keyed).collect())
    }

    fn m_location(l: &CanonLocation<u32>) -> MLocation {
        let physical = l.physical.as_ref().map(|p| {
            let address = p.address.as_ref().map(|a| (a.absolute, a.relative, a.kind));
            (p.base, p.uri, m_region(&p.region), address)
        });
        (physical, l.logical.clone())
    }

    fn m_step(s: &CanonStep<u32>) -> MStep {
        (s.kinds.clone(), s.location.as_ref().map(m_location))
    }

    fn m_result(r: &CanonResult<u32>) -> MResult {
        let flows = r
            .code_flows
            .iter()
            .map(|c| {
                c.thread_flows
                    .iter()
                    .map(|t| match t {
                        CanonThreadFlow::Steps(s) => {
                            MThreadFlow::Steps(s.iter().map(m_step).collect())
                        }
                        CanonThreadFlow::Endpoints(a, b) => {
                            MThreadFlow::Endpoints(m_step(a), m_step(b))
                        }
                    })
                    .collect()
            })
            .collect();
        let p = &r.props;
        (
            r.rule,
            r.kind,
            r.level,
            r.locations.iter().map(m_location).collect(),
            flows,
            (
                p.labels.clone(),
                p.vertex,
                m_keyeds(&p.vertices),
                p.source_fns.clone(),
                p.sink_fns.clone(),
                m_keyeds(&p.tainted_formals),
                p.import,
            ),
        )
    }

    /// A deterministic generator of small values, small enough that two draws are often equal
    /// or nearly so.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self, n: u64) -> u64 {
            // xorshift64
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }

        fn atom(&mut self) -> u32 {
            self.next(2) as u32
        }

        fn opt<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
            if self.next(3) == 0 {
                None
            } else {
                Some(f(self))
            }
        }

        fn vec<T>(&mut self, max: u64, mut f: impl FnMut(&mut Self) -> T) -> Vec<T> {
            let n = self.next(max + 1);
            (0..n).map(|_| f(self)).collect()
        }

        fn int(&mut self) -> Option<i64> {
            self.opt(|g| g.next(3) as i64 - 1)
        }

        fn atoms(&mut self) -> Vec<u32> {
            self.vec(2, Self::atom)
        }

        fn region(&mut self) -> Region {
            Region {
                start_line: self.int(),
                start_column: self.int(),
                end_line: self.int(),
                end_column: self.int(),
                byte_offset: self.int(),
                byte_length: self.int(),
            }
        }

        fn location(&mut self) -> CanonLocation<u32> {
            let physical = self.opt(|g| Physical {
                base: g.opt(Self::atom),
                uri: g.opt(Self::atom),
                region: g.region(),
                address: g.opt(|g| Address {
                    absolute: g.int(),
                    relative: g.int(),
                    kind: g.opt(Self::atom),
                }),
            });
            CanonLocation {
                physical,
                logical: self.atoms(),
            }
        }

        fn step(&mut self) -> CanonStep<u32> {
            CanonStep {
                kinds: self.atoms(),
                location: self.opt(Self::location),
            }
        }

        fn thread_flow(&mut self) -> CanonThreadFlow<u32> {
            if self.next(2) == 0 {
                CanonThreadFlow::Steps(self.vec(2, Self::step))
            } else {
                CanonThreadFlow::Endpoints(self.step(), self.step())
            }
        }

        fn keyeds(&mut self) -> Option<Vec<Keyed<u32>>> {
            self.opt(|g| {
                g.vec(2, |g| Keyed {
                    key: g.atom(),
                    values: g.atoms(),
                })
            })
        }

        fn result(&mut self) -> CanonResult<u32> {
            CanonResult {
                rule: self.opt(Self::atom),
                kind: self.atom(),
                level: self.atom(),
                locations: self.vec(2, Self::location),
                code_flows: self.vec(2, |g| CanonCodeFlow {
                    thread_flows: g.vec(2, Self::thread_flow),
                }),
                props: Props {
                    labels: self.opt(Self::atoms),
                    vertex: self.opt(Self::atom),
                    vertices: self.keyeds(),
                    source_fns: self.opt(Self::atoms),
                    sink_fns: self.opt(Self::atoms),
                    tainted_formals: self.keyeds(),
                    import: self.opt(Self::atom),
                },
            }
        }
    }

    fn cmp(x: &CanonResult<u32>, y: &CanonResult<u32>) -> Ordering {
        cmp_result(x, y, &|a: &u32, b: &u32| a.cmp(b), ghost())
    }

    /// `cmp_result` agrees with the mirror's derived `Ord`, is antisymmetric, and is `Equal`
    /// exactly on equal results, on pairs drawn independently, from one seed (equal), and from
    /// seeds that share all but the last draws (nearly equal).
    #[test]
    fn cmp_result_matches_derived_ord() {
        let mut g = Gen(0x9e37_79b9_7f4a_7c15);
        let mut seen = [0usize; 3];
        for round in 0..600 {
            let x = g.result();
            let y = match round % 3 {
                0 => g.result(),
                1 => x.clone(),
                _ => {
                    // Change one top-level field of a copy.
                    let mut y = x.clone();
                    match g.next(6) {
                        0 => y.rule = g.opt(Gen::atom),
                        1 => y.kind = g.atom(),
                        2 => y.level = g.atom(),
                        3 => y.locations = g.vec(2, Gen::location),
                        4 => y.props.vertices = g.keyeds(),
                        _ => y.props.labels = g.opt(Gen::atoms),
                    }
                    y
                }
            };
            let o = cmp(&x, &y);
            assert_eq!(o, m_result(&x).cmp(&m_result(&y)));
            assert_eq!(cmp(&y, &x), o.reverse());
            assert_eq!(o == Ordering::Equal, x == y);
            seen[(o as i8 + 1) as usize] += 1;
        }
        // Each outcome occurs, so no case is vacuous.
        assert!(seen.iter().all(|&n| n > 0), "{seen:?}");
    }
}
