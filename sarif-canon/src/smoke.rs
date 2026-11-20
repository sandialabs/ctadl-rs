//! Checks of the statements, proved from the `spec fn`s alone (no unproved lemma, no stub):
//! that each lemma's and theorem's `requires` can hold, that simple cases come out as they
//! should, and that statements too strong to be true are false.

use crate::canon::*;
use crate::order::*;
use crate::prims::*;
use crate::theorems::*;
use crate::types::*;
use multiset_diff::{sorted, total_order};
use std::cmp::Ordering;
use vstd::prelude::*;

verus! {

/// `u32`'s usual order, the atom order of these checks.
spec fn ou() -> spec_fn(u32, u32) -> Ordering {
    |x: u32, y: u32| crate::u32_ord(x, y)
}

// ---------------------------------------------------------------------------
// The requirements on the atom order can hold.

proof fn smoke_u32_order()
    ensures
        total_order(ou()),
        equal_means_eq(ou()),
{
}

// ---------------------------------------------------------------------------
// Primitives.

proof fn smoke_canon_bag_sorts()
    ensures
        spec_canon_bag(ou(), seq![2u32, 1]) == seq![1u32, 2],
        spec_canon_bag(ou(), seq![1u32, 1]) == seq![1u32, 1],
{
    reveal_with_fuel(spec_canon_bag, 3);
    reveal_with_fuel(insert_sorted, 3);
    assert(spec_canon_bag(ou(), seq![2u32, 1]) =~= seq![1u32, 2]);
    assert(spec_canon_bag(ou(), seq![1u32, 1]) =~= seq![1u32, 1]);
}

// ---------------------------------------------------------------------------
// Concrete runs. Atoms: 1 `fail`, 2 `warning`, 10 the endpoints rule, 20 and 21 function
// names, 30..=33 step kinds, 40 a URI.

spec fn fail() -> u32 { 1 }

spec fn warning() -> u32 { 2 }

spec fn rule() -> u32 { 10 }

spec fn region(line: i64) -> Region {
    Region {
        start_line: Some(line),
        start_column: None,
        end_line: None,
        end_column: None,
        byte_offset: None,
        byte_length: None,
    }
}

spec fn physical(line: i64) -> Physical<u32> {
    Physical { base: None, uri: Some(40), region: region(line), address: None }
}

spec fn no_props() -> PropsV<u32> {
    PropsV {
        labels: None,
        vertex: None,
        vertices: None,
        source_fns: None,
        sink_fns: None,
        tainted_formals: None,
        import: None,
    }
}

/// A location at `line` naming entry `i` of the table.
spec fn location(line: i64, i: usize) -> RawLocationV<u32> {
    RawLocationV { physical: Some(physical(line)), logical: seq![RawLogicalRef::Index(i)] }
}

/// A step with `executionOrder` `order` and one kind.
spec fn step(order: i64, kind: u32) -> RawStepV<u32> {
    RawStepV { order: Some(order), kinds: seq![kind], location: None }
}

/// A C0001-like result: one location naming entry `i`, one code flow of one thread flow.
spec fn result(i: usize, steps: Seq<RawStepV<u32>>) -> RawResultV<u32> {
    RawResultV {
        rule: Some(rule()),
        kind: fail(),
        level: warning(),
        locations: seq![location(1, i)],
        code_flows: seq![RawCodeFlowV { thread_flows: seq![RawThreadFlowV { steps }] }],
        props: no_props(),
    }
}

spec fn table() -> Seq<RawLogical<u32>> {
    seq![RawLogical { fqn: 20u32 }, RawLogical { fqn: 21u32 }]
}

spec fn table_swapped() -> Seq<RawLogical<u32>> {
    seq![RawLogical { fqn: 21u32 }, RawLogical { fqn: 20u32 }]
}

spec fn run(results: Seq<RawResultV<u32>>) -> RawRunV<u32> {
    RawRunV { logical: table(), results }
}

proof fn calc_location(line: i64, i: usize, t: Seq<RawLogical<u32>>, name: u32)
    requires
        i < t.len(),
        t[i as int].fqn == name,
    ensures
        spec_canon_location(ou(), t, location(line, i)) == Ok::<CanonLocationV<u32>, Error>(
            CanonLocationV { physical: Some(physical(line)), logical: seq![name] },
        ),
{
    reveal_with_fuel(spec_canon_bag, 2);
    reveal_with_fuel(spec_all_ok, 2);
    let l = location(line, i);
    let s = spec_canon_seq(l.logical, |r: RawLogicalRef<u32>| spec_canon_ref(t, r));
    assert(s[0] == Ok::<u32, Error>(name));
    assert(s.drop_first().len() == 0);
    assert(spec_all_ok(s) == Ok::<Seq<u32>, Error>(seq![name] + Seq::empty()));
    assert(seq![name] + Seq::<u32>::empty() =~= seq![name]);
    assert(seq![name].drop_first() =~= Seq::<u32>::empty());
    assert(spec_canon_bag(ou(), seq![name]) =~= seq![name]);
}


// ---------------------------------------------------------------------------
// Calculations, each proved by unfolding the definitions.

proof fn calc_all_ok<V>(s: Seq<Result<V, Error>>, v: Seq<V>)
    requires
        s.len() == v.len(),
        forall|k: int| 0 <= k < s.len() ==> #[trigger] s[k] == Ok::<V, Error>(v[k]),
    ensures
        spec_all_ok(s) == Ok::<Seq<V>, Error>(v),
    decreases s.len(),
{
    if s.len() > 0 {
        calc_all_ok(s.drop_first(), v.drop_first());
        assert(seq![v[0]] + v.drop_first() =~= v);
    } else {
        assert(v =~= Seq::<V>::empty());
    }
}

proof fn calc_all_ok_err<V>(s: Seq<Result<V, Error>>, k: int, e: Error)
    requires
        0 <= k < s.len(),
        s[k] == Err::<V, Error>(e),
        forall|j: int| 0 <= j < k ==> (#[trigger] s[j]) is Ok,
    ensures
        spec_all_ok(s) == Err::<Seq<V>, Error>(e),
    decreases k,
{
    if k > 0 {
        calc_all_ok_err(s.drop_first(), k - 1, e);
    }
}

/// A sorted sequence is its own canonical bag.
proof fn calc_bag_sorted<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>)
    requires
        sorted(ord, s),
    ensures
        spec_canon_bag(ord, s) == s,
    decreases s.len(),
{
    if s.len() == 0 {
        assert(s =~= Seq::<T>::empty());
    } else {
        let d = s.drop_first();
        assert(sorted(ord, d)) by {
            assert forall|p: int, q: int| 0 <= p < q < d.len() implies multiset_diff::le(
                ord,
                #[trigger] d[p],
                #[trigger] d[q],
            ) by {
                assert(d[p] == s[p + 1] && d[q] == s[q + 1]);
            }
        }
        calc_bag_sorted(ord, d);
        if d.len() > 0 {
            assert(d[0] == s[1]);
        }
        assert(seq![s[0]] + d =~= s);
    }
}

proof fn calc_bag_one<T>(ord: spec_fn(T, T) -> Ordering, x: T)
    ensures
        spec_canon_bag(ord, seq![x]) == seq![x],
{
    calc_bag_sorted(ord, seq![x]);
}

/// Two equal elements make one canonical bag, whatever the order says of them.
proof fn calc_bag_twice<T>(ord: spec_fn(T, T) -> Ordering, x: T)
    ensures
        spec_canon_bag(ord, seq![x, x]) == seq![x, x],
{
    reveal_with_fuel(insert_sorted, 2);
    calc_bag_one(ord, x);
    assert(seq![x, x].drop_first() =~= seq![x]);
    assert(seq![x].drop_first() =~= Seq::<T>::empty());
    assert(seq![x] + seq![x] =~= seq![x, x]);
}

spec fn canon_step(kind: u32) -> CanonStepV<u32> {
    CanonStepV { kinds: seq![kind], location: None }
}

proof fn calc_step(t: Seq<RawLogical<u32>>, order: i64, kind: u32)
    ensures
        spec_canon_step(ou(), t, step(order, kind)) == Ok::<CanonStepV<u32>, Error>(canon_step(kind)),
{
    calc_bag_one(ou(), kind);
}

proof fn calc_flow(t: Seq<RawLogical<u32>>, steps: Seq<RawStepV<u32>>, kinds: Seq<u32>)
    requires
        steps.len() == kinds.len(),
        forall|k: int| 0 <= k < steps.len() ==> #[trigger] steps[k] == step((k + 1) as i64, kinds[k]),
        0 < steps.len() < 100,
    ensures
        spec_canon_thread_flow(ou(), t, RawThreadFlowV { steps }) == Ok::<
            CanonThreadFlowV<u32>,
            Error,
        >(CanonThreadFlowV::Steps(kinds.map_values(|k: u32| canon_step(k)))),
{
    let orders = steps.map_values(|s: RawStepV<u32>| step_order(s));
    assert forall|i: int, j: int|
        0 <= i < j < orders.len() && (#[trigger] orders[i]) is Some && (#[trigger] orders[j]) is Some
            implies orders[i]->Some_0 < orders[j]->Some_0 by {
        assert(steps[i] == step((i + 1) as i64, kinds[i]));
        assert(steps[j] == step((j + 1) as i64, kinds[j]));
    }
    let mapped = spec_canon_seq(steps, |s: RawStepV<u32>| spec_canon_step(ou(), t, s));
    let want = kinds.map_values(|k: u32| canon_step(k));
    assert forall|k: int| 0 <= k < mapped.len() implies #[trigger] mapped[k] == Ok::<
        CanonStepV<u32>,
        Error,
    >(want[k]) by {
        assert(steps[k] == step((k + 1) as i64, kinds[k]));
        calc_step(t, (k + 1) as i64, kinds[k]);
    }
    calc_all_ok(mapped, want);
}

proof fn calc_code_flow(t: Seq<RawLogical<u32>>, f: RawThreadFlowV<u32>, c: CanonThreadFlowV<u32>)
    requires
        spec_canon_thread_flow(ou(), t, f) == Ok::<CanonThreadFlowV<u32>, Error>(c),
    ensures
        spec_canon_code_flow(ou(), t, RawCodeFlowV { thread_flows: seq![f] }) == Ok::<
            CanonCodeFlowV<u32>,
            Error,
        >(CanonCodeFlowV { thread_flows: seq![c] }),
{
    let mapped = spec_canon_seq(
        seq![f],
        |x: RawThreadFlowV<u32>| spec_canon_thread_flow(ou(), t, x),
    );
    calc_all_ok(mapped, seq![c]);
    calc_bag_one(ord_thread_flow(ou()), c);
}

/// The full canonical form of `result(i, steps)`, steps numbered from 1 with these kinds.
spec fn canon_result(name: u32, kinds: Seq<u32>) -> CanonResultV<u32> {
    CanonResultV {
        rule: Some(rule()),
        kind: fail(),
        level: warning(),
        locations: seq![CanonLocationV { physical: Some(physical(1)), logical: seq![name] }],
        code_flows: seq![
            CanonCodeFlowV {
                thread_flows: seq![CanonThreadFlowV::Steps(kinds.map_values(|k: u32| canon_step(k)))],
            },
        ],
        props: no_props(),
    }
}

/// `canon_result` cut to its endpoints, its first and last kinds `a` and `b`.
spec fn canon_ends(a: u32, b: u32) -> CanonResultV<u32> {
    CanonResultV {
        rule: Some(rule()),
        kind: fail(),
        level: warning(),
        locations: Seq::empty(),
        code_flows: seq![
            CanonCodeFlowV {
                thread_flows: seq![CanonThreadFlowV::Endpoints(canon_step(a), canon_step(b))],
            },
        ],
        props: no_props(),
    }
}

proof fn calc_result_full(t: Seq<RawLogical<u32>>, i: usize, name: u32, steps: Seq<RawStepV<u32>>, kinds: Seq<u32>)
    requires
        i < t.len(),
        t[i as int].fqn == name,
        steps.len() == kinds.len(),
        forall|k: int| 0 <= k < steps.len() ==> #[trigger] steps[k] == step((k + 1) as i64, kinds[k]),
        0 < steps.len() < 100,
    ensures
        spec_canon_result_full(ou(), t, result(i, steps)) == Ok::<CanonResultV<u32>, Error>(
            canon_result(name, kinds),
        ),
{
    let r = result(i, steps);
    let cl = CanonLocationV { physical: Some(physical(1)), logical: seq![name] };
    calc_location(1, i, t, name);
    let locs = spec_canon_seq(r.locations, |l: RawLocationV<u32>| spec_canon_location(ou(), t, l));
    calc_all_ok(locs, seq![cl]);
    calc_bag_one(ord_location(ou()), cl);
    let f = RawThreadFlowV { steps };
    let cf = CanonCodeFlowV {
        thread_flows: seq![CanonThreadFlowV::Steps(kinds.map_values(|k: u32| canon_step(k)))],
    };
    calc_flow(t, steps, kinds);
    calc_code_flow(t, f, CanonThreadFlowV::Steps(kinds.map_values(|k: u32| canon_step(k))));
    let cfs = spec_canon_seq(r.code_flows, |c: RawCodeFlowV<u32>| spec_canon_code_flow(ou(), t, c));
    calc_all_ok(cfs, seq![cf]);
    calc_bag_one(ord_code_flow(ou()), cf);
    assert(spec_canon_props(ou(), no_props()) == no_props());
}

proof fn calc_result_ends(name: u32, kinds: Seq<u32>)
    requires
        kinds.len() > 0,
    ensures
        spec_endpoints_result(ou(), canon_result(name, kinds)) == Ok::<CanonResultV<u32>, Error>(
            canon_ends(kinds[0], kinds.last()),
        ),
{
    let c = canon_result(name, kinds);
    let steps = kinds.map_values(|k: u32| canon_step(k));
    let ends = CanonThreadFlowV::Endpoints(canon_step(kinds[0]), canon_step(kinds.last()));
    assert(spec_endpoints_flow(CanonThreadFlowV::Steps(steps)) == Ok::<CanonThreadFlowV<u32>, Error>(ends));
    let cf = c.code_flows[0];
    let flows = spec_canon_seq(cf.thread_flows, |f: CanonThreadFlowV<u32>| spec_endpoints_flow(f));
    calc_all_ok(flows, seq![ends]);
    calc_bag_one(ord_thread_flow(ou()), ends);
    let cut = CanonCodeFlowV { thread_flows: seq![ends] };
    let cfs = spec_canon_seq(c.code_flows, |f: CanonCodeFlowV<u32>| spec_endpoints_code_flow(ou(), f));
    calc_all_ok(cfs, seq![cut]);
    calc_bag_one(ord_code_flow(ou()), cut);
}


/// The canonical results of a run whose results' canonical forms are known.
proof fn calc_run(
    t: Seq<RawLogical<u32>>,
    results: Seq<RawResultV<u32>>,
    endpoints_rule: Option<u32>,
    canon: Seq<CanonResultV<u32>>,
)
    requires
        results.len() == canon.len(),
        forall|k: int|
            0 <= k < results.len() ==> spec_canon_result(ou(), t, endpoints_rule, #[trigger] results[k])
                == Ok::<CanonResultV<u32>, Error>(canon[k]),
    ensures
        spec_canon_run(ou(), RawRunV { logical: t, results }, endpoints_rule) == Ok::<
            Seq<CanonResultV<u32>>,
            Error,
        >(spec_canon_bag(ord_result(ou()), canon)),
{
    reveal_with_fuel(spec_pool, 2);
    let run = RawRunV { logical: t, results };
    let mapped = spec_canon_seq(
        results,
        |r: RawResultV<u32>| spec_canon_result(ou(), t, endpoints_rule, r),
    );
    calc_all_ok(mapped, canon);
    assert(seq![run].drop_first() =~= Seq::<RawRunV<u32>>::empty());
    assert(canon + Seq::<CanonResultV<u32>>::empty() =~= canon);
}

/// Steps numbered 1, 2, 3 with these kinds.
spec fn steps_of(a: u32, m: u32, b: u32) -> Seq<RawStepV<u32>> {
    seq![step(1, a), step(2, m), step(3, b)]
}

proof fn calc_steps_of(a: u32, m: u32, b: u32)
    ensures
        forall|k: int| 0 <= k < 3 ==> #[trigger] steps_of(a, m, b)[k] == step((k + 1) as i64, seq![a, m, b][k]),
{
}

/// `result(0, steps_of(a, m, b))` in `table()`, in full and cut to its endpoints.
proof fn calc_example(a: u32, m: u32, b: u32)
    ensures
        spec_canon_result(ou(), table(), None, result(0, steps_of(a, m, b))) == Ok::<
            CanonResultV<u32>,
            Error,
        >(canon_result(20, seq![a, m, b])),
        spec_canon_result(ou(), table(), Some(rule()), result(0, steps_of(a, m, b))) == Ok::<
            CanonResultV<u32>,
            Error,
        >(canon_ends(a, b)),
{
    calc_steps_of(a, m, b);
    calc_result_full(table(), 0, 20, steps_of(a, m, b), seq![a, m, b]);
    calc_result_ends(20, seq![a, m, b]);
}

// ---------------------------------------------------------------------------
// Passes.

/// A run with one well-formed result has a canonical form, in full and by endpoints.
proof fn smoke_one_result_defined()
    ensures
        spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), None) == Ok::<
            Seq<CanonResultV<u32>>,
            Error,
        >(seq![canon_result(20, seq![30u32, 31, 33])]),
        spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), Some(rule())) == Ok::<
            Seq<CanonResultV<u32>>,
            Error,
        >(seq![canon_ends(30, 33)]),
{
    calc_example(30, 31, 33);
    calc_run(table(), seq![result(0, steps_of(30, 31, 33))], None, seq![canon_result(20, seq![30u32, 31, 33])]);
    calc_bag_one(ord_result(ou()), canon_result(20, seq![30u32, 31, 33]));
    calc_run(table(), seq![result(0, steps_of(30, 31, 33))], Some(rule()), seq![canon_ends(30, 33)]);
    calc_bag_one(ord_result(ou()), canon_ends(30, 33));
}

