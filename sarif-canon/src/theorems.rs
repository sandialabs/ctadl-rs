//! What the canonical form is for, end to end.
//!
//! `theorem_canon_invariant`: rewriting a file's runs by any number of steps, each permuting
//! unordered collections or renumbering a run's `logicalLocations` with every reference into
//! it, does not change the canonical form, or whether there is one.
//!
//! `theorem_diff_empty_iff_equal`: on two sorted sequences of canonical results, such as
//! `canon_runs` returns, `multiset_diff` reports exactly the results whose numbers of
//! occurrences differ, with those numbers, so it reports nothing exactly when the two
//! multisets are equal.

use crate::canon::*;
use crate::order::*;
use crate::prims::*;
use crate::types::*;
use multiset_diff::{diff_from, sorted, total_order};
use std::cmp::Ordering;
use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------------------
// Relating sequences element by element.

/// `y` is a permutation of `x` that takes each element of `x` to one it is `rel` to.
pub open spec fn bag_rel<T>(x: Seq<T>, y: Seq<T>, rel: spec_fn(T, T) -> bool) -> bool {
    exists|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.len()) && y.len() == x.len() && forall|k: int|
            0 <= k < x.len() ==> rel(x[k], y[#[trigger] pi[k]])
}

/// `y` is `x` with each element replaced by one it is `rel` to, in the same order.
pub open spec fn seq_rel<T>(x: Seq<T>, y: Seq<T>, rel: spec_fn(T, T) -> bool) -> bool {
    &&& y.len() == x.len()
    &&& forall|k: int| 0 <= k < x.len() ==> rel(#[trigger] x[k], y[k])
}

pub open spec fn opt_rel<T>(x: Option<T>, y: Option<T>, rel: spec_fn(T, T) -> bool) -> bool {
    match (x, y) {
        (None, None) => true,
        (Some(a), Some(b)) => rel(a, b),
        _ => false,
    }
}

pub open spec fn same<T>() -> spec_fn(T, T) -> bool {
    |a: T, b: T| a == b
}

// ---------------------------------------------------------------------------
// Permuting unordered collections: results (and the runs they are pooled from), a result's
// locations and code flows, a code flow's thread flows, a location's logical locations, a
// step's kinds, and the set- and map-valued properties, at any depth at once. A thread flow's
// steps keep their order, though each step's own collections may be permuted.

pub open spec fn atoms_permuted<A>(x: Seq<A>, y: Seq<A>) -> bool {
    bag_rel(x, y, same())
}

pub open spec fn opt_atoms_permuted<A>(x: Option<Seq<A>>, y: Option<Seq<A>>) -> bool {
    opt_rel(x, y, |s: Seq<A>, t: Seq<A>| atoms_permuted(s, t))
}

pub open spec fn keyed_permuted<A>(x: KeyedV<A>, y: KeyedV<A>) -> bool {
    x.key == y.key && atoms_permuted(x.values, y.values)
}

pub open spec fn opt_keyeds_permuted<A>(x: Option<Seq<KeyedV<A>>>, y: Option<Seq<KeyedV<A>>>) -> bool {
    opt_rel(
        x,
        y,
        |s: Seq<KeyedV<A>>, t: Seq<KeyedV<A>>|
            bag_rel(s, t, |a: KeyedV<A>, b: KeyedV<A>| keyed_permuted(a, b)),
    )
}

pub open spec fn props_permuted<A>(x: PropsV<A>, y: PropsV<A>) -> bool {
    &&& opt_atoms_permuted(x.labels, y.labels)
    &&& x.vertex == y.vertex
    &&& opt_keyeds_permuted(x.vertices, y.vertices)
    &&& opt_atoms_permuted(x.source_fns, y.source_fns)
    &&& opt_atoms_permuted(x.sink_fns, y.sink_fns)
    &&& opt_keyeds_permuted(x.tainted_formals, y.tainted_formals)
    &&& x.import == y.import
}

pub open spec fn location_permuted<A>(x: RawLocationV<A>, y: RawLocationV<A>) -> bool {
    x.physical == y.physical && bag_rel(x.logical, y.logical, same())
}

pub open spec fn step_permuted<A>(x: RawStepV<A>, y: RawStepV<A>) -> bool {
    &&& x.order == y.order
    &&& atoms_permuted(x.kinds, y.kinds)
    &&& opt_rel(x.location, y.location, |a: RawLocationV<A>, b: RawLocationV<A>| location_permuted(a, b))
}

pub open spec fn thread_flow_permuted<A>(x: RawThreadFlowV<A>, y: RawThreadFlowV<A>) -> bool {
    seq_rel(x.steps, y.steps, |a: RawStepV<A>, b: RawStepV<A>| step_permuted(a, b))
}

pub open spec fn code_flow_permuted<A>(x: RawCodeFlowV<A>, y: RawCodeFlowV<A>) -> bool {
    bag_rel(
        x.thread_flows,
        y.thread_flows,
        |a: RawThreadFlowV<A>, b: RawThreadFlowV<A>| thread_flow_permuted(a, b),
    )
}

pub open spec fn result_permuted<A>(x: RawResultV<A>, y: RawResultV<A>) -> bool {
    &&& x.rule == y.rule
    &&& x.kind == y.kind
    &&& x.level == y.level
    &&& bag_rel(x.locations, y.locations, |a: RawLocationV<A>, b: RawLocationV<A>| location_permuted(a, b))
    &&& bag_rel(x.code_flows, y.code_flows, |a: RawCodeFlowV<A>, b: RawCodeFlowV<A>| code_flow_permuted(a, b))
    &&& props_permuted(x.props, y.props)
}

pub open spec fn run_permuted<A>(x: RawRunV<A>, y: RawRunV<A>) -> bool {
    x.logical == y.logical && bag_rel(
        x.results,
        y.results,
        |a: RawResultV<A>, b: RawResultV<A>| result_permuted(a, b),
    )
}

/// `ys` is `xs` with any of its unordered collections permuted, at any depth.
pub open spec fn permuted_bags<A>(xs: Seq<RawRunV<A>>, ys: Seq<RawRunV<A>>) -> bool {
    bag_rel(xs, ys, |a: RawRunV<A>, b: RawRunV<A>| run_permuted(a, b))
}

// ---------------------------------------------------------------------------
// Renumbering a run's `logicalLocations` by a permutation `pi` of its `n` entries: entry `i`
// moves to `pi[i]`, a reference to `i` becomes a reference to `pi[i]`, and a reference out of
// range stays out of range.

pub open spec fn ref_renumbered<A>(pi: Seq<int>, n: nat, x: RawLogicalRef<A>, y: RawLogicalRef<A>) -> bool {
    match (x, y) {
        (RawLogicalRef::Index(i), RawLogicalRef::Index(j)) => if i < n {
            j as int == pi[i as int]
        } else {
            j >= n
        },
        (RawLogicalRef::Inline(a), RawLogicalRef::Inline(b)) => a == b,
        _ => false,
    }
}

pub open spec fn location_renumbered<A>(pi: Seq<int>, n: nat, x: RawLocationV<A>, y: RawLocationV<A>) -> bool {
    x.physical == y.physical && seq_rel(
        x.logical,
        y.logical,
        |a: RawLogicalRef<A>, b: RawLogicalRef<A>| ref_renumbered(pi, n, a, b),
    )
}

pub open spec fn step_renumbered<A>(pi: Seq<int>, n: nat, x: RawStepV<A>, y: RawStepV<A>) -> bool {
    &&& x.order == y.order
    &&& x.kinds == y.kinds
    &&& opt_rel(
        x.location,
        y.location,
        |a: RawLocationV<A>, b: RawLocationV<A>| location_renumbered(pi, n, a, b),
    )
}

pub open spec fn thread_flow_renumbered<A>(
    pi: Seq<int>,
    n: nat,
    x: RawThreadFlowV<A>,
    y: RawThreadFlowV<A>,
) -> bool {
    seq_rel(x.steps, y.steps, |a: RawStepV<A>, b: RawStepV<A>| step_renumbered(pi, n, a, b))
}

pub open spec fn code_flow_renumbered<A>(pi: Seq<int>, n: nat, x: RawCodeFlowV<A>, y: RawCodeFlowV<A>) -> bool {
    seq_rel(
        x.thread_flows,
        y.thread_flows,
        |a: RawThreadFlowV<A>, b: RawThreadFlowV<A>| thread_flow_renumbered(pi, n, a, b),
    )
}

pub open spec fn result_renumbered<A>(pi: Seq<int>, n: nat, x: RawResultV<A>, y: RawResultV<A>) -> bool {
    &&& x.rule == y.rule
    &&& x.kind == y.kind
    &&& x.level == y.level
    &&& seq_rel(
        x.locations,
        y.locations,
        |a: RawLocationV<A>, b: RawLocationV<A>| location_renumbered(pi, n, a, b),
    )
    &&& seq_rel(
        x.code_flows,
        y.code_flows,
        |a: RawCodeFlowV<A>, b: RawCodeFlowV<A>| code_flow_renumbered(pi, n, a, b),
    )
    &&& x.props == y.props
}

pub open spec fn run_renumbered<A>(pi: Seq<int>, x: RawRunV<A>, y: RawRunV<A>) -> bool {
    &&& renumbered_table(x.logical, y.logical, pi)
    &&& seq_rel(
        x.results,
        y.results,
        |a: RawResultV<A>, b: RawResultV<A>| result_renumbered(pi, x.logical.len(), a, b),
    )
}

/// `y` is `x` with its `logicalLocations` renumbered by some permutation.
pub open spec fn renumbered<A>(x: RawRunV<A>, y: RawRunV<A>) -> bool {
    exists|pi: Seq<int>| #[trigger] run_renumbered(pi, x, y)
}

/// `ys` is `xs` with each run renumbered (the identity permutation leaves a run as it is).
pub open spec fn renumbered_runs<A>(xs: Seq<RawRunV<A>>, ys: Seq<RawRunV<A>>) -> bool {
    seq_rel(xs, ys, |a: RawRunV<A>, b: RawRunV<A>| renumbered(a, b))
}

// ---------------------------------------------------------------------------
// Any number of such steps.

pub open spec fn one_step<A>(xs: Seq<RawRunV<A>>, ys: Seq<RawRunV<A>>) -> bool {
    renumbered_runs(xs, ys) || permuted_bags(xs, ys)
}

/// `chain` leads from its first element to its last by `one_step`s.
pub open spec fn is_chain<A>(chain: Seq<Seq<RawRunV<A>>>) -> bool {
    &&& chain.len() >= 1
    &&& forall|k: int| 0 <= k < chain.len() - 1 ==> one_step(#[trigger] chain[k], chain[k + 1])
}

/// `ys` is `xs` after any number of renumberings and permutations.
pub open spec fn rewritten<A>(xs: Seq<RawRunV<A>>, ys: Seq<RawRunV<A>>) -> bool {
    exists|chain: Seq<Seq<RawRunV<A>>>| #[trigger] is_chain(chain) && chain[0] == xs && chain.last() == ys
}

/// Both have no canonical form, or both have the same one. Which error is reported first may
/// depend on the order of the input.
pub open spec fn same_outcome<S>(x: Result<S, Error>, y: Result<S, Error>) -> bool {
    (x is Err && y is Err) || x == y
}

pub proof fn theorem_canon_invariant<A>(
    oa: spec_fn(A, A) -> Ordering,
    xs: Seq<RawRunV<A>>,
    ys: Seq<RawRunV<A>>,
    endpoints_rule: Option<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        rewritten(xs, ys),
    ensures
        same_outcome(spec_canon_runs(oa, xs, endpoints_rule), spec_canon_runs(oa, ys, endpoints_rule)),
{
    admit();
}

// ---------------------------------------------------------------------------
// The diff.

pub open spec fn views<A>(s: Seq<CanonResult<A>>) -> Seq<CanonResultV<A>> {
    s.map_values(|c: CanonResult<A>| c@)
}

pub proof fn theorem_diff_empty_iff_equal<'a, A>(
    oa: spec_fn(A, A) -> Ordering,
    a: &'a [CanonResult<A>],
    b: &'a [CanonResult<A>],
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        sorted(ord_result_exec(oa), a@),
        sorted(ord_result_exec(oa), b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        ({
            let d = diff_from(ord_result_exec(oa), a, b, 0, 0);
            &&& forall|k: int| 0 <= k < d.len() ==> {
                &&& (#[trigger] d[k]).count_a as nat == views(a@).to_multiset().count((*d[k].row)@)
                &&& d[k].count_b as nat == views(b@).to_multiset().count((*d[k].row)@)
                &&& d[k].count_a != d[k].count_b
            }
            &&& forall|v: CanonResultV<A>|
                #![trigger views(a@).to_multiset().count(v)]
                #![trigger views(b@).to_multiset().count(v)]
                views(a@).to_multiset().count(v) != views(b@).to_multiset().count(v) ==> exists|k: int|
                    0 <= k < d.len() && (*(#[trigger] d[k]).row)@ == v
            &&& d.len() == 0 <==> views(a@).to_multiset() == views(b@).to_multiset()
            &&& d.len() == 0 <==> views(a@) == views(b@)
        }),
{
    admit();
}

} // verus!
