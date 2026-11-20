//! What the canonical form is for, end to end: `theorem_canon_invariant` and
//! `theorem_diff_empty_iff_equal`.

use crate::canon::*;
use crate::order::*;
use crate::prims::*;
use crate::sort::lemma_sorted_unique;
use crate::types::*;
use multiset_diff::proofs::theorem_reports_exactly_count_differences;
use multiset_diff::{count, count_all, diff_from, eqv, le, sorted, total_order};
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

// ---------------------------------------------------------------------------
// Generic facts the invariance proof is built from.

spec fn ord_ints() -> spec_fn(int, int) -> Ordering {
    |x: int, y: int| ord_int(x, y)
}

proof fn lemma_ord_ints()
    ensures
        total_order(ord_ints()),
        equal_means_eq(ord_ints()),
{
    let o = ord_ints();
    assert forall|x: int, y: int| #[trigger] o(x, y) == Ordering::Equal <==> x == y by {}
    assert forall|x: int, y: int, z: int| #[trigger] le(o, x, y) && #[trigger] le(o, y, z) implies le(
        o,
        x,
        z,
    ) by {}
}

/// A permutation of `0..n` reaches every position.
proof fn lemma_permutation_onto(pi: Seq<int>, n: nat, j: int)
    requires
        is_permutation(pi, n),
        0 <= j < n,
    ensures
        exists|k: int| 0 <= k < n && #[trigger] pi[k] == j,
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    let ids = Seq::new(n, |i: int| i);
    let id = |i: int| i;
    lemma_ord_ints();
    assert forall|k: int| 0 <= k < pi.len() implies ids[#[trigger] pi[k]] == pi[k] by {
        assert(0 <= pi[k] < n);
    }
    lemma_canon_bag_permuted(ord_ints(), id, pi, ids, pi);
    assert(spec_canon_seq(pi, id) =~= pi);
    assert(spec_canon_seq(ids, id) =~= ids);
    lemma_canon_bag(ord_ints(), pi);
    lemma_canon_bag(ord_ints(), ids);
    assert(ids[j] == j);
    assert(ids.contains(j));
    assert(pi.to_multiset().count(j) > 0);
    assert(pi.contains(j));
}