/// Swapping the table's two entries, and the reference to match, leaves the result as it was.
proof fn smoke_renumbered_result_same()
    ensures
        spec_canon_result(ou(), table(), None, result(0, steps_of(30, 31, 33))) == spec_canon_result(
            ou(),
            table_swapped(),
            None,
            result(1, steps_of(30, 31, 33)),
        ),
{
    calc_steps_of(30, 31, 33);
    calc_result_full(table(), 0, 20, steps_of(30, 31, 33), seq![30u32, 31, 33]);
    calc_result_full(table_swapped(), 1, 20, steps_of(30, 31, 33), seq![30u32, 31, 33]);
}

proof fn smoke_endpoints_ignore_interior()
    ensures
        spec_endpoints(seq![1u32, 2, 3]) == spec_endpoints(seq![1u32, 4, 3]),
        spec_endpoints(seq![1u32, 2, 3]) == spec_endpoints(seq![1u32, 3]),
        spec_canon_result(ou(), table(), Some(rule()), result(0, steps_of(30, 31, 33)))
            == spec_canon_result(ou(), table(), Some(rule()), result(0, steps_of(30, 32, 33))),
{
    hide(spec_canon_result);
    calc_example(30, 31, 33);
    calc_example(30, 32, 33);
}

// ---------------------------------------------------------------------------
// Errors.

