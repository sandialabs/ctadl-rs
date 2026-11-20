//! The canonical form of a run's results.
//!
//! `spec_canon_runs` defines it, element by element: references resolve to the `fqn` of the
//! table entry they name; bags (results, locations, code flows, thread flows, logical
//! locations, kinds and the set- and map-valued properties) are sorted; a thread flow's steps
//! keep their order, after a check that their `executionOrder` values increase.
//!
//! With an endpoints rule, a result of that rule is compared by its rule, kind, level, labels,
//! source and sink functions, and the first and last step of each thread flow: its locations,
//! interior steps and other properties are dropped. Its full form is still built first, so an
//! input that cannot be canonicalized is an error with or without the endpoints rule.
//!
//! The errors, the first in the order the form is built (results in order; within a result,
//! its locations, then its code flows; within a thread flow, the empty check, the
//! `executionOrder` check, then its steps): a reference past the end of the table
//! (`OutOfRange`); a thread flow with no steps, which SARIF does not allow (`EmptyFlow`); and
//! `executionOrder` values that do not increase (`NotIncreasing`). The endpoints rule changes
//! what is compared, never whether an input is accepted.

use crate::types::*;
use std::cmp::Ordering;
use vstd::prelude::*;

use crate::order::*;
use crate::prims::*;
#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, le, sorted, total_order};

verus! {

// ---------------------------------------------------------------------------
// The full canonical form of each element, `t` the run's `logicalLocations` table.

/// A logical location: the `fqn` of the entry an index names, or an inline entry's name.
pub open spec fn spec_canon_ref<A>(t: Seq<RawLogical<A>>, r: RawLogicalRef<A>) -> Result<A, Error> {
    match r {
        RawLogicalRef::Index(i) => match spec_resolve(t, i as int) {
            Ok(l) => Ok(l.fqn),
            Err(e) => Err(e),
        },
        RawLogicalRef::Inline(a) => Ok(a),
    }
}

pub open spec fn spec_canon_location<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    l: RawLocationV<A>,
) -> Result<CanonLocationV<A>, Error> {
    match spec_all_ok(spec_canon_seq(l.logical, |r: RawLogicalRef<A>| spec_canon_ref(t, r))) {
        Ok(names) => Ok(CanonLocationV { physical: l.physical, logical: spec_canon_bag(oa, names) }),
        Err(e) => Err(e),
    }
}

pub open spec fn spec_canon_step<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    s: RawStepV<A>,
) -> Result<CanonStepV<A>, Error> {
    match s.location {
        None => Ok(CanonStepV { kinds: spec_canon_bag(oa, s.kinds), location: None }),
        Some(l) => match spec_canon_location(oa, t, l) {
            Ok(c) => Ok(CanonStepV { kinds: spec_canon_bag(oa, s.kinds), location: Some(c) }),
            Err(e) => Err(e),
        },
    }
}

pub open spec fn spec_canon_thread_flow<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    f: RawThreadFlowV<A>,
) -> Result<CanonThreadFlowV<A>, Error> {
    if f.steps.len() == 0 {
        Err(Error::EmptyFlow)
    } else if !spec_increasing(f.steps.map_values(|s: RawStepV<A>| step_order(s))) {
        Err(Error::NotIncreasing)
    } else {
        match spec_all_ok(spec_canon_seq(f.steps, |s: RawStepV<A>| spec_canon_step(oa, t, s))) {
            Ok(steps) => Ok(CanonThreadFlowV::Steps(steps)),
            Err(e) => Err(e),
        }
    }
}

pub open spec fn spec_canon_code_flow<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    c: RawCodeFlowV<A>,
) -> Result<CanonCodeFlowV<A>, Error> {
    match spec_all_ok(
        spec_canon_seq(c.thread_flows, |f: RawThreadFlowV<A>| spec_canon_thread_flow(oa, t, f)),
    ) {
        Ok(flows) => Ok(CanonCodeFlowV { thread_flows: spec_canon_bag(ord_thread_flow(oa), flows) }),
        Err(e) => Err(e),
    }
}

pub open spec fn spec_canon_atoms<A>(oa: spec_fn(A, A) -> Ordering, s: Option<Seq<A>>) -> Option<
    Seq<A>,
> {
    match s {
        Some(s) => Some(spec_canon_bag(oa, s)),
        None => None,
    }
}

pub open spec fn spec_canon_keyed<A>(oa: spec_fn(A, A) -> Ordering, k: KeyedV<A>) -> KeyedV<A> {
    KeyedV { key: k.key, values: spec_canon_bag(oa, k.values) }
}

pub open spec fn spec_canon_keyeds<A>(
    oa: spec_fn(A, A) -> Ordering,
    s: Option<Seq<KeyedV<A>>>,
) -> Option<Seq<KeyedV<A>>> {
    match s {
        Some(s) => Some(
            spec_canon_bag(ord_keyed(oa), spec_canon_seq(s, |k: KeyedV<A>| spec_canon_keyed(oa, k))),
        ),
        None => None,
    }
}

pub open spec fn spec_canon_props<A>(oa: spec_fn(A, A) -> Ordering, p: PropsV<A>) -> PropsV<A> {
    PropsV {
        labels: spec_canon_atoms(oa, p.labels),
        vertex: p.vertex,
        vertices: spec_canon_keyeds(oa, p.vertices),
        source_fns: spec_canon_atoms(oa, p.source_fns),
        sink_fns: spec_canon_atoms(oa, p.sink_fns),
        tainted_formals: spec_canon_keyeds(oa, p.tainted_formals),
        import: p.import,
    }
}

