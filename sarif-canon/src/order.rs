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
use multiset_diff::{computes, total_order};

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

/// The lexicographic order `ord` induces: element by element, a proper prefix first.
pub open spec fn lex<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>) -> Ordering
    decreases s.len(),
{
    if s.len() == 0 {
        if t.len() == 0 {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else if t.len() == 0 {
        Ordering::Greater
    } else if ord(s[0], t[0]) == Ordering::Equal {
        lex(ord, s.drop_first(), t.drop_first())
    } else {
        ord(s[0], t[0])
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
// Each order is a total order whose `Equal` is equality, given that the atom order is.

pub proof fn lemma_ord_region()
    ensures
        total_order(ord_region()),
        equal_means_eq(ord_region()),
{
    admit();
}

pub proof fn lemma_ord_address<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_address(oa)),
        equal_means_eq(ord_address(oa)),
{
    admit();
}

pub proof fn lemma_ord_physical<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_physical(oa)),
        equal_means_eq(ord_physical(oa)),
{
    admit();
}

pub proof fn lemma_ord_keyed<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_keyed(oa)),
        equal_means_eq(ord_keyed(oa)),
{
    admit();
}

pub proof fn lemma_ord_props<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_props(oa)),
        equal_means_eq(ord_props(oa)),
{
    admit();
}

pub proof fn lemma_ord_location<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_location(oa)),
        equal_means_eq(ord_location(oa)),
{
    admit();
}

pub proof fn lemma_ord_step<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_step(oa)),
        equal_means_eq(ord_step(oa)),
{
    admit();
}

pub proof fn lemma_ord_thread_flow<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_thread_flow(oa)),
        equal_means_eq(ord_thread_flow(oa)),
{
    admit();
}

pub proof fn lemma_ord_code_flow<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_code_flow(oa)),
        equal_means_eq(ord_code_flow(oa)),
{
    admit();
}

pub proof fn lemma_ord_result<A>(oa: spec_fn(A, A) -> Ordering)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        total_order(ord_result(oa)),
        equal_means_eq(ord_result(oa)),
{
    admit();
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
    admit();
}

// ---------------------------------------------------------------------------
// The comparisons, each given a `cmp` that computes the atom order `oa`.

#[verifier::external_body]
#[allow(unused_variables)]
pub fn cmp_region(x: &Region, y: &Region) -> (o: Ordering)
    ensures
        o == ord_region()(*x, *y),
{
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

} // verus!