proof fn smoke_out_of_range_errs()
    ensures
        spec_canon_run(ou(), run(seq![result(2, steps_of(30, 31, 33))]), None) == Err::<
            Seq<CanonResultV<u32>>,
            Error,
        >(Error::OutOfRange),
        spec_canon_run(ou(), run(seq![result(2, steps_of(30, 31, 33))]), Some(rule())) == Err::<
            Seq<CanonResultV<u32>>,
            Error,
        >(Error::OutOfRange),
{
    reveal_with_fuel(spec_pool, 2);
    let r = result(2, steps_of(30, 31, 33));
    let l = location(1, 2);
    let refs = spec_canon_seq(l.logical, |x: RawLogicalRef<u32>| spec_canon_ref(table(), x));
    calc_all_ok_err(refs, 0, Error::OutOfRange);
    let locs = spec_canon_seq(r.locations, |x: RawLocationV<u32>| spec_canon_location(ou(), table(), x));
    calc_all_ok_err(locs, 0, Error::OutOfRange);
    let e = Some(rule());
    let rs = spec_canon_seq(seq![r], |x: RawResultV<u32>| spec_canon_result(ou(), table(), None, x));
    calc_all_ok_err(rs, 0, Error::OutOfRange);
    let rs = spec_canon_seq(seq![r], |x: RawResultV<u32>| spec_canon_result(ou(), table(), e, x));
    calc_all_ok_err(rs, 0, Error::OutOfRange);
}