/// A result's full canonical form, whatever its rule.
pub open spec fn spec_canon_result_full<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    r: RawResultV<A>,
) -> Result<CanonResultV<A>, Error> {
    match spec_all_ok(spec_canon_seq(r.locations, |l: RawLocationV<A>| spec_canon_location(oa, t, l))) {
        Err(e) => Err(e),
        Ok(locations) => match spec_all_ok(
            spec_canon_seq(r.code_flows, |c: RawCodeFlowV<A>| spec_canon_code_flow(oa, t, c)),
        ) {
            Err(e) => Err(e),
            Ok(code_flows) => Ok(
                CanonResultV {
                    rule: r.rule,
                    kind: r.kind,
                    level: r.level,
                    locations: spec_canon_bag(ord_location(oa), locations),
                    code_flows: spec_canon_bag(ord_code_flow(oa), code_flows),
                    props: spec_canon_props(oa, r.props),
                },
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// Cutting a full canonical result down to what the endpoints rule compares.

pub open spec fn spec_endpoints_flow<A>(f: CanonThreadFlowV<A>) -> Result<CanonThreadFlowV<A>, Error> {
    match f {
        CanonThreadFlowV::Steps(s) => match spec_endpoints(s) {
            Ok((first, last)) => Ok(CanonThreadFlowV::Endpoints(first, last)),
            Err(e) => Err(e),
        },
        CanonThreadFlowV::Endpoints(first, last) => Ok(CanonThreadFlowV::Endpoints(first, last)),
    }
}

pub open spec fn spec_endpoints_code_flow<A>(
    oa: spec_fn(A, A) -> Ordering,
    c: CanonCodeFlowV<A>,
) -> Result<CanonCodeFlowV<A>, Error> {
    match spec_all_ok(spec_canon_seq(c.thread_flows, |f: CanonThreadFlowV<A>| spec_endpoints_flow(f))) {
        Ok(flows) => Ok(CanonCodeFlowV { thread_flows: spec_canon_bag(ord_thread_flow(oa), flows) }),
        Err(e) => Err(e),
    }
}

/// Keeps the rule, kind, level, labels, source and sink functions, and each thread flow's
/// endpoints; drops the locations, `taintVertex`, `taintVertices`, `taintedFormals` and
/// `import`. The bags are sorted again, since their elements changed.
pub open spec fn spec_endpoints_result<A>(
    oa: spec_fn(A, A) -> Ordering,
    c: CanonResultV<A>,
) -> Result<CanonResultV<A>, Error> {
    match spec_all_ok(
        spec_canon_seq(c.code_flows, |f: CanonCodeFlowV<A>| spec_endpoints_code_flow(oa, f)),
    ) {
        Ok(code_flows) => Ok(
            CanonResultV {
                rule: c.rule,
                kind: c.kind,
                level: c.level,
                locations: Seq::empty(),
                code_flows: spec_canon_bag(ord_code_flow(oa), code_flows),
                props: PropsV {
                    labels: c.props.labels,
                    vertex: None,
                    vertices: None,
                    source_fns: c.props.source_fns,
                    sink_fns: c.props.sink_fns,
                    tainted_formals: None,
                    import: None,
                },
            },
        ),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Results and runs.

/// A result's canonical form: the full form, cut to its endpoints when its rule is
/// `endpoints_rule`.
pub open spec fn spec_canon_result<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    endpoints_rule: Option<A>,
    r: RawResultV<A>,
) -> Result<CanonResultV<A>, Error> {
    match spec_canon_result_full(oa, t, r) {
        Err(e) => Err(e),
        Ok(c) => if endpoints_rule is Some && r.rule == endpoints_rule {
            spec_endpoints_result(oa, c)
        } else {
            Ok(c)
        },
    }
}

/// A run's canonical results in file order, each resolved against the run's own table.
pub open spec fn spec_run_results<A>(
    oa: spec_fn(A, A) -> Ordering,
    run: RawRunV<A>,
    endpoints_rule: Option<A>,
) -> Result<Seq<CanonResultV<A>>, Error> {
    spec_all_ok(
        spec_canon_seq(
            run.results,
            |r: RawResultV<A>| spec_canon_result(oa, run.logical, endpoints_rule, r),
        ),
    )
}

/// Every run's canonical results, run after run, or the first error.
pub open spec fn spec_pool<A>(
    oa: spec_fn(A, A) -> Ordering,
    runs: Seq<RawRunV<A>>,
    endpoints_rule: Option<A>,
) -> Result<Seq<CanonResultV<A>>, Error>
    decreases runs.len(),
{
    if runs.len() == 0 {
        Ok(Seq::empty())
    } else {
        match spec_run_results(oa, runs[0], endpoints_rule) {
            Err(e) => Err(e),
            Ok(first) => match spec_pool(oa, runs.drop_first(), endpoints_rule) {
                Err(e) => Err(e),
                Ok(rest) => Ok(first + rest),
            },
        }
    }
}

/// The canonical results of a file's runs: all of them, as one bag.
pub open spec fn spec_canon_runs<A>(
    oa: spec_fn(A, A) -> Ordering,
    runs: Seq<RawRunV<A>>,
    endpoints_rule: Option<A>,
) -> Result<Seq<CanonResultV<A>>, Error> {
    match spec_pool(oa, runs, endpoints_rule) {
        Ok(results) => Ok(spec_canon_bag(ord_result(oa), results)),
        Err(e) => Err(e),
    }
}

pub open spec fn spec_canon_run<A>(
    oa: spec_fn(A, A) -> Ordering,
    run: RawRunV<A>,
    endpoints_rule: Option<A>,
) -> Result<Seq<CanonResultV<A>>, Error> {
    spec_canon_runs(oa, seq![run], endpoints_rule)
}

pub open spec fn result_view<A>() -> spec_fn(CanonResult<A>) -> CanonResultV<A> {
    |c: CanonResult<A>| c@
}

// ---------------------------------------------------------------------------
// The code. `endpoints_rule` is the atom of the rule whose results are compared by their
// endpoints, or `None` to compare every result in full; `cmp` computes the atom order `oa`.

/// One run's canonical results, sorted.
pub fn canon_run<A: Copy, F: Fn(&A, &A) -> Ordering>(
    run: &RawRun<A>,
    endpoints_rule: Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<Vec<CanonResult<A>>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift_vec(result_view(), r) == spec_canon_run(oa, run@, endpoints_rule),
        r is Ok ==> sorted(ord_result_exec(oa), r->Ok_0@),
{
    proof {
        let runs = seq![run@];
        assert(runs.drop_first() =~= Seq::<RawRunV<A>>::empty());
        assert(spec_pool(oa, runs.drop_first(), endpoints_rule) == Ok::<
            Seq<CanonResultV<A>>,
            Error,
        >(Seq::empty()));
        if let Ok(first) = spec_run_results(oa, run@, endpoints_rule) {
            assert(first + Seq::<CanonResultV<A>>::empty() =~= first);
        }
    }
    let results = run_results(run, endpoints_rule, cmp, Ghost(oa))?;
    Ok(bag_results(results, cmp, Ghost(oa)))
}

/// The canonical results of all of `runs`, pooled and sorted.
pub fn canon_runs<A: Copy, F: Fn(&A, &A) -> Ordering>(
    runs: &[RawRun<A>],
    endpoints_rule: Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<Vec<CanonResult<A>>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift_vec(result_view(), r) == spec_canon_runs(
            oa,
            runs@.map_values(|x: RawRun<A>| x@),
            endpoints_rule,
        ),
        r is Ok ==> sorted(ord_result_exec(oa), r->Ok_0@),
{
    let ghost rv = runs@.map_values(|x: RawRun<A>| x@);
    let mut pool: Vec<CanonResult<A>> = Vec::new();
    let mut i: usize = 0;
    proof {
        assert(rv.skip(0) =~= rv);
        assert forall|s: Seq<CanonResultV<A>>| Seq::<CanonResultV<A>>::empty() + s == s by {
            assert(Seq::<CanonResultV<A>>::empty() + s =~= s);
        }
    }
    while i < runs.len()
        invariant
            i <= runs@.len(),
            rv == runs@.map_values(|x: RawRun<A>| x@),
            total_order(oa),
            equal_means_eq(oa),
            computes(*cmp, oa),
            spec_pool(oa, rv, endpoints_rule) == prepend(
                pool@.map_values(result_view()),
                spec_pool(oa, rv.skip(i as int), endpoints_rule),
            ),
        decreases runs@.len() - i,
    {
        let ghost before = pool@.map_values(result_view());
        proof {
            assert(rv.skip(i as int)[0] == runs@[i as int]@);
            assert(rv.skip(i as int).drop_first() =~= rv.skip(i + 1));
        }
        let mut results = run_results(&runs[i], endpoints_rule, cmp, Ghost(oa))?;
        let ghost added = results@.map_values(result_view());
        pool.append(&mut results);
        proof {
            assert(pool@.map_values(result_view()) =~= before + added);
            lemma_prepend_assoc(before, added, spec_pool(oa, rv.skip(i + 1), endpoints_rule));
        }
        i += 1;
    }
    proof {
        assert(rv.skip(i as int) =~= Seq::<RawRunV<A>>::empty());
        assert(pool@.map_values(result_view()) + Seq::<CanonResultV<A>>::empty()
            =~= pool@.map_values(result_view()));
    }
    Ok(bag_results(pool, cmp, Ghost(oa)))
}

// ---------------------------------------------------------------------------
// Helpers of the code.

/// `s` before the values of `r`, or `r`'s error.
spec fn prepend<V>(s: Seq<V>, r: Result<Seq<V>, Error>) -> Result<Seq<V>, Error> {
    match r {
        Ok(t) => Ok(s + t),
        Err(e) => Err(e),
    }
}

proof fn lemma_prepend_assoc<V>(s: Seq<V>, t: Seq<V>, r: Result<Seq<V>, Error>)
    ensures
        prepend(s, prepend(t, r)) == prepend(s + t, r),
{
    if let Ok(u) = r {
        assert(s + (t + u) =~= (s + t) + u);
    }
}

spec fn atom_view<A>() -> spec_fn(A) -> A {
    |a: A| a
}

spec fn keyed_view<A>() -> spec_fn(Keyed<A>) -> KeyedV<A> {
    |k: Keyed<A>| k@
}

spec fn location_view<A>() -> spec_fn(CanonLocation<A>) -> CanonLocationV<A> {
    |c: CanonLocation<A>| c@
}

spec fn step_view<A>() -> spec_fn(CanonStep<A>) -> CanonStepV<A> {
    |c: CanonStep<A>| c@
}

spec fn thread_flow_view<A>() -> spec_fn(CanonThreadFlow<A>) -> CanonThreadFlowV<A> {
    |c: CanonThreadFlow<A>| c@
}

spec fn code_flow_view<A>() -> spec_fn(CanonCodeFlow<A>) -> CanonCodeFlowV<A> {
    |c: CanonCodeFlow<A>| c@
}

/// `f` of each element of `xs`, in order, or the first error `f` returns, consuming `xs`.
/// `f` computes `sf` on views.
// Verus does not support `while let`.
#[allow(clippy::manual_while_let_some)]
fn map_owned<X, C, V, F: Fn(X) -> Result<C, Error>>(
    xs: Vec<X>,
    f: &F,
    Ghost(view): Ghost<spec_fn(C) -> V>,
    Ghost(sf): Ghost<spec_fn(X) -> Result<V, Error>>,
) -> (r: Result<Vec<C>, Error>)
    requires
        forall|x: X| #[trigger] f.requires((x,)),
        forall|x: X, c: Result<C, Error>| #[trigger] f.ensures((x,), c) ==> lift(view, c) == sf(x),
    ensures
        lift_vec(view, r) == spec_all_ok(spec_canon_seq(xs@, sf)),
{
    let ghost xs0 = xs@;
    let ghost s = spec_canon_seq(xs0, sf);
    let ghost n = xs0.len() as int;
    // `xs` reversed, so that popping it yields `xs` in order.
    let mut rest = xs;
    let mut rev: Vec<X> = Vec::new();
    while !rest.is_empty()
        invariant
            xs0 == xs@,
            n == xs0.len(),
            rest@.len() + rev@.len() == n,
            rest@ == xs0.take(rest@.len() as int),
            forall|k: int| 0 <= k < rev@.len() ==> #[trigger] rev@[k] == xs0[n - 1 - k],
        decreases rest@.len(),
    {
        let x = rest.pop().unwrap();
        rev.push(x);
        proof {
            assert(rest@ =~= xs0.take(rest@.len() as int));
        }
    }
    let mut out: Vec<C> = Vec::new();
    let ghost mut i: int = 0;
    while !rev.is_empty()
        invariant
            xs0 == xs@,
            n == xs0.len(),
            i + rev@.len() == n,
            out@.len() == i,
            s == spec_canon_seq(xs0, sf),
            forall|k: int| 0 <= k < rev@.len() ==> #[trigger] rev@[k] == xs0[n - 1 - k],
            forall|x: X| #[trigger] f.requires((x,)),
            forall|x: X, c: Result<C, Error>| #[trigger]
                f.ensures((x,), c) ==> lift(view, c) == sf(x),
            forall|k: int| 0 <= k < i ==> (#[trigger] s[k]) is Ok && s[k]->Ok_0 == view(out@[k]),
        decreases rev@.len(),
    {
        let x = rev.pop().unwrap();
        proof {
            assert(x == xs0[i]);
        }
        match f(x) {
            Err(e) => {
                proof {
                    assert(s[i] == sf(xs0[i]));
                    assert(s[i] == Err::<V, Error>(e));
                    lemma_all_ok(s);
                    assert(spec_all_ok(s) is Err);
                    let k = choose|k: int|
                        0 <= k < s.len() && (#[trigger] s[k]) is Err && s[k]->Err_0
                            == spec_all_ok(s)->Err_0 && forall|j: int|
                            0 <= j < k ==> (#[trigger] s[j]) is Ok;
                    if k < i {
                        assert(s[k] is Ok);
                    } else if k > i {
                        assert(s[i] is Ok);
                    }
                }
                return Err(e);
            },
            Ok(c) => {
                out.push(c);
            },
        }
        proof {
            i = i + 1;
        }
    }
    proof {
        lemma_all_ok(s);
        assert(spec_all_ok(s) is Ok);
        assert(out@.map_values(view) =~= spec_all_ok(s)->Ok_0);
    }
    Ok(out)
}

/// A bag of atoms, sorted.
fn bag_atoms<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<A>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@ == spec_canon_bag(oa, v@),
{
    proof {
        assert(by_view(atom_view(), oa) =~= oa);
        assert(v@.map_values(atom_view()) =~= v@);
    }
    let r = canon_bag::<A, A, F>(v, cmp, Ghost(atom_view()), Ghost(oa));
    proof {
        assert(r@.map_values(atom_view()) =~= r@);
    }
    r
}

fn bag_keyeds<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<Keyed<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<Keyed<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@.map_values(keyed_view()) == spec_canon_bag(ord_keyed(oa), v@.map_values(keyed_view())),
{
    let goa = Ghost(oa);
    let c = |x: &Keyed<A>, y: &Keyed<A>| -> (o: Ordering)
        ensures
            o == by_view(keyed_view(), ord_keyed(goa@))(*x, *y),
        { cmp_keyed(x, y, cmp, goa) };
    proof {
        lemma_ord_keyed(oa);
    }
    canon_bag::<Keyed<A>, KeyedV<A>, _>(v, &c, Ghost(keyed_view()), Ghost(ord_keyed(oa)))
}

fn bag_locations<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<CanonLocation<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<CanonLocation<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@.map_values(location_view()) == spec_canon_bag(
            ord_location(oa),
            v@.map_values(location_view()),
        ),
{
    let goa = Ghost(oa);
    let c = |x: &CanonLocation<A>, y: &CanonLocation<A>| -> (o: Ordering)
        ensures
            o == by_view(location_view(), ord_location(goa@))(*x, *y),
        { cmp_location(x, y, cmp, goa) };
    proof {
        lemma_ord_location(oa);
    }
    canon_bag::<CanonLocation<A>, CanonLocationV<A>, _>(v, &c, Ghost(location_view()), Ghost(ord_location(oa)))
}

fn bag_thread_flows<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<CanonThreadFlow<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<CanonThreadFlow<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@.map_values(thread_flow_view()) == spec_canon_bag(
            ord_thread_flow(oa),
            v@.map_values(thread_flow_view()),
        ),
{
    let goa = Ghost(oa);
    let c = |x: &CanonThreadFlow<A>, y: &CanonThreadFlow<A>| -> (o: Ordering)
        ensures
            o == by_view(thread_flow_view(), ord_thread_flow(goa@))(*x, *y),
        { cmp_thread_flow(x, y, cmp, goa) };
    proof {
        lemma_ord_thread_flow(oa);
    }
    canon_bag::<CanonThreadFlow<A>, CanonThreadFlowV<A>, _>(v, &c, Ghost(thread_flow_view()), Ghost(ord_thread_flow(oa)))
}

fn bag_code_flows<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<CanonCodeFlow<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<CanonCodeFlow<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@.map_values(code_flow_view()) == spec_canon_bag(
            ord_code_flow(oa),
            v@.map_values(code_flow_view()),
        ),
{
    let goa = Ghost(oa);
    let c = |x: &CanonCodeFlow<A>, y: &CanonCodeFlow<A>| -> (o: Ordering)
        ensures
            o == by_view(code_flow_view(), ord_code_flow(goa@))(*x, *y),
        { cmp_code_flow(x, y, cmp, goa) };
    proof {
        lemma_ord_code_flow(oa);
    }
    canon_bag::<CanonCodeFlow<A>, CanonCodeFlowV<A>, _>(v, &c, Ghost(code_flow_view()), Ghost(ord_code_flow(oa)))
}

fn bag_results<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: Vec<CanonResult<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Vec<CanonResult<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@.map_values(result_view()) == spec_canon_bag(ord_result(oa), v@.map_values(result_view())),
        sorted(ord_result_exec(oa), r@),
{
    let goa = Ghost(oa);
    let c = |x: &CanonResult<A>, y: &CanonResult<A>| -> (o: Ordering)
        ensures
            o == by_view(result_view(), ord_result(goa@))(*x, *y),
        { cmp_result(x, y, cmp, goa) };
    proof {
        lemma_ord_result(oa);
    }
    let r = canon_bag::<CanonResult<A>, CanonResultV<A>, _>(v, &c, Ghost(result_view()), Ghost(ord_result(oa)));
    proof {
        assert forall|p: int, q: int| 0 <= p < q < r@.len() implies le(
            ord_result_exec(oa),
            #[trigger] r@[p],
            #[trigger] r@[q],
        ) by {
            assert(le(by_view(result_view(), ord_result(oa)), r@[p], r@[q]));
        }
    }
    r
}

/// The name a logical reference resolves to.
fn canon_ref<A: Copy>(table: &[RawLogical<A>], r: &RawLogicalRef<A>) -> (o: Result<A, Error>)
    ensures
        o == spec_canon_ref(table@, *r),
{
    match r {
        RawLogicalRef::Index(i) => match resolve(table, *i) {
            Ok(l) => Ok(l.fqn),
            Err(e) => Err(e),
        },
        RawLogicalRef::Inline(a) => Ok(*a),
    }
}

fn canon_location<A: Copy, F: Fn(&A, &A) -> Ordering>(
    l: &RawLocation<A>,
    table: &[RawLogical<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonLocation<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(location_view(), r) == spec_canon_location(oa, table@, l@),
{
    let f = |x: &RawLogicalRef<A>| -> (o: Result<A, Error>)
        ensures
            lift(atom_view(), o) == spec_canon_ref(table@, *x),
        { canon_ref(table, x) };
    let names = canon_seq::<RawLogicalRef<A>, A, A, _>(
        &l.logical,
        &f,
        Ghost(atom_view()),
        Ghost(|x: RawLogicalRef<A>| spec_canon_ref(table@, x)),
    )?;
    proof {
        assert(names@.map_values(atom_view()) =~= names@);
    }
    let physical = match &l.physical {
        Some(p) => Some(copy_physical(p)),
        None => None,
    };
    Ok(CanonLocation { physical, logical: bag_atoms(names, cmp, Ghost(oa)) })
}

fn canon_step<A: Copy, F: Fn(&A, &A) -> Ordering>(
    s: &RawStep<A>,
    table: &[RawLogical<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonStep<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(step_view(), r) == spec_canon_step(oa, table@, s@),
{
    let location = match &s.location {
        None => None,
        Some(l) => Some(canon_location(l, table, cmp, Ghost(oa))?),
    };
    Ok(CanonStep { kinds: bag_atoms(copy_atoms(&s.kinds), cmp, Ghost(oa)), location })
}

fn canon_thread_flow<A: Copy, F: Fn(&A, &A) -> Ordering>(
    f: &RawThreadFlow<A>,
    table: &[RawLogical<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonThreadFlow<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(thread_flow_view(), r) == spec_canon_thread_flow(oa, table@, f@),
{
    if f.steps.is_empty() {
        return Err(Error::EmptyFlow);
    }
    proof {
        assert(f@.steps.map_values(|s: RawStepV<A>| step_order(s)) =~= f.steps@.map_values(
            |s: RawStep<A>| step_order(s@),
        ));
    }
    check_increasing(&f.steps)?;
    let goa = Ghost(oa);
    let g = |s: &RawStep<A>| -> (o: Result<CanonStep<A>, Error>)
        ensures
            lift(step_view(), o) == spec_canon_step(goa@, table@, s@),
        { canon_step(s, table, cmp, goa) };
    proof {
        assert(spec_canon_seq(f@.steps, |s: RawStepV<A>| spec_canon_step(oa, table@, s))
            =~= spec_canon_seq(f.steps@, |s: RawStep<A>| spec_canon_step(oa, table@, s@)));
    }
    let steps = canon_seq::<RawStep<A>, CanonStep<A>, CanonStepV<A>, _>(
        &f.steps,
        &g,
        Ghost(step_view()),
        Ghost(|s: RawStep<A>| spec_canon_step(oa, table@, s@)),
    )?;
    proof {
        assert(steps@.map_values(|x: CanonStep<A>| x@) =~= steps@.map_values(step_view()));
    }
    Ok(CanonThreadFlow::Steps(steps))
}

fn canon_code_flow<A: Copy, F: Fn(&A, &A) -> Ordering>(
    c: &RawCodeFlow<A>,
    table: &[RawLogical<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonCodeFlow<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(code_flow_view(), r) == spec_canon_code_flow(oa, table@, c@),
{
    let goa = Ghost(oa);
    let g = |f: &RawThreadFlow<A>| -> (o: Result<CanonThreadFlow<A>, Error>)
        ensures
            lift(thread_flow_view(), o) == spec_canon_thread_flow(goa@, table@, f@),
        { canon_thread_flow(f, table, cmp, goa) };
    proof {
        assert(spec_canon_seq(
            c@.thread_flows,
            |f: RawThreadFlowV<A>| spec_canon_thread_flow(oa, table@, f),
        ) =~= spec_canon_seq(
            c.thread_flows@,
            |f: RawThreadFlow<A>| spec_canon_thread_flow(oa, table@, f@),
        ));
    }
    let flows = canon_seq::<RawThreadFlow<A>, CanonThreadFlow<A>, CanonThreadFlowV<A>, _>(
        &c.thread_flows,
        &g,
        Ghost(thread_flow_view()),
        Ghost(|f: RawThreadFlow<A>| spec_canon_thread_flow(oa, table@, f@)),
    )?;
    let flows = bag_thread_flows(flows, cmp, Ghost(oa));
    proof {
        assert(flows@.map_values(|t: CanonThreadFlow<A>| t@) =~= flows@.map_values(
            thread_flow_view(),
        ));
    }
    Ok(CanonCodeFlow { thread_flows: flows })
}

/// A bag of atoms, if present, sorted.
fn canon_opt_atoms<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: &Option<Vec<A>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Option<Vec<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        opt_seq(r) == spec_canon_atoms(oa, opt_seq(*v)),
{
    match v {
        Some(v) => Some(bag_atoms(copy_atoms(v), cmp, Ghost(oa))),
        None => None,
    }
}

/// A bag of keyed entries, if present, with each entry's values sorted, then the entries.
fn canon_opt_keyeds<A: Copy, F: Fn(&A, &A) -> Ordering>(
    v: &Option<Vec<Keyed<A>>>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Option<Vec<Keyed<A>>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        opt_keyed(r) == spec_canon_keyeds(oa, opt_keyed(*v)),
{
    match v {
        None => None,
        Some(v) => {
            let mut out: Vec<Keyed<A>> = Vec::new();
            let mut i: usize = 0;
            while i < v.len()
                invariant
                    i <= v@.len(),
                    out@.len() == i,
                    total_order(oa),
                    equal_means_eq(oa),
                    computes(*cmp, oa),
                    forall|k: int|
                        0 <= k < i ==> #[trigger] out@[k]@ == spec_canon_keyed(oa, v@[k]@),
                decreases v@.len() - i,
            {
                let values = bag_atoms(copy_atoms(&v[i].values), cmp, Ghost(oa));
                out.push(Keyed { key: v[i].key, values });
                i += 1;
            }
            proof {
                assert(out@.map_values(keyed_view()) =~= spec_canon_seq(
                    v@.map_values(|k: Keyed<A>| k@),
                    |k: KeyedV<A>| spec_canon_keyed(oa, k),
                ));
            }
            let r = bag_keyeds(out, cmp, Ghost(oa));
            proof {
                assert(r@.map_values(|k: Keyed<A>| k@) =~= r@.map_values(keyed_view()));
            }
            Some(r)
        },
    }
}

fn canon_props<A: Copy, F: Fn(&A, &A) -> Ordering>(
    p: &Props<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Props<A>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        r@ == spec_canon_props(oa, p@),
{
    Props {
        labels: canon_opt_atoms(&p.labels, cmp, Ghost(oa)),
        vertex: p.vertex,
        vertices: canon_opt_keyeds(&p.vertices, cmp, Ghost(oa)),
        source_fns: canon_opt_atoms(&p.source_fns, cmp, Ghost(oa)),
        sink_fns: canon_opt_atoms(&p.sink_fns, cmp, Ghost(oa)),
        tainted_formals: canon_opt_keyeds(&p.tainted_formals, cmp, Ghost(oa)),
        import: p.import,
    }
}

fn canon_result_full<A: Copy, F: Fn(&A, &A) -> Ordering>(
    r: &RawResult<A>,
    table: &[RawLogical<A>],
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (res: Result<CanonResult<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(result_view(), res) == spec_canon_result_full(oa, table@, r@),
{
    let goa = Ghost(oa);
    proof {
        assert(spec_canon_seq(r@.locations, |l: RawLocationV<A>| spec_canon_location(oa, table@, l))
            =~= spec_canon_seq(
            r.locations@,
            |l: RawLocation<A>| spec_canon_location(oa, table@, l@),
        ));
        assert(spec_canon_seq(r@.code_flows, |c: RawCodeFlowV<A>| spec_canon_code_flow(oa, table@, c))
            =~= spec_canon_seq(
            r.code_flows@,
            |c: RawCodeFlow<A>| spec_canon_code_flow(oa, table@, c@),
        ));
    }
    let gl = |l: &RawLocation<A>| -> (o: Result<CanonLocation<A>, Error>)
        ensures
            lift(location_view(), o) == spec_canon_location(goa@, table@, l@),
        { canon_location(l, table, cmp, goa) };
    let locations = canon_seq::<RawLocation<A>, CanonLocation<A>, CanonLocationV<A>, _>(
        &r.locations,
        &gl,
        Ghost(location_view()),
        Ghost(|l: RawLocation<A>| spec_canon_location(oa, table@, l@)),
    )?;
    let gc = |c: &RawCodeFlow<A>| -> (o: Result<CanonCodeFlow<A>, Error>)
        ensures
            lift(code_flow_view(), o) == spec_canon_code_flow(goa@, table@, c@),
        { canon_code_flow(c, table, cmp, goa) };
    let code_flows = canon_seq::<RawCodeFlow<A>, CanonCodeFlow<A>, CanonCodeFlowV<A>, _>(
        &r.code_flows,
        &gc,
        Ghost(code_flow_view()),
        Ghost(|c: RawCodeFlow<A>| spec_canon_code_flow(oa, table@, c@)),
    )?;
    let locations = bag_locations(locations, cmp, Ghost(oa));
    let code_flows = bag_code_flows(code_flows, cmp, Ghost(oa));
    proof {
        assert(locations@.map_values(|l: CanonLocation<A>| l@) =~= locations@.map_values(
            location_view(),
        ));
        assert(code_flows@.map_values(|c: CanonCodeFlow<A>| c@) =~= code_flows@.map_values(
            code_flow_view(),
        ));
    }
    Ok(
        CanonResult {
            rule: r.rule,
            kind: r.kind,
            level: r.level,
            locations,
            code_flows,
            props: canon_props(&r.props, cmp, Ghost(oa)),
        },
    )
}

/// A thread flow cut to its endpoints.
fn endpoints_flow<A: Copy>(f: CanonThreadFlow<A>) -> (r: Result<CanonThreadFlow<A>, Error>)
    ensures
        lift(thread_flow_view(), r) == spec_endpoints_flow(f@),
{
    match f {
        CanonThreadFlow::Steps(s) => {
            proof {
                assert(s@.map_values(|x: CanonStep<A>| x@) =~= s@.map_values(
                    |s: CanonStep<A>| s@,
                ));
            }
            match endpoints(s) {
                Ok((first, last)) => Ok(CanonThreadFlow::Endpoints(first, last)),
                Err(e) => Err(e),
            }
        },
        CanonThreadFlow::Endpoints(first, last) => Ok(CanonThreadFlow::Endpoints(first, last)),
    }
}

fn endpoints_code_flow<A: Copy, F: Fn(&A, &A) -> Ordering>(
    c: CanonCodeFlow<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonCodeFlow<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(code_flow_view(), r) == spec_endpoints_code_flow(oa, c@),
{
    let ghost cv = c@;
    let g = |f: CanonThreadFlow<A>| -> (o: Result<CanonThreadFlow<A>, Error>)
        ensures
            lift(thread_flow_view(), o) == spec_endpoints_flow(f@),
        { endpoints_flow(f) };
    proof {
        assert(spec_canon_seq(cv.thread_flows, |f: CanonThreadFlowV<A>| spec_endpoints_flow(f))
            =~= spec_canon_seq(
            c.thread_flows@,
            |f: CanonThreadFlow<A>| spec_endpoints_flow(f@),
        ));
    }
    let flows = map_owned::<CanonThreadFlow<A>, CanonThreadFlow<A>, CanonThreadFlowV<A>, _>(
        c.thread_flows,
        &g,
        Ghost(thread_flow_view()),
        Ghost(|f: CanonThreadFlow<A>| spec_endpoints_flow(f@)),
    )?;
    let flows = bag_thread_flows(flows, cmp, Ghost(oa));
    proof {
        assert(flows@.map_values(|t: CanonThreadFlow<A>| t@) =~= flows@.map_values(
            thread_flow_view(),
        ));
    }
    Ok(CanonCodeFlow { thread_flows: flows })
}

/// A full canonical result cut to what the endpoints rule compares.
fn endpoints_result<A: Copy, F: Fn(&A, &A) -> Ordering>(
    c: CanonResult<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<CanonResult<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(result_view(), r) == spec_endpoints_result(oa, c@),
{
    let ghost cv = c@;
    let goa = Ghost(oa);
    let g = |f: CanonCodeFlow<A>| -> (o: Result<CanonCodeFlow<A>, Error>)
        ensures
            lift(code_flow_view(), o) == spec_endpoints_code_flow(goa@, f@),
        { endpoints_code_flow(f, cmp, goa) };
    proof {
        assert(spec_canon_seq(
            cv.code_flows,
            |f: CanonCodeFlowV<A>| spec_endpoints_code_flow(oa, f),
        ) =~= spec_canon_seq(c.code_flows@, |f: CanonCodeFlow<A>| spec_endpoints_code_flow(oa, f@)));
    }
    let code_flows = map_owned::<CanonCodeFlow<A>, CanonCodeFlow<A>, CanonCodeFlowV<A>, _>(
        c.code_flows,
        &g,
        Ghost(code_flow_view()),
        Ghost(|f: CanonCodeFlow<A>| spec_endpoints_code_flow(oa, f@)),
    )?;
    let code_flows = bag_code_flows(code_flows, cmp, Ghost(oa));
    let locations: Vec<CanonLocation<A>> = Vec::new();
    proof {
        assert(code_flows@.map_values(|c: CanonCodeFlow<A>| c@) =~= code_flows@.map_values(
            code_flow_view(),
        ));
        assert(locations@.map_values(|l: CanonLocation<A>| l@) =~= Seq::<CanonLocationV<A>>::empty());
    }
    Ok(
        CanonResult {
            rule: c.rule,
            kind: c.kind,
            level: c.level,
            locations,
            code_flows,
            props: Props {
                labels: c.props.labels,
                vertex: None,
                vertices: None,
                source_fns: c.props.source_fns,
                sink_fns: c.props.sink_fns,
                tainted_formals: None,
                import: None,
            },
        },
    )
}

/// Whether `rule` is `endpoints_rule`.
fn is_endpoints_rule<A: Copy, F: Fn(&A, &A) -> Ordering>(
    rule: Option<A>,
    endpoints_rule: Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (b: bool)
    requires
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        b == (endpoints_rule is Some && rule == endpoints_rule),
{
    match (rule, endpoints_rule) {
        (Some(x), Some(e)) => matches!(cmp(&x, &e), Ordering::Equal),
        _ => false,
    }
}

fn canon_result<A: Copy, F: Fn(&A, &A) -> Ordering>(
    r: &RawResult<A>,
    table: &[RawLogical<A>],
    endpoints_rule: Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (res: Result<CanonResult<A>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift(result_view(), res) == spec_canon_result(oa, table@, endpoints_rule, r@),
{
    let full = canon_result_full(r, table, cmp, Ghost(oa))?;
    if is_endpoints_rule(r.rule, endpoints_rule, cmp, Ghost(oa)) {
        endpoints_result(full, cmp, Ghost(oa))
    } else {
        Ok(full)
    }
}

/// A run's canonical results, in file order.
fn run_results<A: Copy, F: Fn(&A, &A) -> Ordering>(
    run: &RawRun<A>,
    endpoints_rule: Option<A>,
    cmp: &F,
    Ghost(oa): Ghost<spec_fn(A, A) -> Ordering>,
) -> (r: Result<Vec<CanonResult<A>>, Error>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        computes(*cmp, oa),
    ensures
        lift_vec(result_view(), r) == spec_run_results(oa, run@, endpoints_rule),
{
    let table: &[RawLogical<A>] = &run.logical;
    let goa = Ghost(oa);
    let g = |x: &RawResult<A>| -> (o: Result<CanonResult<A>, Error>)
        ensures
            lift(result_view(), o) == spec_canon_result(goa@, table@, endpoints_rule, x@),
        { canon_result(x, table, endpoints_rule, cmp, goa) };
    proof {
        assert(spec_canon_seq(
            run@.results,
            |r: RawResultV<A>| spec_canon_result(oa, run@.logical, endpoints_rule, r),
        ) =~= spec_canon_seq(
            run.results@,
            |x: RawResult<A>| spec_canon_result(oa, table@, endpoints_rule, x@),
        ));
    }
    canon_seq::<RawResult<A>, CanonResult<A>, CanonResultV<A>, _>(
        &run.results,
        &g,
        Ghost(result_view()),
        Ghost(|x: RawResult<A>| spec_canon_result(oa, table@, endpoints_rule, x@)),
    )
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ghost;

    const RULE: u32 = 100;
    const ENDPOINTS: u32 = 101;

    fn by_value(a: &u32, b: &u32) -> Ordering {
        a.cmp(b)
    }

    fn canon(run: &RawRun<u32>) -> Result<Vec<CanonResult<u32>>, Error> {
        canon_run(run, None, &by_value, ghost())
    }

    fn canon_endpoints(run: &RawRun<u32>) -> Result<Vec<CanonResult<u32>>, Error> {
        canon_run(run, Some(ENDPOINTS), &by_value, ghost())
    }

    fn no_props() -> Props<u32> {
        Props {
            labels: None,
            vertex: None,
            vertices: None,
            source_fns: None,
            sink_fns: None,
            tainted_formals: None,
            import: None,
        }
    }

    /// A location on `line` naming the logical locations `logical`.
    fn location(line: i64, logical: Vec<RawLogicalRef<u32>>) -> RawLocation<u32> {
        RawLocation {
            physical: Some(Physical {
                base: Some(1),
                uri: Some(2),
                region: Region {
                    start_line: Some(line),
                    start_column: None,
                    end_line: None,
                    end_column: None,
                    byte_offset: None,
                    byte_length: None,
                },
                address: None,
            }),
            logical,
        }
    }

    /// A thread flow with a step on each of `lines`, `executionOrder` 1, 2, ...
    fn flow(lines: &[i64]) -> RawThreadFlow<u32> {
        RawThreadFlow {
            steps: lines
                .iter()
                .enumerate()
                .map(|(k, &line)| RawStep {
                    order: Some(k as i64 + 1),
                    kinds: vec![7, 6],
                    location: Some(location(line, vec![RawLogicalRef::Inline(9)])),
                })
                .collect(),
        }
    }

    fn result(
        rule: u32,
        locations: Vec<RawLocation<u32>>,
        flows: Vec<RawThreadFlow<u32>>,
    ) -> RawResult<u32> {
        RawResult {
            rule: Some(rule),
            kind: 3,
            level: 4,
            locations,
            code_flows: vec![RawCodeFlow {
                thread_flows: flows,
            }],
            props: Props {
                labels: Some(vec![12, 11]),
                vertex: Some(13),
                vertices: Some(vec![
                    Keyed {
                        key: 15,
                        values: vec![17, 16],
                    },
                    Keyed {
                        key: 14,
                        values: vec![],
                    },
                ]),
                source_fns: Some(vec![19, 18]),
                sink_fns: Some(vec![20]),
                ..no_props()
            },
        }
    }

    fn run(logical: &[u32], results: Vec<RawResult<u32>>) -> RawRun<u32> {
        RawRun {
            logical: logical.iter().map(|&fqn| RawLogical { fqn }).collect(),
            results,
        }
    }

    #[test]
    fn renumbered_table_same() {
        use RawLogicalRef::Index;
        let a = run(
            &[30, 40, 50],
            vec![result(
                RULE,
                vec![
                    location(1, vec![Index(0), Index(2)]),
                    location(2, vec![Index(1)]),
                ],
                vec![],
            )],
        );
        // Entry `i` of `a`'s table is entry `pi[i]` of `b`'s, with `pi = [2, 0, 1]`.
        let b = run(
            &[40, 50, 30],
            vec![result(
                RULE,
                vec![
                    location(1, vec![Index(2), Index(1)]),
                    location(2, vec![Index(0)]),
                ],
                vec![],
            )],
        );
        let ca = canon(&a).unwrap();
        assert_eq!(ca, canon(&b).unwrap());
        assert_eq!(ca[0].locations[0].logical, vec![30, 50]);
    }

    #[test]
    fn shuffled_results_same() {
        let r1 = result(RULE, vec![location(1, vec![])], vec![flow(&[1, 2])]);
        let r2 = result(RULE, vec![location(5, vec![]), location(3, vec![])], vec![]);
        let r2_shuffled = result(RULE, vec![location(3, vec![]), location(5, vec![])], vec![]);
        let a = run(&[], vec![r1.clone(), r2]);
        let b = run(&[], vec![r2_shuffled, r1]);
        let ca = canon(&a).unwrap();
        assert_eq!(ca, canon(&b).unwrap());
        assert_eq!(ca.len(), 2);
        // Sorted by the order on results, and every bag inside sorted too.
        let mut sorted = ca.clone();
        sorted.sort_by(|x, y| cmp_result(x, y, &by_value, ghost()));
        assert_eq!(ca, sorted);
        assert_eq!(ca[0].props.labels, Some(vec![11, 12]));
        assert_eq!(ca[0].props.source_fns, Some(vec![18, 19]));
        let vertices = ca[0].props.vertices.as_ref().unwrap();
        assert_eq!(vertices[0].key, 14);
        assert_eq!(vertices[1].values, vec![16, 17]);
    }

    #[test]
    fn swapped_steps_differ() {
        let a = run(&[], vec![result(RULE, vec![], vec![flow(&[1, 2, 3])])]);
        let b = run(&[], vec![result(RULE, vec![], vec![flow(&[1, 3, 2])])]);
        assert_ne!(canon(&a).unwrap(), canon(&b).unwrap());
    }

    #[test]
    fn endpoints_only_ignores_interior() {
        let a = run(
            &[30],
            vec![result(
                ENDPOINTS,
                vec![location(1, vec![RawLogicalRef::Index(0)])],
                vec![flow(&[1, 2, 3])],
            )],
        );
        let b = run(
            &[30],
            vec![result(
                ENDPOINTS,
                vec![location(9, vec![])],
                vec![flow(&[1, 5, 6, 3])],
            )],
        );
        assert_ne!(canon(&a).unwrap(), canon(&b).unwrap());
        let ca = canon_endpoints(&a).unwrap();
        assert_eq!(ca, canon_endpoints(&b).unwrap());
        assert!(ca[0].locations.is_empty());
        assert_eq!(ca[0].props.vertex, None);
        assert_eq!(ca[0].props.vertices, None);
        assert_eq!(ca[0].props.labels, Some(vec![11, 12]));
        assert!(matches!(
            ca[0].code_flows[0].thread_flows[0],
            CanonThreadFlow::Endpoints(_, _)
        ));
        // Another rule is still compared in full.
        let c = run(&[], vec![result(RULE, vec![], vec![flow(&[1, 2, 3])])]);
        let d = run(&[], vec![result(RULE, vec![], vec![flow(&[1, 5, 3])])]);
        assert_ne!(canon_endpoints(&c).unwrap(), canon_endpoints(&d).unwrap());
    }

    #[test]
    fn endpoints_only_counts_twice() {
        let two = run(
            &[],
            vec![
                result(ENDPOINTS, vec![], vec![flow(&[1, 2, 3])]),
                result(ENDPOINTS, vec![], vec![flow(&[1, 4, 3])]),
            ],
        );
        let one = run(&[], vec![result(ENDPOINTS, vec![], vec![flow(&[1, 2, 3])])]);
        let c2 = canon_endpoints(&two).unwrap();
        let c1 = canon_endpoints(&one).unwrap();
        assert_eq!(c2.len(), 2);
        assert_eq!(c1.len(), 1);
        assert_eq!(c2[0], c2[1]);
        assert_eq!(c2[0], c1[0]);
    }

    #[test]
    fn out_of_range_errs() {
        let bad = location(1, vec![RawLogicalRef::Index(1)]);
        let a = run(&[30], vec![result(ENDPOINTS, vec![bad], vec![])]);
        assert_eq!(canon(&a), Err(Error::OutOfRange));
        // The endpoints rule drops the location, but it is still checked.
        assert_eq!(canon_endpoints(&a), Err(Error::OutOfRange));
    }

    #[test]
    fn non_increasing_errs() {
        let mut f = flow(&[1, 2]);
        f.steps[1].order = Some(1);
        let a = run(&[], vec![result(RULE, vec![], vec![f])]);
        assert_eq!(canon(&a), Err(Error::NotIncreasing));
    }

    #[test]
    fn empty_flow_errs() {
        let a = run(&[], vec![result(ENDPOINTS, vec![], vec![flow(&[])])]);
        assert_eq!(canon(&a), Err(Error::EmptyFlow));
        assert_eq!(canon_endpoints(&a), Err(Error::EmptyFlow));
    }

    #[test]
    fn runs_pooled() {
        use RawLogicalRef::Index;
        // Each run's references resolve against its own table.
        let a = run(
            &[30],
            vec![result(RULE, vec![location(1, vec![Index(0)])], vec![])],
        );
        let b = run(
            &[40],
            vec![result(RULE, vec![location(1, vec![Index(0)])], vec![])],
        );
        let both = canon_runs(&[a.clone(), b.clone()], None, &by_value, ghost()).unwrap();
        let mut want = canon(&a).unwrap();
        want.extend(canon(&b).unwrap());
        want.sort_by(|x, y| cmp_result(x, y, &by_value, ghost()));
        assert_eq!(both, want);
        assert_eq!(canon_runs(&[b, a], None, &by_value, ghost()).unwrap(), both);
        let bad = run(
            &[],
            vec![result(RULE, vec![location(1, vec![Index(0)])], vec![])],
        );
        assert_eq!(
            canon_runs(&[bad], None, &by_value, ghost()),
            Err(Error::OutOfRange)
        );
    }
}
