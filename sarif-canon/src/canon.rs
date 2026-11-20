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

#[cfg(verus_keep_ghost)]
use crate::order::*;
#[cfg(verus_keep_ghost)]
use crate::prims::*;
#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, sorted, total_order};

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
#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

/// The canonical results of all of `runs`, pooled and sorted.
#[verifier::external_body]
#[allow(unused_variables)]
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
    unimplemented!()
}

} // verus!