proof fn smoke_not_increasing_errs()
    ensures
        spec_canon_result(
            ou(),
            table(),
            None,
            result(0, seq![step(2, 30), step(2, 31), step(3, 33)]),
        ) == Err::<CanonResultV<u32>, Error>(Error::NotIncreasing),
        spec_canon_result(
            ou(),
            table(),
            Some(rule()),
            result(0, seq![step(2, 30), step(2, 31), step(3, 33)]),
        ) == Err::<CanonResultV<u32>, Error>(Error::NotIncreasing),
        !spec_increasing(seq![Some(2i64), Some(2)]),
        spec_increasing(seq![Some(1i64), None, Some(2)]),
        spec_increasing(Seq::<Option<i64>>::empty()),
{
    let steps = seq![step(2, 30), step(2, 31), step(3, 33)];
    let r = result(0, steps);
    let f = RawThreadFlowV { steps };
    let orders = steps.map_values(|s: RawStepV<u32>| step_order(s));
    assert(orders[0] == Some(2i64) && orders[1] == Some(2i64));
    assert(spec_canon_thread_flow(ou(), table(), f) == Err::<CanonThreadFlowV<u32>, Error>(Error::NotIncreasing));
    let flows = spec_canon_seq(seq![f], |x: RawThreadFlowV<u32>| spec_canon_thread_flow(ou(), table(), x));
    calc_all_ok_err(flows, 0, Error::NotIncreasing);
    let cfs = spec_canon_seq(r.code_flows, |c: RawCodeFlowV<u32>| spec_canon_code_flow(ou(), table(), c));
    calc_all_ok_err(cfs, 0, Error::NotIncreasing);
    calc_location(1, 0, table(), 20);
    let locs = spec_canon_seq(r.locations, |x: RawLocationV<u32>| spec_canon_location(ou(), table(), x));
    calc_all_ok(locs, seq![CanonLocationV { physical: Some(physical(1)), logical: seq![20u32] }]);
    let o = seq![Some(2i64), Some(2)];
    assert(o[0] is Some && o[1] is Some && !(o[0]->Some_0 < o[1]->Some_0));
}