/// Two sequences, one a permutation of the other, give the same bag.
proof fn lemma_bag_eq<V>(ord: spec_fn(V, V) -> Ordering, xs: Seq<V>, ys: Seq<V>, pi: Seq<int>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        is_permutation(pi, xs.len()),
        ys.len() == xs.len(),
        forall|k: int| 0 <= k < xs.len() ==> ys[#[trigger] pi[k]] == xs[k],
    ensures
        spec_canon_bag(ord, xs) == spec_canon_bag(ord, ys),
{
    let id = |v: V| v;
    lemma_canon_bag_permuted(ord, id, xs, ys, pi);
    assert(spec_canon_seq(xs, id) =~= xs);
    assert(spec_canon_seq(ys, id) =~= ys);
}

/// Atoms permuted give the same bag.
proof fn lemma_atoms_permuted<A>(oa: spec_fn(A, A) -> Ordering, x: Seq<A>, y: Seq<A>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        atoms_permuted(x, y),
    ensures
        spec_canon_bag(oa, x) == spec_canon_bag(oa, y),
{
    let pi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.len()) && y.len() == x.len() && forall|k: int|
            0 <= k < x.len() ==> same::<A>()(x[k], y[#[trigger] pi[k]]);
    assert forall|k: int| 0 <= k < x.len() implies y[#[trigger] pi[k]] == x[k] by {
        assert(same::<A>()(x[k], y[pi[k]]));
    }
    lemma_bag_eq(oa, x, y, pi);
}

/// Elements related one to one through a permutation, each pair with the same outcome: both
/// sequences fail, or both succeed with the same bag.
proof fn lemma_bag_outcome<V>(
    ord: spec_fn(V, V) -> Ordering,
    rx: Seq<Result<V, Error>>,
    ry: Seq<Result<V, Error>>,
    pi: Seq<int>,
)
    requires
        total_order(ord),
        equal_means_eq(ord),
        is_permutation(pi, rx.len()),
        ry.len() == rx.len(),
        forall|k: int| 0 <= k < rx.len() ==> same_outcome(rx[k], #[trigger] ry[pi[k]]),
    ensures
        spec_all_ok(rx) is Err <==> spec_all_ok(ry) is Err,
        spec_all_ok(rx) is Ok ==> spec_canon_bag(ord, spec_all_ok(rx)->Ok_0) == spec_canon_bag(
            ord,
            spec_all_ok(ry)->Ok_0,
        ),
{
    lemma_all_ok(rx);
    lemma_all_ok(ry);
    let n = rx.len();
    if spec_all_ok(rx) is Ok {
        assert forall|j: int| 0 <= j < n implies (#[trigger] ry[j]) is Ok by {
            lemma_permutation_onto(pi, n, j);
            let k = choose|k: int| 0 <= k < n && #[trigger] pi[k] == j;
            assert(rx[k] is Ok);
            assert(same_outcome(rx[k], ry[pi[k]]));
        }
        let vx = spec_all_ok(rx)->Ok_0;
        let vy = spec_all_ok(ry)->Ok_0;
        assert forall|k: int| 0 <= k < vx.len() implies vy[#[trigger] pi[k]] == vx[k] by {
            assert(rx[k] is Ok);
            assert(same_outcome(rx[k], ry[pi[k]]));
            assert(0 <= pi[k] < n);
            assert(vy[pi[k]] == ry[pi[k]]->Ok_0);
        }
        lemma_bag_eq(ord, vx, vy, pi);
    } else {
        let k = choose|k: int| 0 <= k < n && (#[trigger] rx[k]) is Err;
        assert(same_outcome(rx[k], ry[pi[k]]));
        assert(0 <= pi[k] < n);
        assert(ry[pi[k]] is Err);
    }
}

/// Elements related in order, each pair with the same outcome: the same outcome.
proof fn lemma_seq_outcome<V>(rx: Seq<Result<V, Error>>, ry: Seq<Result<V, Error>>)
    requires
        ry.len() == rx.len(),
        forall|k: int| 0 <= k < rx.len() ==> same_outcome(#[trigger] rx[k], ry[k]),
    ensures
        same_outcome(spec_all_ok(rx), spec_all_ok(ry)),
{
    lemma_all_ok(rx);
    lemma_all_ok(ry);
    if spec_all_ok(rx) is Ok {
        assert forall|k: int| 0 <= k < rx.len() implies (#[trigger] ry[k]) is Ok by {
            assert(rx[k] is Ok);
            assert(same_outcome(rx[k], ry[k]));
        }
        assert forall|k: int| 0 <= k < rx.len() implies #[trigger] ry[k] == rx[k] by {
            assert(rx[k] is Ok);
            assert(same_outcome(rx[k], ry[k]));
        }
        assert(spec_all_ok(rx)->Ok_0 =~= spec_all_ok(ry)->Ok_0);
    } else {
        let k = choose|k: int| 0 <= k < rx.len() && (#[trigger] rx[k]) is Err;
        assert(same_outcome(rx[k], ry[k]));
    }
}

/// `same_outcome` is transitive.
proof fn lemma_same_outcome_trans<S>(x: Result<S, Error>, y: Result<S, Error>, z: Result<S, Error>)
    requires
        same_outcome(x, y),
        same_outcome(y, z),
    ensures
        same_outcome(x, z),
{
}

// ---------------------------------------------------------------------------
// Pooling runs: the pool is the concatenation of every run's results, or an error if any run
// has one.

spec fn flat<V>(ss: Seq<Seq<V>>) -> Seq<V>
    decreases ss.len(),
{
    if ss.len() == 0 {
        Seq::empty()
    } else {
        ss[0] + flat(ss.drop_first())
    }
}

spec fn per_run<A>(oa: spec_fn(A, A) -> Ordering, runs: Seq<RawRunV<A>>, endpoints_rule: Option<A>) -> Seq<
    Result<Seq<CanonResultV<A>>, Error>,
> {
    spec_canon_seq(runs, |r: RawRunV<A>| spec_run_results(oa, r, endpoints_rule))
}

proof fn lemma_pool_flat<A>(oa: spec_fn(A, A) -> Ordering, runs: Seq<RawRunV<A>>, endpoints_rule: Option<A>)
    ensures
        ({
            let all = spec_all_ok(per_run(oa, runs, endpoints_rule));
            &&& spec_pool(oa, runs, endpoints_rule) is Err <==> all is Err
            &&& all is Ok ==> spec_pool(oa, runs, endpoints_rule)->Ok_0 == flat(all->Ok_0)
        }),
    decreases runs.len(),
{
    let rr = per_run(oa, runs, endpoints_rule);
    if runs.len() == 0 {
        return;
    }
    lemma_pool_flat(oa, runs.drop_first(), endpoints_rule);
    assert(rr.drop_first() =~= per_run(oa, runs.drop_first(), endpoints_rule));
    if let Ok(first) = rr[0] {
        if let Ok(rest) = spec_all_ok(rr.drop_first()) {
            let all = seq![first] + rest;
            assert(all.drop_first() =~= rest);
        }
    }
}

/// The multiset of `flat(ss)` is that of `flat` without `ss[m]`, plus `ss[m]`'s.
proof fn lemma_flat_remove<V>(ss: Seq<Seq<V>>, m: int)
    requires
        0 <= m < ss.len(),
    ensures
        flat(ss).to_multiset() == flat(ss.remove(m)).to_multiset().add(ss[m].to_multiset()),
    decreases ss.len(),
{
    broadcast use vstd::seq_lib::lemma_multiset_commutative;

    let rest = ss.drop_first();
    if m == 0 {
        assert(ss.remove(0) =~= rest);
        assert(flat(ss).to_multiset() =~= flat(ss.remove(m)).to_multiset().add(ss[m].to_multiset()));
    } else {
        lemma_flat_remove(rest, m - 1);
        let r = ss.remove(m);
        assert(r.len() > 0);
        assert(r[0] == ss[0]);
        assert(r.drop_first() =~= rest.remove(m - 1));
        assert(flat(r) == r[0] + flat(r.drop_first()));
        assert(flat(ss).to_multiset() =~= flat(ss.remove(m)).to_multiset().add(ss[m].to_multiset()));
    }
}

/// Permuting sequences of values, and each sequence's values, keeps the pooled multiset.
proof fn lemma_flat_permuted<V>(ss: Seq<Seq<V>>, tt: Seq<Seq<V>>, pi: Seq<int>)
    requires
        is_permutation(pi, ss.len()),
        tt.len() == ss.len(),
        forall|k: int| 0 <= k < ss.len() ==> (#[trigger] tt[pi[k]]).to_multiset() == ss[k].to_multiset(),
    ensures
        flat(ss).to_multiset() == flat(tt).to_multiset(),
    decreases ss.len(),
{
    let n = ss.len() as int;
    if n == 0 {
        return;
    }
    let m = pi[n - 1];
    assert(0 <= m < n);
    let ss1 = ss.drop_last();
    let tt1 = tt.remove(m);
    let pi1 = Seq::new((n - 1) as nat, |k: int| if pi[k] < m { pi[k] } else { pi[k] - 1 });
    assert forall|k: int| 0 <= k < n - 1 implies #[trigger] pi[k] != m by {
        assert(pi[k] != pi[n - 1]);
    }
    assert(is_permutation(pi1, ss1.len())) by {
        assert forall|i: int| 0 <= i < n - 1 implies 0 <= #[trigger] pi1[i] < n - 1 by {
            assert(0 <= pi[i] < n && pi[i] != m);
        }
        assert forall|i: int, j: int| 0 <= i < j < n - 1 implies #[trigger] pi1[i]
            != #[trigger] pi1[j] by {
            assert(pi[i] != pi[j]);
            assert(pi[i] != m && pi[j] != m);
        }
    }
    assert forall|k: int| 0 <= k < ss1.len() implies (#[trigger] tt1[pi1[k]]).to_multiset()
        == ss1[k].to_multiset() by {
        assert(tt[pi[k]].to_multiset() == ss[k].to_multiset());
        assert(0 <= pi[k] < n && pi[k] != m);
        assert(tt1[pi1[k]] == tt[pi[k]]);
    }
    lemma_flat_permuted(ss1, tt1, pi1);
    lemma_flat_remove(ss, n - 1);
    assert(ss.remove(n - 1) =~= ss1);
    lemma_flat_remove(tt, m);
    assert(tt[pi[n - 1]].to_multiset() == ss[n - 1].to_multiset());
}

// ---------------------------------------------------------------------------
// Permuting bags leaves each element's canonical form, or whether it has one, unchanged.

proof fn lemma_location_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    x: RawLocationV<A>,
    y: RawLocationV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        location_permuted(x, y),
    ensures
        same_outcome(spec_canon_location(oa, t, x), spec_canon_location(oa, t, y)),
{
    let pi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.logical.len()) && y.logical.len() == x.logical.len()
            && forall|k: int|
            0 <= k < x.logical.len() ==> same::<RawLogicalRef<A>>()(
                x.logical[k],
                y.logical[#[trigger] pi[k]],
            );
    let f = |r: RawLogicalRef<A>| spec_canon_ref(t, r);
    let rx = spec_canon_seq(x.logical, f);
    let ry = spec_canon_seq(y.logical, f);
    assert forall|k: int| 0 <= k < rx.len() implies same_outcome(rx[k], #[trigger] ry[pi[k]]) by {
        assert(same::<RawLogicalRef<A>>()(x.logical[k], y.logical[pi[k]]));
        assert(0 <= pi[k] < x.logical.len());
    }
    lemma_bag_outcome(oa, rx, ry, pi);
}

proof fn lemma_step_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    x: RawStepV<A>,
    y: RawStepV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        step_permuted(x, y),
    ensures
        same_outcome(spec_canon_step(oa, t, x), spec_canon_step(oa, t, y)),
{
    lemma_atoms_permuted(oa, x.kinds, y.kinds);
    if let (Some(a), Some(b)) = (x.location, y.location) {
        lemma_location_permuted(oa, t, a, b);
    }
}

proof fn lemma_thread_flow_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    x: RawThreadFlowV<A>,
    y: RawThreadFlowV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        thread_flow_permuted(x, y),
    ensures
        same_outcome(spec_canon_thread_flow(oa, t, x), spec_canon_thread_flow(oa, t, y)),
{
    let rel = |a: RawStepV<A>, b: RawStepV<A>| step_permuted(a, b);
    assert forall|k: int| 0 <= k < x.steps.len() implies #[trigger] x.steps[k].order == y.steps[k].order by {
        assert(rel(x.steps[k], y.steps[k]));
    }
    assert(x.steps.map_values(|s: RawStepV<A>| step_order(s)) =~= y.steps.map_values(
        |s: RawStepV<A>| step_order(s),
    ));
    let f = |s: RawStepV<A>| spec_canon_step(oa, t, s);
    let rx = spec_canon_seq(x.steps, f);
    let ry = spec_canon_seq(y.steps, f);
    assert forall|k: int| 0 <= k < rx.len() implies same_outcome(#[trigger] rx[k], ry[k]) by {
        assert(rel(x.steps[k], y.steps[k]));
        lemma_step_permuted(oa, t, x.steps[k], y.steps[k]);
    }
    lemma_seq_outcome(rx, ry);
}

proof fn lemma_code_flow_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    x: RawCodeFlowV<A>,
    y: RawCodeFlowV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        code_flow_permuted(x, y),
    ensures
        same_outcome(spec_canon_code_flow(oa, t, x), spec_canon_code_flow(oa, t, y)),
{
    let rel = |a: RawThreadFlowV<A>, b: RawThreadFlowV<A>| thread_flow_permuted(a, b);
    let pi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.thread_flows.len()) && y.thread_flows.len() == x.thread_flows.len()
            && forall|k: int|
            0 <= k < x.thread_flows.len() ==> rel(x.thread_flows[k], y.thread_flows[#[trigger] pi[k]]);
    let f = |g: RawThreadFlowV<A>| spec_canon_thread_flow(oa, t, g);
    let rx = spec_canon_seq(x.thread_flows, f);
    let ry = spec_canon_seq(y.thread_flows, f);
    assert forall|k: int| 0 <= k < rx.len() implies same_outcome(rx[k], #[trigger] ry[pi[k]]) by {
        assert(rel(x.thread_flows[k], y.thread_flows[pi[k]]));
        assert(0 <= pi[k] < x.thread_flows.len());
        lemma_thread_flow_permuted(oa, t, x.thread_flows[k], y.thread_flows[pi[k]]);
    }
    lemma_ord_thread_flow(oa);
    lemma_bag_outcome(ord_thread_flow(oa), rx, ry, pi);
}

proof fn lemma_keyeds_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    x: Option<Seq<KeyedV<A>>>,
    y: Option<Seq<KeyedV<A>>>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        opt_keyeds_permuted(x, y),
    ensures
        spec_canon_keyeds(oa, x) == spec_canon_keyeds(oa, y),
{
    if let (Some(s), Some(u)) = (x, y) {
        let rel = |a: KeyedV<A>, b: KeyedV<A>| keyed_permuted(a, b);
        assert(bag_rel(s, u, rel));
        let pi = choose|pi: Seq<int>|
            #[trigger] is_permutation(pi, s.len()) && u.len() == s.len() && forall|k: int|
                0 <= k < s.len() ==> rel(s[k], u[#[trigger] pi[k]]);
        let f = |k: KeyedV<A>| spec_canon_keyed(oa, k);
        let sx = spec_canon_seq(s, f);
        let sy = spec_canon_seq(u, f);
        assert forall|k: int| 0 <= k < sx.len() implies sy[#[trigger] pi[k]] == sx[k] by {
            assert(rel(s[k], u[pi[k]]));
            assert(0 <= pi[k] < s.len());
            lemma_atoms_permuted(oa, s[k].values, u[pi[k]].values);
        }
        lemma_ord_keyed(oa);
        lemma_bag_eq(ord_keyed(oa), sx, sy, pi);
    }
}

proof fn lemma_opt_atoms_permuted<A>(oa: spec_fn(A, A) -> Ordering, x: Option<Seq<A>>, y: Option<Seq<A>>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        opt_atoms_permuted(x, y),
    ensures
        spec_canon_atoms(oa, x) == spec_canon_atoms(oa, y),
{
    if let (Some(s), Some(u)) = (x, y) {
        lemma_atoms_permuted(oa, s, u);
    }
}

proof fn lemma_props_permuted<A>(oa: spec_fn(A, A) -> Ordering, x: PropsV<A>, y: PropsV<A>)
    requires
        total_order(oa),
        equal_means_eq(oa),
        props_permuted(x, y),
    ensures
        spec_canon_props(oa, x) == spec_canon_props(oa, y),
{
    lemma_opt_atoms_permuted(oa, x.labels, y.labels);
    lemma_opt_atoms_permuted(oa, x.source_fns, y.source_fns);
    lemma_opt_atoms_permuted(oa, x.sink_fns, y.sink_fns);
    lemma_keyeds_permuted(oa, x.vertices, y.vertices);
    lemma_keyeds_permuted(oa, x.tainted_formals, y.tainted_formals);
}

proof fn lemma_result_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    t: Seq<RawLogical<A>>,
    endpoints_rule: Option<A>,
    x: RawResultV<A>,
    y: RawResultV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        result_permuted(x, y),
    ensures
        same_outcome(
            spec_canon_result(oa, t, endpoints_rule, x),
            spec_canon_result(oa, t, endpoints_rule, y),
        ),
{
    let lrel = |a: RawLocationV<A>, b: RawLocationV<A>| location_permuted(a, b);
    let lpi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.locations.len()) && y.locations.len() == x.locations.len()
            && forall|k: int|
            0 <= k < x.locations.len() ==> lrel(x.locations[k], y.locations[#[trigger] pi[k]]);
    let lf = |l: RawLocationV<A>| spec_canon_location(oa, t, l);
    let lx = spec_canon_seq(x.locations, lf);
    let ly = spec_canon_seq(y.locations, lf);
    assert forall|k: int| 0 <= k < lx.len() implies same_outcome(lx[k], #[trigger] ly[lpi[k]]) by {
        assert(lrel(x.locations[k], y.locations[lpi[k]]));
        assert(0 <= lpi[k] < x.locations.len());
        lemma_location_permuted(oa, t, x.locations[k], y.locations[lpi[k]]);
    }
    lemma_ord_location(oa);
    lemma_bag_outcome(ord_location(oa), lx, ly, lpi);

    let crel = |a: RawCodeFlowV<A>, b: RawCodeFlowV<A>| code_flow_permuted(a, b);
    let cpi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.code_flows.len()) && y.code_flows.len() == x.code_flows.len()
            && forall|k: int|
            0 <= k < x.code_flows.len() ==> crel(x.code_flows[k], y.code_flows[#[trigger] pi[k]]);
    let cf = |c: RawCodeFlowV<A>| spec_canon_code_flow(oa, t, c);
    let cx = spec_canon_seq(x.code_flows, cf);
    let cy = spec_canon_seq(y.code_flows, cf);
    assert forall|k: int| 0 <= k < cx.len() implies same_outcome(cx[k], #[trigger] cy[cpi[k]]) by {
        assert(crel(x.code_flows[k], y.code_flows[cpi[k]]));
        assert(0 <= cpi[k] < x.code_flows.len());
        lemma_code_flow_permuted(oa, t, x.code_flows[k], y.code_flows[cpi[k]]);
    }
    lemma_ord_code_flow(oa);
    lemma_bag_outcome(ord_code_flow(oa), cx, cy, cpi);

    lemma_props_permuted(oa, x.props, y.props);
    assert(same_outcome(spec_canon_result_full(oa, t, x), spec_canon_result_full(oa, t, y)));
}

/// Permuting a run's results, at any depth: both fail, or both succeed with the same multiset.
proof fn lemma_run_permuted<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    x: RawRunV<A>,
    y: RawRunV<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        run_permuted(x, y),
    ensures
        spec_run_results(oa, x, endpoints_rule) is Err <==> spec_run_results(oa, y, endpoints_rule) is Err,
        spec_run_results(oa, x, endpoints_rule) is Ok ==> spec_run_results(
            oa,
            x,
            endpoints_rule,
        )->Ok_0.to_multiset() == spec_run_results(oa, y, endpoints_rule)->Ok_0.to_multiset(),
{
    let rel = |a: RawResultV<A>, b: RawResultV<A>| result_permuted(a, b);
    let pi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, x.results.len()) && y.results.len() == x.results.len()
            && forall|k: int| 0 <= k < x.results.len() ==> rel(x.results[k], y.results[#[trigger] pi[k]]);
    let f = |r: RawResultV<A>| spec_canon_result(oa, x.logical, endpoints_rule, r);
    let rx = spec_canon_seq(x.results, f);
    let ry = spec_canon_seq(y.results, f);
    assert forall|k: int| 0 <= k < rx.len() implies same_outcome(rx[k], #[trigger] ry[pi[k]]) by {
        assert(rel(x.results[k], y.results[pi[k]]));
        assert(0 <= pi[k] < x.results.len());
        lemma_result_permuted(oa, x.logical, endpoints_rule, x.results[k], y.results[pi[k]]);
    }
    lemma_ord_result(oa);
    lemma_bag_outcome(ord_result(oa), rx, ry, pi);
    assert(spec_run_results(oa, x, endpoints_rule) == spec_all_ok(rx));
    assert(spec_run_results(oa, y, endpoints_rule) == spec_all_ok(ry));
    if spec_all_ok(rx) is Ok {
        lemma_canon_bag(ord_result(oa), spec_all_ok(rx)->Ok_0);
        lemma_canon_bag(ord_result(oa), spec_all_ok(ry)->Ok_0);
    }
}

proof fn lemma_permuted_bags<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    xs: Seq<RawRunV<A>>,
    ys: Seq<RawRunV<A>>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        permuted_bags(xs, ys),
    ensures
        same_outcome(spec_canon_runs(oa, xs, endpoints_rule), spec_canon_runs(oa, ys, endpoints_rule)),
{
    let rel = |a: RawRunV<A>, b: RawRunV<A>| run_permuted(a, b);
    let pi = choose|pi: Seq<int>|
        #[trigger] is_permutation(pi, xs.len()) && ys.len() == xs.len() && forall|k: int|
            0 <= k < xs.len() ==> rel(xs[k], ys[#[trigger] pi[k]]);
    let rx = per_run(oa, xs, endpoints_rule);
    let ry = per_run(oa, ys, endpoints_rule);
    lemma_pool_flat(oa, xs, endpoints_rule);
    lemma_pool_flat(oa, ys, endpoints_rule);
    lemma_all_ok(rx);
    lemma_all_ok(ry);
    let n = xs.len();
    assert forall|k: int| 0 <= k < n implies {
        &&& (rx[k] is Err <==> (#[trigger] ry[pi[k]]) is Err)
        &&& rx[k] is Ok ==> rx[k]->Ok_0.to_multiset() == ry[pi[k]]->Ok_0.to_multiset()
    } by {
        assert(rel(xs[k], ys[pi[k]]));
        assert(0 <= pi[k] < n);
        lemma_run_permuted(oa, endpoints_rule, xs[k], ys[pi[k]]);
    }
    if spec_all_ok(rx) is Ok {
        assert forall|j: int| 0 <= j < n implies (#[trigger] ry[j]) is Ok by {
            lemma_permutation_onto(pi, n, j);
            let k = choose|k: int| 0 <= k < n && #[trigger] pi[k] == j;
            assert(rx[k] is Ok);
            assert(ry[pi[k]] is Ok);
        }
        let ss = spec_all_ok(rx)->Ok_0;
        let tt = spec_all_ok(ry)->Ok_0;
        assert forall|k: int| 0 <= k < ss.len() implies (#[trigger] tt[pi[k]]).to_multiset()
            == ss[k].to_multiset() by {
            assert(rx[k] is Ok);
            assert(0 <= pi[k] < n);
            assert(tt[pi[k]] == ry[pi[k]]->Ok_0);
        }
        lemma_flat_permuted(ss, tt, pi);
        lemma_ord_result(oa);
        lemma_canon_bag_unique(ord_result(oa), flat(ss), flat(tt));
    } else {
        let k = choose|k: int| 0 <= k < n && (#[trigger] rx[k]) is Err;
        assert(ry[pi[k]] is Err);
        assert(0 <= pi[k] < n);
    }
}

// ---------------------------------------------------------------------------
// Renumbering a table and every reference into it leaves each element's canonical form,
// error included, unchanged.

proof fn lemma_ref_renumbered<A>(
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    x: RawLogicalRef<A>,
    y: RawLogicalRef<A>,
)
    requires
        renumbered_table(t, u, pi),
        ref_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_ref(t, x) == spec_canon_ref(u, y),
{
    if let (RawLogicalRef::Index(i), RawLogicalRef::Index(j)) = (x, y) {
        lemma_resolve_renumbered(t, u, pi, i as int);
        lemma_resolve_renumbered(t, u, pi, j as int);
    }
}

proof fn lemma_location_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    x: RawLocationV<A>,
    y: RawLocationV<A>,
)
    requires
        renumbered_table(t, u, pi),
        location_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_location(oa, t, x) == spec_canon_location(oa, u, y),
{
    let rel = |a: RawLogicalRef<A>, b: RawLogicalRef<A>| ref_renumbered(pi, t.len(), a, b);
    let rx = spec_canon_seq(x.logical, |r: RawLogicalRef<A>| spec_canon_ref(t, r));
    let ry = spec_canon_seq(y.logical, |r: RawLogicalRef<A>| spec_canon_ref(u, r));
    assert forall|k: int| 0 <= k < rx.len() implies #[trigger] rx[k] == ry[k] by {
        assert(rel(x.logical[k], y.logical[k]));
        lemma_ref_renumbered(pi, t, u, x.logical[k], y.logical[k]);
    }
    assert(rx =~= ry);
}

proof fn lemma_step_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    x: RawStepV<A>,
    y: RawStepV<A>,
)
    requires
        renumbered_table(t, u, pi),
        step_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_step(oa, t, x) == spec_canon_step(oa, u, y),
{
    if let (Some(a), Some(b)) = (x.location, y.location) {
        lemma_location_renumbered(oa, pi, t, u, a, b);
    }
}

proof fn lemma_thread_flow_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    x: RawThreadFlowV<A>,
    y: RawThreadFlowV<A>,
)
    requires
        renumbered_table(t, u, pi),
        thread_flow_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_thread_flow(oa, t, x) == spec_canon_thread_flow(oa, u, y),
{
    let rel = |a: RawStepV<A>, b: RawStepV<A>| step_renumbered(pi, t.len(), a, b);
    assert forall|k: int| 0 <= k < x.steps.len() implies #[trigger] x.steps[k].order == y.steps[k].order by {
        assert(rel(x.steps[k], y.steps[k]));
    }
    assert(x.steps.map_values(|s: RawStepV<A>| step_order(s)) =~= y.steps.map_values(
        |s: RawStepV<A>| step_order(s),
    ));
    let rx = spec_canon_seq(x.steps, |s: RawStepV<A>| spec_canon_step(oa, t, s));
    let ry = spec_canon_seq(y.steps, |s: RawStepV<A>| spec_canon_step(oa, u, s));
    assert forall|k: int| 0 <= k < rx.len() implies #[trigger] rx[k] == ry[k] by {
        assert(rel(x.steps[k], y.steps[k]));
        lemma_step_renumbered(oa, pi, t, u, x.steps[k], y.steps[k]);
    }
    assert(rx =~= ry);
}

proof fn lemma_code_flow_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    x: RawCodeFlowV<A>,
    y: RawCodeFlowV<A>,
)
    requires
        renumbered_table(t, u, pi),
        code_flow_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_code_flow(oa, t, x) == spec_canon_code_flow(oa, u, y),
{
    let rel = |a: RawThreadFlowV<A>, b: RawThreadFlowV<A>| thread_flow_renumbered(pi, t.len(), a, b);
    let rx = spec_canon_seq(x.thread_flows, |f: RawThreadFlowV<A>| spec_canon_thread_flow(oa, t, f));
    let ry = spec_canon_seq(y.thread_flows, |f: RawThreadFlowV<A>| spec_canon_thread_flow(oa, u, f));
    assert forall|k: int| 0 <= k < rx.len() implies #[trigger] rx[k] == ry[k] by {
        assert(rel(x.thread_flows[k], y.thread_flows[k]));
        lemma_thread_flow_renumbered(oa, pi, t, u, x.thread_flows[k], y.thread_flows[k]);
    }
    assert(rx =~= ry);
}

proof fn lemma_result_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    pi: Seq<int>,
    t: Seq<RawLogical<A>>,
    u: Seq<RawLogical<A>>,
    endpoints_rule: Option<A>,
    x: RawResultV<A>,
    y: RawResultV<A>,
)
    requires
        renumbered_table(t, u, pi),
        result_renumbered(pi, t.len(), x, y),
    ensures
        spec_canon_result(oa, t, endpoints_rule, x) == spec_canon_result(oa, u, endpoints_rule, y),
{
    let lrel = |a: RawLocationV<A>, b: RawLocationV<A>| location_renumbered(pi, t.len(), a, b);
    let lx = spec_canon_seq(x.locations, |l: RawLocationV<A>| spec_canon_location(oa, t, l));
    let ly = spec_canon_seq(y.locations, |l: RawLocationV<A>| spec_canon_location(oa, u, l));
    assert forall|k: int| 0 <= k < lx.len() implies #[trigger] lx[k] == ly[k] by {
        assert(lrel(x.locations[k], y.locations[k]));
        lemma_location_renumbered(oa, pi, t, u, x.locations[k], y.locations[k]);
    }
    assert(lx =~= ly);
    let crel = |a: RawCodeFlowV<A>, b: RawCodeFlowV<A>| code_flow_renumbered(pi, t.len(), a, b);
    let cx = spec_canon_seq(x.code_flows, |c: RawCodeFlowV<A>| spec_canon_code_flow(oa, t, c));
    let cy = spec_canon_seq(y.code_flows, |c: RawCodeFlowV<A>| spec_canon_code_flow(oa, u, c));
    assert forall|k: int| 0 <= k < cx.len() implies #[trigger] cx[k] == cy[k] by {
        assert(crel(x.code_flows[k], y.code_flows[k]));
        lemma_code_flow_renumbered(oa, pi, t, u, x.code_flows[k], y.code_flows[k]);
    }
    assert(cx =~= cy);
    assert(spec_canon_result_full(oa, t, x) == spec_canon_result_full(oa, u, y));
}

proof fn lemma_run_renumbered<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    x: RawRunV<A>,
    y: RawRunV<A>,
)
    requires
        renumbered(x, y),
    ensures
        spec_run_results(oa, x, endpoints_rule) == spec_run_results(oa, y, endpoints_rule),
{
    let pi = choose|pi: Seq<int>| #[trigger] run_renumbered(pi, x, y);
    let t = x.logical;
    let u = y.logical;
    let rel = |a: RawResultV<A>, b: RawResultV<A>| result_renumbered(pi, t.len(), a, b);
    let rx = spec_canon_seq(x.results, |r: RawResultV<A>| spec_canon_result(oa, t, endpoints_rule, r));
    let ry = spec_canon_seq(y.results, |r: RawResultV<A>| spec_canon_result(oa, u, endpoints_rule, r));
    assert forall|k: int| 0 <= k < rx.len() implies #[trigger] rx[k] == ry[k] by {
        assert(rel(x.results[k], y.results[k]));
        lemma_result_renumbered(oa, pi, t, u, endpoints_rule, x.results[k], y.results[k]);
    }
    assert(rx =~= ry);
}

proof fn lemma_renumbered_runs<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    xs: Seq<RawRunV<A>>,
    ys: Seq<RawRunV<A>>,
)
    requires
        renumbered_runs(xs, ys),
    ensures
        spec_canon_runs(oa, xs, endpoints_rule) == spec_canon_runs(oa, ys, endpoints_rule),
{
    let rel = |a: RawRunV<A>, b: RawRunV<A>| renumbered(a, b);
    let rx = per_run(oa, xs, endpoints_rule);
    let ry = per_run(oa, ys, endpoints_rule);
    assert forall|k: int| 0 <= k < rx.len() implies #[trigger] rx[k] == ry[k] by {
        assert(rel(xs[k], ys[k]));
        lemma_run_renumbered(oa, endpoints_rule, xs[k], ys[k]);
    }
    assert(rx =~= ry);
    lemma_pool_same(oa, xs, ys, endpoints_rule);
}

/// Runs whose results are the same have the same pool, error included.
proof fn lemma_pool_same<A>(
    oa: spec_fn(A, A) -> Ordering,
    xs: Seq<RawRunV<A>>,
    ys: Seq<RawRunV<A>>,
    endpoints_rule: Option<A>,
)
    requires
        per_run(oa, xs, endpoints_rule) == per_run(oa, ys, endpoints_rule),
    ensures
        spec_pool(oa, xs, endpoints_rule) == spec_pool(oa, ys, endpoints_rule),
    decreases xs.len(),
{
    let rx = per_run(oa, xs, endpoints_rule);
    let ry = per_run(oa, ys, endpoints_rule);
    assert(xs.len() == rx.len() && ys.len() == ry.len());
    if xs.len() == 0 {
        return;
    }
    assert(rx[0] == spec_run_results(oa, xs[0], endpoints_rule));
    assert(ry[0] == spec_run_results(oa, ys[0], endpoints_rule));
    assert(per_run(oa, xs.drop_first(), endpoints_rule) =~= rx.drop_first());
    assert(per_run(oa, ys.drop_first(), endpoints_rule) =~= ry.drop_first());
    lemma_pool_same(oa, xs.drop_first(), ys.drop_first(), endpoints_rule);
}

// ---------------------------------------------------------------------------
// Any number of steps.

proof fn lemma_one_step<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    xs: Seq<RawRunV<A>>,
    ys: Seq<RawRunV<A>>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        one_step(xs, ys),
    ensures
        same_outcome(spec_canon_runs(oa, xs, endpoints_rule), spec_canon_runs(oa, ys, endpoints_rule)),
{
    if renumbered_runs(xs, ys) {
        lemma_renumbered_runs(oa, endpoints_rule, xs, ys);
    } else {
        lemma_permuted_bags(oa, endpoints_rule, xs, ys);
    }
}

proof fn lemma_chain<A>(
    oa: spec_fn(A, A) -> Ordering,
    endpoints_rule: Option<A>,
    chain: Seq<Seq<RawRunV<A>>>,
    k: int,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        is_chain(chain),
        0 <= k < chain.len(),
    ensures
        same_outcome(
            spec_canon_runs(oa, chain[0], endpoints_rule),
            spec_canon_runs(oa, chain[k], endpoints_rule),
        ),
    decreases k,
{
    if k > 0 {
        lemma_chain(oa, endpoints_rule, chain, k - 1);
        lemma_one_step(oa, endpoints_rule, chain[k - 1], chain[k]);
        lemma_same_outcome_trans(
            spec_canon_runs(oa, chain[0], endpoints_rule),
            spec_canon_runs(oa, chain[k - 1], endpoints_rule),
            spec_canon_runs(oa, chain[k], endpoints_rule),
        );
    }
}

/// Two SARIF files that differ only in how they were written have the same canonical form,
/// so the diff reports no difference between them. "How they were written" means any mix of
/// two edits, applied any number of times: listing an unordered collection (the runs, a run's
/// results, a result's locations, ...; not a thread flow's steps) in another order, and
/// reordering a run's `logicalLocations` while rewriting every `index` into it to match. If
/// one file has no canonical form, neither does the other, though they may report different
/// errors.
///
/// The proof walks the chain of edits from `xs` to `ys`; `lemma_one_step` shows each edit
/// leaves the canonical form, or its absence, unchanged.
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
    let chain = choose|chain: Seq<Seq<RawRunV<A>>>|
        #[trigger] is_chain(chain) && chain[0] == xs && chain.last() == ys;
    lemma_chain(oa, endpoints_rule, chain, chain.len() - 1);
}

// ---------------------------------------------------------------------------
// The diff.

pub open spec fn views<A>(s: Seq<CanonResult<A>>) -> Seq<CanonResultV<A>> {
    s.map_values(|c: CanonResult<A>| c@)
}

/// On two sorted sequences of canonical results, such as `canon_runs` returns, `multiset_diff`
/// reports exactly the results whose numbers of occurrences differ, with those numbers, so it
/// reports nothing exactly when the two multisets are equal.
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
    let ord = ord_result_exec(oa);
    lemma_ord_result_exec(oa);
    lemma_ord_result(oa);
    theorem_reports_exactly_count_differences(ord, a, b);
    let d = diff_from(ord, a, b, 0, 0);
    let ma = views(a@).to_multiset();
    let mb = views(b@).to_multiset();
    assert forall|c: CanonResult<A>|
        #![trigger count_all(ord, a@, c)]
        #![trigger count_all(ord, b@, c)]
        count_all(ord, a@, c) == ma.count(c@) && count_all(ord, b@, c) == mb.count(c@) by {
        lemma_count_all_views(oa, a@, c);
        lemma_count_all_views(oa, b@, c);
    }
    assert forall|k: int| 0 <= k < d.len() implies {
        &&& (#[trigger] d[k]).count_a as nat == ma.count((*d[k].row)@)
        &&& d[k].count_b as nat == mb.count((*d[k].row)@)
        &&& d[k].count_a != d[k].count_b
    } by {
        assert(count_all(ord, a@, *d[k].row) == ma.count((*d[k].row)@));
        assert(count_all(ord, b@, *d[k].row) == mb.count((*d[k].row)@));
    }
    assert forall|v: CanonResultV<A>| #[trigger] ma.count(v) != mb.count(v) implies exists|k: int|
        0 <= k < d.len() && (*(#[trigger] d[k]).row)@ == v by {
        broadcast use vstd::seq_lib::group_to_multiset_ensures;

        let c = if ma.count(v) > 0 {
            assert(views(a@).contains(v));
            let i = choose|i: int| 0 <= i < views(a@).len() && views(a@)[i] == v;
            a@[i]
        } else {
            assert(views(b@).contains(v));
            let i = choose|i: int| 0 <= i < views(b@).len() && views(b@)[i] == v;
            b@[i]
        };
        assert(c@ == v);
        assert(count_all(ord, a@, c) != count_all(ord, b@, c));
        let k = choose|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, c);
        assert((*d[k].row)@ == v);
    }
    if d.len() == 0 {
        assert(ma =~= mb);
    }
    if ma == mb && d.len() > 0 {
        assert(d[0].count_a != d[0].count_b);
    }
    lemma_views_sorted(oa, a@);
    lemma_views_sorted(oa, b@);
    if ma == mb {
        lemma_sorted_unique(ord_result(oa), views(a@), views(b@));
    }
}

/// `views` is sorted by the order on views when its results are sorted by the order on results.
proof fn lemma_views_sorted<A>(oa: spec_fn(A, A) -> Ordering, s: Seq<CanonResult<A>>)
    requires
        sorted(ord_result_exec(oa), s),
    ensures
        sorted(ord_result(oa), views(s)),
{
    assert forall|p: int, q: int| 0 <= p < q < views(s).len() implies le(
        ord_result(oa),
        #[trigger] views(s)[p],
        #[trigger] views(s)[q],
    ) by {
        assert(le(ord_result_exec(oa), s[p], s[q]));
    }
}

/// Counting results the order calls `Equal` is counting equal views.
proof fn lemma_count_all_views<A>(oa: spec_fn(A, A) -> Ordering, s: Seq<CanonResult<A>>, c: CanonResult<A>)
    requires
        total_order(oa),
        equal_means_eq(oa),
    ensures
        count_all(ord_result_exec(oa), s, c) == views(s).to_multiset().count(c@),
{
    lemma_count_views(oa, s, s.len() as int, c);
    assert(s.take(s.len() as int) =~= s);
}

proof fn lemma_count_views<A>(
    oa: spec_fn(A, A) -> Ordering,
    s: Seq<CanonResult<A>>,
    hi: int,
    c: CanonResult<A>,
)
    requires
        total_order(oa),
        equal_means_eq(oa),
        0 <= hi <= s.len(),
    ensures
        count(ord_result_exec(oa), s, 0, hi, c) == views(s.take(hi)).to_multiset().count(c@),
    decreases hi,
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    lemma_ord_result_exec(oa);
    if hi == 0 {
        assert(views(s.take(0)) =~= Seq::<CanonResultV<A>>::empty());
    } else {
        lemma_count_views(oa, s, hi - 1, c);
        assert(views(s.take(hi)) =~= views(s.take(hi - 1)).push(s[hi - 1]@));
        assert(eqv(ord_result_exec(oa), s[hi - 1], c) <==> s[hi - 1]@ == c@);
    }
}

} // verus!