/// A thread flow with no steps is not SARIF: an error, with or without the endpoints rule.
proof fn smoke_empty_flow_errs()
    ensures
        spec_canon_result(ou(), table(), None, result(0, Seq::empty())) == Err::<
            CanonResultV<u32>,
            Error,
        >(Error::EmptyFlow),
        spec_canon_result(ou(), table(), Some(rule()), result(0, Seq::empty())) == Err::<
            CanonResultV<u32>,
            Error,
        >(Error::EmptyFlow),
{
    let steps = Seq::<RawStepV<u32>>::empty();
    let r = result(0, steps);
    let f = RawThreadFlowV { steps };
    assert(spec_canon_thread_flow(ou(), table(), f) == Err::<CanonThreadFlowV<u32>, Error>(Error::EmptyFlow));
    let flows = spec_canon_seq(seq![f], |x: RawThreadFlowV<u32>| spec_canon_thread_flow(ou(), table(), x));
    calc_all_ok_err(flows, 0, Error::EmptyFlow);
    let cfs = spec_canon_seq(r.code_flows, |c: RawCodeFlowV<u32>| spec_canon_code_flow(ou(), table(), c));
    calc_all_ok_err(cfs, 0, Error::EmptyFlow);
    calc_location(1, 0, table(), 20);
    let locs = spec_canon_seq(r.locations, |x: RawLocationV<u32>| spec_canon_location(ou(), table(), x));
    calc_all_ok(locs, seq![CanonLocationV { physical: Some(physical(1)), logical: seq![20u32] }]);
}


// ---------------------------------------------------------------------------
// Fails: statements too strong to be true are false.

spec fn id() -> spec_fn(u32) -> u32 {
    |x: u32| x
}

/// A sequence is not invariant under swapping two elements `f` tells apart.
proof fn smoke_seq_not_swap_invariant()
    ensures
        !(forall|xs: Seq<u32>, j: int, k: int|
            0 <= j < k < xs.len() ==> #[trigger] spec_canon_seq(xs.update(j, xs[k]).update(k, xs[j]), id())
                == #[trigger] spec_canon_seq(xs, id())),
{
    let xs = seq![1u32, 2];
    let ys = xs.update(0, xs[1]).update(1, xs[0]);
    assert(spec_canon_seq(ys, id())[0] == 2);
    assert(spec_canon_seq(xs, id())[0] == 1);
}

/// The canonical results of a run of one example result, in full.
proof fn calc_run_one(a: u32, m: u32, b: u32)
    ensures
        spec_canon_run(ou(), run(seq![result(0, steps_of(a, m, b))]), None) == Ok::<
            Seq<CanonResultV<u32>>,
            Error,
        >(seq![canon_result(20, seq![a, m, b])]),
{
    calc_example(a, m, b);
    let x = canon_result(20, seq![a, m, b]);
    calc_run(table(), seq![result(0, steps_of(a, m, b))], None, seq![x]);
    calc_bag_one(ord_result(ou()), x);
}

/// Swapping two differing steps of a thread flow changes the result.
proof fn smoke_swapped_steps_differ()
    ensures
        spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), None) != spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(31, 30, 33))]),
            None,
        ),
{
    hide(spec_canon_run);
    hide(spec_canon_result);
    calc_run_one(30, 31, 33);
    calc_run_one(31, 30, 33);
    let x = canon_result(20, seq![30u32, 31, 33]);
    let y = canon_result(20, seq![31u32, 30, 33]);
    let sx = seq![30u32, 31, 33].map_values(|k: u32| canon_step(k));
    let sy = seq![31u32, 30, 33].map_values(|k: u32| canon_step(k));
    assert(sx[0].kinds[0] != sy[0].kinds[0]);
    assert(x.code_flows[0].thread_flows[0] != y.code_flows[0].thread_flows[0]);
    assert(seq![x][0] != seq![y][0]);
}

/// The canonical results of a run of the same example result twice.
proof fn calc_run_twice(endpoints_rule: Option<u32>, c: CanonResultV<u32>)
    requires
        spec_canon_result(ou(), table(), endpoints_rule, result(0, steps_of(30, 31, 33))) == Ok::<
            CanonResultV<u32>,
            Error,
        >(c),
    ensures
        spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 31, 33))]),
            endpoints_rule,
        ) == Ok::<Seq<CanonResultV<u32>>, Error>(seq![c, c]),
        spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), endpoints_rule) == Ok::<
            Seq<CanonResultV<u32>>,
            Error,
        >(seq![c]),
{
    hide(spec_canon_result);
    hide(spec_canon_result_full);
    hide(spec_endpoints_result);
    let r = result(0, steps_of(30, 31, 33));
    calc_run(table(), seq![r, r], endpoints_rule, seq![c, c]);
    calc_run(table(), seq![r], endpoints_rule, seq![c]);
    calc_bag_twice(ord_result(ou()), c);
    calc_bag_one(ord_result(ou()), c);
}

/// Deleting one of two equal results changes the canonical results.
proof fn smoke_deleted_duplicate_differs()
    ensures
        spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 31, 33))]),
            None,
        ) != spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), None),
{
    hide(spec_canon_run);
    hide(spec_canon_result);
    calc_example(30, 31, 33);
    let c = canon_result(20, seq![30u32, 31, 33]);
    calc_run_twice(None, c);
    assert(seq![c, c].len() != seq![c].len());
}

/// The canonical results of a run of two example results with equal endpoints, by endpoints.
proof fn calc_run_equal_endpoints()
    ensures
        spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))]),
            Some(rule()),
        ) == Ok::<Seq<CanonResultV<u32>>, Error>(seq![canon_ends(30, 33), canon_ends(30, 33)]),
{
    hide(spec_canon_result);
    hide(spec_canon_result_full);
    hide(spec_endpoints_result);
    calc_example(30, 31, 33);
    calc_example(30, 32, 33);
    let e = canon_ends(30, 33);
    calc_run(
        table(),
        seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))],
        Some(rule()),
        seq![e, e],
    );
    calc_bag_twice(ord_result(ou()), e);
}

/// With the endpoints rule, two results with equal endpoints are two equal results, not one.
proof fn smoke_equal_endpoints_count_twice()
    ensures
        spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))]),
            Some(rule()),
        ) == Ok::<Seq<CanonResultV<u32>>, Error>(seq![canon_ends(30, 33), canon_ends(30, 33)]),
        spec_canon_run(
            ou(),
            run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))]),
            Some(rule()),
        ) != spec_canon_run(ou(), run(seq![result(0, steps_of(30, 31, 33))]), Some(rule())),
{
    hide(spec_canon_run);
    hide(spec_canon_result);
    calc_run_equal_endpoints();
    calc_example(30, 31, 33);
    calc_run_twice(Some(rule()), canon_ends(30, 33));
    assert(seq![canon_ends(30, 33), canon_ends(30, 33)].len() != seq![canon_ends(30, 33)].len());
}

/// Two results that differ only in one region's start line are not `Equal`.
proof fn smoke_region_line_not_equal()
    ensures
        ord_result(ou())(
            canon_result(20, seq![30u32, 31, 33]),
            CanonResultV {
                locations: seq![CanonLocationV { physical: Some(physical(2)), logical: seq![20u32] }],
                ..canon_result(20, seq![30u32, 31, 33])
            },
        ) != Ordering::Equal,
{
    let x = canon_result(20, seq![30u32, 31, 33]);
    let lx = x.locations[0];
    let ly = CanonLocationV { physical: Some(physical(2)), logical: seq![20u32] };
    assert(ord_region()(region(1), region(2)) == Ordering::Less);
    assert(ord_physical(ou())(physical(1), physical(2)) == Ordering::Less);
    assert(ord_location(ou())(lx, ly) == Ordering::Less);
    assert(lex(ord_location(ou()), x.locations, seq![ly]) == Ordering::Less);
}

// ---------------------------------------------------------------------------
// Each lemma's and theorem's `requires` can hold. (`u32`'s order covers the requirements on an
// atom order: `smoke_u32_order`.)

proof fn smoke_permutation_requires()
    ensures
        renumbered_table(table(), table_swapped(), seq![1int, 0]),
        is_permutation(seq![1int, 0], 2),
        sorted(ou(), seq![1u32, 2]),
        seq![1u32, 2].to_multiset() == seq![2u32, 1].to_multiset(),
        seq![2u32, 1][seq![1int, 0][0]] == seq![1u32, 2][0],
        seq![2u32, 1][seq![1int, 0][1]] == seq![1u32, 2][1],
        id()(seq![1u32, 2][0]) != id()(seq![1u32, 2][1]),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    assert(seq![1u32, 2].to_multiset() =~= seq![2u32, 1].to_multiset()) by {
        assert(seq![1u32, 2] =~= seq![1u32].push(2));
        assert(seq![2u32, 1] =~= seq![2u32].push(1));
        assert(seq![1u32] =~= Seq::<u32>::empty().push(1));
        assert(seq![2u32] =~= Seq::<u32>::empty().push(2));
    }
}

/// `theorem_diff_empty_iff_equal`'s sortedness: sequences of up to one result are sorted.
proof fn smoke_diff_requires(a: &[CanonResult<u32>])
    requires
        a@.len() <= 1,
    ensures
        sorted(ord_result_exec(ou()), a@),
{
}

/// Identity permutations of a one-element sequence.
proof fn calc_bag_rel_one<T>(x: T, y: T, rel: spec_fn(T, T) -> bool)
    requires
        rel(x, y),
    ensures
        bag_rel(seq![x], seq![y], rel),
{
    let pi = seq![0int];
    let (xs, ys) = (seq![x], seq![y]);
    assert(is_permutation(pi, xs.len()));
    assert forall|k: int| 0 <= k < xs.len() implies rel(xs[k], ys[#[trigger] pi[k]]) by {
        assert(xs[k] == x && pi[k] == 0 && ys[0] == y);
    }
    assert(is_permutation(pi, xs.len()) && ys.len() == xs.len() && forall|k: int|
        0 <= k < xs.len() ==> rel(xs[k], ys[#[trigger] pi[k]]));
}

/// The swap of a two-element sequence.
proof fn calc_bag_rel_swap<T>(x: T, y: T, rel: spec_fn(T, T) -> bool)
    requires
        rel(x, x),
        rel(y, y),
    ensures
        bag_rel(seq![x, y], seq![y, x], rel),
{
    let pi = seq![1int, 0];
    let (xs, ys) = (seq![x, y], seq![y, x]);
    assert(is_permutation(pi, xs.len()));
    assert forall|k: int| 0 <= k < xs.len() implies rel(xs[k], ys[#[trigger] pi[k]]) by {
        if k == 0 {
            assert(xs[k] == x && pi[k] == 1 && ys[1] == x);
        } else {
            assert(xs[k] == y && pi[k] == 0 && ys[0] == y);
        }
    }
    assert(is_permutation(pi, xs.len()) && ys.len() == xs.len() && forall|k: int|
        0 <= k < xs.len() ==> rel(xs[k], ys[#[trigger] pi[k]]));
}

proof fn calc_result_permuted_self(r: RawResultV<u32>)
    requires
        r == result(0, steps_of(30, 31, 33)) || r == result(0, steps_of(30, 32, 33)),
    ensures
        result_permuted(r, r),
{
    let l = r.locations[0];
    calc_bag_rel_one(l.logical[0], l.logical[0], same());
    calc_bag_rel_one(l, l, |a: RawLocationV<u32>, b: RawLocationV<u32>| location_permuted(a, b));
    let f = r.code_flows[0].thread_flows[0];
    assert forall|k: int| 0 <= k < f.steps.len() implies step_permuted(
        #[trigger] f.steps[k],
        f.steps[k],
    ) by {
        let s = f.steps[k];
        assert(s.kinds.len() == 1);
        assert(s.kinds =~= seq![s.kinds[0]]);
        calc_bag_rel_one(s.kinds[0], s.kinds[0], same());
    }
    calc_bag_rel_one(f, f, |a: RawThreadFlowV<u32>, b: RawThreadFlowV<u32>| thread_flow_permuted(a, b));
    let c = r.code_flows[0];
    calc_bag_rel_one(c, c, |a: RawCodeFlowV<u32>, b: RawCodeFlowV<u32>| code_flow_permuted(a, b));
}

/// A run with two results, and the same run with its results reversed.
proof fn smoke_permuted_bags_holds()
    ensures
        permuted_bags(
            seq![run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))])],
            seq![run(seq![result(0, steps_of(30, 32, 33)), result(0, steps_of(30, 31, 33))])],
        ),
{
    let r1 = result(0, steps_of(30, 31, 33));
    let r2 = result(0, steps_of(30, 32, 33));
    calc_result_permuted_self(r1);
    calc_result_permuted_self(r2);
    let x = run(seq![r1, r2]);
    let y = run(seq![r2, r1]);
    calc_bag_rel_swap(r1, r2, |a: RawResultV<u32>, b: RawResultV<u32>| result_permuted(a, b));
    calc_bag_rel_one(x, y, |a: RawRunV<u32>, b: RawRunV<u32>| run_permuted(a, b));
}

/// A run with its two-entry table swapped and its reference rewritten to match.
proof fn smoke_renumbered_holds()
    ensures
        renumbered(
            run(seq![result(0, steps_of(30, 31, 33))]),
            RawRunV { logical: table_swapped(), results: seq![result(1, steps_of(30, 31, 33))] },
        ),
{
    let pi = seq![1int, 0];
    let x = run(seq![result(0, steps_of(30, 31, 33))]);
    let y = RawRunV { logical: table_swapped(), results: seq![result(1, steps_of(30, 31, 33))] };
    let (rx, ry) = (x.results[0], y.results[0]);
    let f = rx.code_flows[0].thread_flows[0];
    assert forall|k: int| 0 <= k < f.steps.len() implies step_renumbered(
        pi,
        2,
        #[trigger] f.steps[k],
        ry.code_flows[0].thread_flows[0].steps[k],
    ) by {}
    assert(thread_flow_renumbered(pi, 2, f, ry.code_flows[0].thread_flows[0]));
    assert(code_flow_renumbered(pi, 2, rx.code_flows[0], ry.code_flows[0]));
    assert(location_renumbered(pi, 2, rx.locations[0], ry.locations[0]));
    assert(result_renumbered(pi, 2, rx, ry));
    assert(run_renumbered(pi, x, y));
}

/// `theorem_canon_invariant`'s requirement holds of a renumbering followed by a permutation.
proof fn smoke_rewritten_holds()
    ensures
        rewritten(
            seq![run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))])],
            seq![run(seq![result(0, steps_of(30, 32, 33)), result(0, steps_of(30, 31, 33))])],
        ),
        rewritten(
            seq![run(seq![result(0, steps_of(30, 31, 33))])],
            seq![RawRunV { logical: table_swapped(), results: seq![result(1, steps_of(30, 31, 33))] }],
        ),
{
    let xs = seq![run(seq![result(0, steps_of(30, 31, 33)), result(0, steps_of(30, 32, 33))])];
    let ys = seq![run(seq![result(0, steps_of(30, 32, 33)), result(0, steps_of(30, 31, 33))])];
    smoke_permuted_bags_holds();
    let chain = seq![xs, ys];
    assert(one_step(chain[0], chain[1]));
    assert(is_chain(chain));
    let us = seq![run(seq![result(0, steps_of(30, 31, 33))])];
    let vs = seq![RawRunV { logical: table_swapped(), results: seq![result(1, steps_of(30, 31, 33))] }];
    smoke_renumbered_holds();
    assert(renumbered_runs(us, vs));
    let chain2 = seq![us, vs];
    assert(one_step(chain2[0], chain2[1]));
    assert(is_chain(chain2));
}

} // verus!
