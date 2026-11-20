//! Names each key of two sequences of `(key, fact)` rows by the set of its facts.
//!
//! [`Names::build`] takes the rows of two sides, each sorted by key and then by fact, and gives
//! every key a name: a `u32` that is 0 for a key with no facts and otherwise the rank, among the
//! distinct sets of facts of both sides, of the key's set. The code is verified with Verus, for
//! inputs of any length, against the orders of `order-laws`: `nix build .#checks.<system>.verus`.
//! A plain `cargo build` erases the specs and proofs.
//!
//! What is proved (`Names::theorem_names`): for any keys `i` and `j`, on either side, the names
//! are equal exactly when `i` and `j` have the same set of facts. Keys and facts are the same
//! when the orders `cmp_i` and `cmp_r` compute call them `Equal`. A key on no row has no facts.
//! The rest of the crate's guarantees are checks at run time: `build` returns `None` if the
//! rows are not sorted, so the theorem holds for anything it returns. `facts_of` is not verified.

use std::cmp::Ordering;
use vstd::prelude::*;

// Specs and proofs only, which a plain build erases.
#[cfg(verus_keep_ghost)]
pub mod order;

#[cfg(verus_keep_ghost)]
use order::*;
#[cfg(verus_keep_ghost)]
use order_laws::*;

verus! {

/// `r` is a fact of key `i` in `rows`: some row's key is equivalent to `i`, and its fact to `r`.
pub open spec fn has_fact<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    rows: Seq<(I, R)>,
    i: I,
    r: R,
) -> bool {
    exists|k: int| 0 <= k < rows.len() && eqv(oi, (#[trigger] rows[k]).0, i) && eqv(or, rows[k].1, r)
}

/// Key `i` of rows `a` and key `j` of rows `b` have the same facts.
pub open spec fn same_facts<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    a: Seq<(I, R)>,
    i: I,
    b: Seq<(I, R)>,
    j: I,
) -> bool {
    forall|r: R|
        #![trigger has_fact(oi, or, a, i, r)]
        #![trigger has_fact(oi, or, b, j, r)]
        has_fact(oi, or, a, i, r) <==> has_fact(oi, or, b, j, r)
}

// ---------------------------------------------------------------------------
// Groups: keys, and each key's facts as a strictly sorted run of `facts`.

/// Group `g`'s facts.
pub open spec fn group_of<R>(starts: Seq<usize>, facts: Seq<R>, g: int) -> Seq<R> {
    facts.subrange(starts[g] as int, starts[g + 1] as int)
}

/// `n` groups: group `g` is `facts[starts[g]..starts[g + 1]]`, non-empty and strictly sorted.
pub open spec fn shape<R>(or: spec_fn(R, R) -> Ordering, n: int, starts: Seq<usize>, facts: Seq<R>) -> bool {
    &&& starts.len() == n + 1
    &&& starts[n] == facts.len()
    &&& forall|g: int| 0 <= g <= n ==> #[trigger] starts[g] <= facts.len()
    &&& forall|g: int| 0 <= g < n ==> #[trigger] starts[g] < starts[g + 1]
    &&& forall|g: int| 0 <= g < n ==> strictly_sorted(or, #[trigger] group_of(starts, facts, g))
}

/// Groups `lo..hi` are the keys of `rows` with their facts: the keys strictly sorted, each
/// group holding its key's facts, and every key with a fact among them.
pub open spec fn side_wf<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    keys: Seq<I>,
    starts: Seq<usize>,
    facts: Seq<R>,
    lo: int,
    hi: int,
    rows: Seq<(I, R)>,
) -> bool {
    &&& forall|p: int, q: int| lo <= p < q < hi ==> lt(oi, #[trigger] keys[p], #[trigger] keys[q])
    &&& forall|g: int, r: R|
        #![trigger has_fact(oi, or, rows, keys[g], r)]
        #![trigger mem(or, group_of(starts, facts, g), r)]
        lo <= g < hi ==> (has_fact(oi, or, rows, keys[g], r) <==> mem(or, group_of(starts, facts, g), r))
    &&& forall|i: I, r: R|
        #[trigger] has_fact(oi, or, rows, i, r) ==> exists|g: int| lo <= g < hi && eqv(oi, #[trigger] keys[g], i)
}

// ---------------------------------------------------------------------------
// Lemmas about `has_fact`.

proof fn lemma_has_fact_step<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    rows: Seq<(I, R)>,
    k: int,
    i: I,
    r: R,
)
    requires
        0 <= k < rows.len(),
    ensures
        has_fact(oi, or, rows.take(k + 1), i, r) <==> (has_fact(oi, or, rows.take(k), i, r) || (
        eqv(oi, rows[k].0, i) && eqv(or, rows[k].1, r))),
{
    let a = rows.take(k + 1);
    let b = rows.take(k);
    if has_fact(oi, or, a, i, r) {
        let m = choose|m: int| 0 <= m < a.len() && eqv(oi, (#[trigger] a[m]).0, i) && eqv(or, a[m].1, r);
        if m < k {
            assert(b[m] == a[m]);
        }
    }
    if has_fact(oi, or, b, i, r) {
        let m = choose|m: int| 0 <= m < b.len() && eqv(oi, (#[trigger] b[m]).0, i) && eqv(or, b[m].1, r);
        assert(a[m] == b[m]);
    }
    if eqv(oi, rows[k].0, i) && eqv(or, rows[k].1, r) {
        assert(a[k] == rows[k]);
    }
}

proof fn lemma_has_fact_none<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    rows: Seq<(I, R)>,
    i: I,
    r: R,
)
    ensures
        !has_fact(oi, or, rows.take(0), i, r),
{
}

/// `has_fact` depends on the key and the fact only up to equivalence.
proof fn lemma_has_fact_congr<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    rows: Seq<(I, R)>,
    i: I,
    j: I,
    r: R,
)
    requires
        total_order(oi),
        eqv(oi, i, j),
    ensures
        has_fact(oi, or, rows, i, r) <==> has_fact(oi, or, rows, j, r),
{
    lemma_eqv_symm(oi, i, j);
    if has_fact(oi, or, rows, i, r) {
        let k = choose|k: int| 0 <= k < rows.len() && eqv(oi, (#[trigger] rows[k]).0, i) && eqv(or, rows[k].1, r);
        lemma_trans(oi, rows[k].0, i, j);
    }
    if has_fact(oi, or, rows, j, r) {
        let k = choose|k: int| 0 <= k < rows.len() && eqv(oi, (#[trigger] rows[k]).0, j) && eqv(or, rows[k].1, r);
        lemma_trans(oi, rows[k].0, j, i);
    }
}

// ---------------------------------------------------------------------------
// Grouping one side.

/// What grouping knows before row `k`: groups `base..` are those of `rows[..k]`.
pub open spec fn grouped<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    base: int,
    keys: Seq<I>,
    starts: Seq<usize>,
    facts: Seq<R>,
    rows: Seq<(I, R)>,
    k: int,
) -> bool {
    &&& 0 <= base <= keys.len()
    &&& shape(or, keys.len() as int, starts, facts)
    &&& side_wf(oi, or, keys, starts, facts, base, keys.len() as int, rows.take(k))
}

/// A group is unchanged when its bounds are, and `facts` only grows.
proof fn lemma_group_same<R>(starts: Seq<usize>, facts: Seq<R>, starts1: Seq<usize>, facts1: Seq<R>, g: int)
    requires
        0 <= g,
        g + 1 < starts.len(),
        g + 1 < starts1.len(),
        starts1[g] == starts[g],
        starts1[g + 1] == starts[g + 1],
        starts[g] <= starts[g + 1] <= facts.len(),
        facts.len() <= facts1.len(),
        forall|m: int| 0 <= m < facts.len() ==> facts1[m] == facts[m],
    ensures
        group_of(starts1, facts1, g) == group_of(starts, facts, g),
{
    assert(group_of(starts1, facts1, g) =~= group_of(starts, facts, g));
}

/// Row `k` opens a new group: its key is past every key so far.
proof fn lemma_open_group<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    base: int,
    keys: Seq<I>,
    starts: Seq<usize>,
    facts: Seq<R>,
    rows: Seq<(I, R)>,
    k: int,
    end: usize,
)
    requires
        total_order(oi),
        total_order(or),
        0 <= k < rows.len(),
        grouped(oi, or, base, keys, starts, facts, rows, k),
        keys.len() == base || lt(oi, keys.last(), rows[k].0),
        end == facts.len() + 1,
    ensures
        grouped(oi, or, base, keys.push(rows[k].0), starts.push(end), facts.push(rows[k].1), rows, k + 1),
{
    let (i, r) = (rows[k].0, rows[k].1);
    let n = keys.len() as int;
    let keys1 = keys.push(i);
    let facts1 = facts.push(r);
    let starts1 = starts.push(end);
    let (prefix, next) = (rows.take(k), rows.take(k + 1));
    assert forall|g: int| base <= g < n implies lt(oi, #[trigger] keys[g], i) by {
        if g < n - 1 {
            assert(lt(oi, keys[g], keys[n - 1]));
            lemma_trans(oi, keys[g], keys[n - 1], i);
        }
    }
    assert forall|g: int| 0 <= g < n implies #[trigger] group_of(starts1, facts1, g) == group_of(starts, facts, g) by {
        assert(starts[g] < starts[g + 1]);
        assert(starts[g + 1] <= facts.len());
        lemma_group_same(starts, facts, starts1, facts1, g);
    }
    let g_new = group_of(starts1, facts1, n);
    assert(g_new =~= seq![r]);
    assert(shape(or, n + 1, starts1, facts1)) by {
        assert forall|g: int| 0 <= g < n + 1 implies strictly_sorted(or, #[trigger] group_of(starts1, facts1, g)) by {
            if g < n {
                assert(group_of(starts1, facts1, g) == group_of(starts, facts, g));
            }
        }
        assert forall|g: int| 0 <= g <= n + 1 implies #[trigger] starts1[g] <= facts1.len() by {
            if g <= n {
                assert(starts1[g] == starts[g]);
            }
        }
        assert forall|g: int| 0 <= g < n + 1 implies #[trigger] starts1[g] < starts1[g + 1] by {
            if g < n {
                assert(starts1[g] == starts[g]);
                assert(starts1[g + 1] == starts[g + 1]);
            }
        }
    }
    assert forall|p: int, q: int| base <= p < q < n + 1 implies lt(oi, #[trigger] keys1[p], #[trigger] keys1[q]) by {
        if q < n {
            assert(lt(oi, keys[p], keys[q]));
        } else {
            assert(lt(oi, keys[p], i));
        }
    }
    lemma_refl(oi, i);
    assert forall|g: int, v: R|
        #![trigger has_fact(oi, or, next, keys1[g], v)]
        #![trigger mem(or, group_of(starts1, facts1, g), v)]
        base <= g < n + 1 implies (has_fact(oi, or, next, keys1[g], v) <==> mem(or, group_of(starts1, facts1, g), v)) by {
        lemma_has_fact_step(oi, or, rows, k, keys1[g], v);
        if g < n {
            assert(keys1[g] == keys[g]);
            assert(lt(oi, keys[g], i));
            assert(oi(keys[g], i) == Ordering::Less <==> oi(i, keys[g]) == Ordering::Greater);
            assert(has_fact(oi, or, prefix, keys[g], v) <==> mem(or, group_of(starts, facts, g), v));
            assert(group_of(starts1, facts1, g) == group_of(starts, facts, g));
        } else {
            if has_fact(oi, or, prefix, i, v) {
                let h = choose|h: int| base <= h < n && eqv(oi, #[trigger] keys[h], i);
                assert(lt(oi, keys[h], i));
            }
            if mem(or, g_new, v) {
                let m = choose|m: int| 0 <= m < g_new.len() && eqv(or, #[trigger] g_new[m], v);
                assert(m == 0);
            }
            if eqv(or, r, v) {
                assert(g_new[0] == r);
            }
        }
    }
    assert forall|j: I, v: R| #[trigger] has_fact(oi, or, next, j, v) implies exists|g: int|
        base <= g < n + 1 && eqv(oi, #[trigger] keys1[g], j) by {
        lemma_has_fact_step(oi, or, rows, k, j, v);
        if has_fact(oi, or, prefix, j, v) {
            let h = choose|h: int| base <= h < n && eqv(oi, #[trigger] keys[h], j);
            assert(keys1[h] == keys[h]);
        } else {
            assert(keys1[n] == i);
        }
    }
}

/// Row `k` is about the last group's key, and its fact is past the group's last fact.
proof fn lemma_push_fact<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    base: int,
    keys: Seq<I>,
    starts: Seq<usize>,
    facts: Seq<R>,
    rows: Seq<(I, R)>,
    k: int,
    end: usize,
)
    requires
        total_order(oi),
        total_order(or),
        0 <= k < rows.len(),
        grouped(oi, or, base, keys, starts, facts, rows, k),
        keys.len() > base,
        eqv(oi, keys.last(), rows[k].0),
        facts.len() > 0,
        lt(or, facts.last(), rows[k].1),
        end == facts.len() + 1,
    ensures
        grouped(oi, or, base, keys, starts.update(keys.len() as int, end), facts.push(rows[k].1), rows, k + 1),
{
    let (i, r) = (rows[k].0, rows[k].1);
    let n = keys.len() as int;
    let facts1 = facts.push(r);
    let starts1 = starts.update(n, end);
    let (prefix, next) = (rows.take(k), rows.take(k + 1));
    let cur = group_of(starts, facts, n - 1);
    assert(starts[n - 1] < starts[n]);
    assert(cur.last() == facts.last());
    let g_new = group_of(starts1, facts1, n - 1);
    assert(g_new =~= cur.push(r));
    lemma_eqv_symm(oi, keys[n - 1], i);
    assert forall|g: int| 0 <= g < n - 1 implies #[trigger] group_of(starts1, facts1, g) == group_of(starts, facts, g) by {
        assert(starts[g] < starts[g + 1]);
        assert(starts[g + 1] <= facts.len());
        lemma_group_same(starts, facts, starts1, facts1, g);
    }
    assert(strictly_sorted(or, g_new)) by {
        assert(strictly_sorted(or, cur));
        assert forall|p: int, q: int| 0 <= p < q < g_new.len() implies lt(or, #[trigger] g_new[p], #[trigger] g_new[q]) by {
            if q == g_new.len() - 1 {
                if p < cur.len() - 1 {
                    assert(lt(or, cur[p], cur[cur.len() - 1]));
                    lemma_trans(or, cur[p], cur.last(), r);
                }
            } else {
                assert(lt(or, cur[p], cur[q]));
            }
        }
    }
    assert(shape(or, n, starts1, facts1)) by {
        assert forall|g: int| 0 <= g < n implies strictly_sorted(or, #[trigger] group_of(starts1, facts1, g)) by {
            if g < n - 1 {
                assert(group_of(starts1, facts1, g) == group_of(starts, facts, g));
            }
        }
        assert forall|g: int| 0 <= g <= n implies #[trigger] starts1[g] <= facts1.len() by {
            if g < n {
                assert(starts1[g] == starts[g]);
            }
        }
        assert forall|g: int| 0 <= g < n implies #[trigger] starts1[g] < starts1[g + 1] by {
            assert(starts1[g] == starts[g]);
            if g < n - 1 {
                assert(starts1[g + 1] == starts[g + 1]);
            }
        }
    }
    assert forall|g: int, v: R|
        #![trigger has_fact(oi, or, next, keys[g], v)]
        #![trigger mem(or, group_of(starts1, facts1, g), v)]
        base <= g < n implies (has_fact(oi, or, next, keys[g], v) <==> mem(or, group_of(starts1, facts1, g), v)) by {
        lemma_has_fact_step(oi, or, rows, k, keys[g], v);
        assert(has_fact(oi, or, prefix, keys[g], v) <==> mem(or, group_of(starts, facts, g), v));
        if g < n - 1 {
            assert(group_of(starts1, facts1, g) == group_of(starts, facts, g));
            assert(lt(oi, keys[g], keys[n - 1]));
            lemma_trans(oi, keys[g], keys[n - 1], i);
            assert(oi(keys[g], i) == Ordering::Less <==> oi(i, keys[g]) == Ordering::Greater);
        } else {
            if mem(or, cur, v) {
                let m = choose|m: int| 0 <= m < cur.len() && eqv(or, #[trigger] cur[m], v);
                assert(g_new[m] == cur[m]);
            }
            if mem(or, g_new, v) {
                let m = choose|m: int| 0 <= m < g_new.len() && eqv(or, #[trigger] g_new[m], v);
                if m < cur.len() {
                    assert(cur[m] == g_new[m]);
                }
            }
            if eqv(or, r, v) {
                assert(g_new[cur.len() as int] == r);
            }
        }
    }
    assert forall|j: I, v: R| #[trigger] has_fact(oi, or, next, j, v) implies exists|g: int|
        base <= g < n && eqv(oi, #[trigger] keys[g], j) by {
        lemma_has_fact_step(oi, or, rows, k, j, v);
        if !has_fact(oi, or, prefix, j, v) {
            lemma_trans(oi, keys[n - 1], i, j);
        }
    }
}

/// Row `k` repeats a fact the last group, of its key, already holds.
proof fn lemma_repeat_fact<I, R>(
    oi: spec_fn(I, I) -> Ordering,
    or: spec_fn(R, R) -> Ordering,
    base: int,
    keys: Seq<I>,
    starts: Seq<usize>,
    facts: Seq<R>,
    rows: Seq<(I, R)>,
    k: int,
)
    requires
        total_order(oi),
        total_order(or),
        0 <= k < rows.len(),
        grouped(oi, or, base, keys, starts, facts, rows, k),
        keys.len() > base,
        eqv(oi, keys.last(), rows[k].0),
        facts.len() > 0,
        eqv(or, facts.last(), rows[k].1),
    ensures
        grouped(oi, or, base, keys, starts, facts, rows, k + 1),
{
    let (i, r) = (rows[k].0, rows[k].1);
    let n = keys.len() as int;
    let (prefix, next) = (rows.take(k), rows.take(k + 1));
    let cur = group_of(starts, facts, n - 1);
    assert(starts[n - 1] < starts[n]);
    assert(cur.last() == facts.last());
    lemma_eqv_symm(oi, keys[n - 1], i);
    assert forall|g: int, v: R|
        #![trigger has_fact(oi, or, next, keys[g], v)]
        #![trigger mem(or, group_of(starts, facts, g), v)]
        base <= g < n implies (has_fact(oi, or, next, keys[g], v) <==> mem(or, group_of(starts, facts, g), v)) by {
        lemma_has_fact_step(oi, or, rows, k, keys[g], v);
        assert(has_fact(oi, or, prefix, keys[g], v) <==> mem(or, group_of(starts, facts, g), v));
        if g < n - 1 {
            assert(lt(oi, keys[g], keys[n - 1]));
            lemma_trans(oi, keys[g], keys[n - 1], i);
            assert(oi(keys[g], i) == Ordering::Less <==> oi(i, keys[g]) == Ordering::Greater);
        } else if eqv(or, r, v) {
            lemma_trans(or, cur.last(), r, v);
            assert(cur[cur.len() - 1] == cur.last());
        }
    }
    assert forall|j: I, v: R| #[trigger] has_fact(oi, or, next, j, v) implies exists|g: int|
        base <= g < n && eqv(oi, #[trigger] keys[g], j) by {
        lemma_has_fact_step(oi, or, rows, k, j, v);
        if !has_fact(oi, or, prefix, j, v) {
            lemma_trans(oi, keys[n - 1], i, j);
        }
    }
}

/// Appends the groups of `rows`, which must be sorted by key and then by fact, to `keys`,
/// `starts` and `facts`, dropping repeated facts. Returns false, and leaves the groups in no
/// particular state, if `rows` is not sorted.
fn group_side<I: Copy, R: Copy, CI: Fn(&I, &I) -> Ordering, CR: Fn(&R, &R) -> Ordering>(
    keys: &mut Vec<I>,
    starts: &mut Vec<usize>,
    facts: &mut Vec<R>,
    rows: &[(I, R)],
    cmp_i: &CI,
    cmp_r: &CR,
    Ghost(oi): Ghost<spec_fn(I, I) -> Ordering>,
    Ghost(or): Ghost<spec_fn(R, R) -> Ordering>,
) -> (ok: bool)
    requires
        total_order(oi),
        total_order(or),
        computes(*cmp_i, oi),
        computes(*cmp_r, or),
        shape(or, old(keys)@.len() as int, old(starts)@, old(facts)@),
    ensures
        ok ==> final(keys)@.len() >= old(keys)@.len(),
        ok ==> final(keys)@.subrange(0, old(keys)@.len() as int) == old(keys)@,
        ok ==> final(starts)@.subrange(0, old(keys)@.len() as int + 1) == old(starts)@,
        ok ==> final(facts)@.len() >= old(facts)@.len(),
        ok ==> final(facts)@.subrange(0, old(facts)@.len() as int) == old(facts)@,
        ok ==> shape(or, final(keys)@.len() as int, final(starts)@, final(facts)@),
        ok ==> side_wf(oi, or, final(keys)@, final(starts)@, final(facts)@, old(keys)@.len() as int, final(keys)@.len() as int, rows@),
{
    let base = keys.len();
    let ghost facts0 = facts@.len();
    let mut k: usize = 0;
    proof {
        assert forall|i: I, r: R| #[trigger] has_fact(oi, or, rows@.take(0), i, r) implies exists|g: int|
            base <= g < keys@.len() && eqv(oi, #[trigger] keys@[g], i) by {
            lemma_has_fact_none(oi, or, rows@, i, r);
        }
        assert(keys@.subrange(0, base as int) =~= keys@);
        assert(starts@.subrange(0, base as int + 1) =~= starts@);
        assert(facts@.subrange(0, facts0 as int) =~= facts@);
    }
    while k < rows.len()
        invariant
            total_order(oi),
            total_order(or),
            computes(*cmp_i, oi),
            computes(*cmp_r, or),
            base == old(keys)@.len(),
            facts0 == old(facts)@.len(),
            k <= rows@.len(),
            grouped(oi, or, base as int, keys@, starts@, facts@, rows@, k as int),
            keys@.subrange(0, base as int) == old(keys)@,
            starts@.subrange(0, base as int + 1) == old(starts)@,
            facts@.len() >= facts0,
            facts@.subrange(0, facts0 as int) == old(facts)@,
        decreases rows@.len() - k,
    {
        let row = &rows[k];
        let i = row.0;
        let r = row.1;
        let n = keys.len();
        let ghost (keys0, starts0, facts_0) = (keys@, starts@, facts@);
        assert(rows@[k as int] == (i, r));
        let open_group = if n == base {
            true
        } else {
            match cmp_i(&keys[n - 1], &i) {
                Ordering::Less => true,
                Ordering::Equal => false,
                Ordering::Greater => {
                    return false;
                },
            }
        };
        if open_group {
            keys.push(i);
            facts.push(r);
            starts.push(facts.len());
            proof {
                lemma_open_group(oi, or, base as int, keys0, starts0, facts_0, rows@, k as int, facts@.len() as usize);
                assert(keys@.subrange(0, base as int) =~= keys0.subrange(0, base as int));
                assert(starts@.subrange(0, base as int + 1) =~= starts0.subrange(0, base as int + 1));
                assert(facts@.subrange(0, facts0 as int) =~= facts_0.subrange(0, facts0 as int));
            }
        } else {
            proof {
                assert(starts@[n - 1] < starts@[n as int]);
            }
            let last = facts.len() - 1;
            match cmp_r(&facts[last], &r) {
                Ordering::Less => {
                    facts.push(r);
                    starts.set(n, facts.len());
                    proof {
                        lemma_push_fact(oi, or, base as int, keys0, starts0, facts_0, rows@, k as int, facts@.len() as usize);
                        assert(starts@.subrange(0, base as int + 1) =~= starts0.subrange(0, base as int + 1));
                        assert(facts@.subrange(0, facts0 as int) =~= facts_0.subrange(0, facts0 as int));
                    }
                },
                Ordering::Equal => {
                    proof {
                        lemma_repeat_fact(oi, or, base as int, keys@, starts@, facts@, rows@, k as int);
                    }
                },
                Ordering::Greater => {
                    return false;
                },
            }
        }
        k += 1;
    }
    assert(rows@.take(rows@.len() as int) =~= rows@);
    true
}

// ---------------------------------------------------------------------------
// Comparing two groups, and finding a key.

/// `lex` of `facts[a..a_end]` and `facts[b..b_end]`.
fn lex_cmp<R, CR: Fn(&R, &R) -> Ordering>(
    facts: &[R],
    a: usize,
    a_end: usize,
    b: usize,
    b_end: usize,
    cmp: &CR,
    Ghost(or): Ghost<spec_fn(R, R) -> Ordering>,
) -> (o: Ordering)
    requires
        a <= a_end <= facts@.len(),
        b <= b_end <= facts@.len(),
        computes(*cmp, or),
    ensures
        o == lex(or, facts@.subrange(a as int, a_end as int), facts@.subrange(b as int, b_end as int)),
{
    let ghost s = facts@.subrange(a as int, a_end as int);
    let ghost t = facts@.subrange(b as int, b_end as int);
    let mut m: usize = 0;
    assert(s.skip(0) =~= s);
    assert(t.skip(0) =~= t);
    while a + m < a_end && b + m < b_end
        invariant
            a <= a_end <= facts@.len(),
            b <= b_end <= facts@.len(),
            a + m <= a_end,
            b + m <= b_end,
            computes(*cmp, or),
            s == facts@.subrange(a as int, a_end as int),
            t == facts@.subrange(b as int, b_end as int),
            lex(or, s, t) == lex(or, s.skip(m as int), t.skip(m as int)),
        decreases a_end - a - m,
    {
        let c = cmp(&facts[a + m], &facts[b + m]);
        assert(s.skip(m as int)[0] == facts@[a + m]);
        assert(t.skip(m as int)[0] == facts@[b + m]);
        assert(s.skip(m as int).drop_first() =~= s.skip(m + 1));
        assert(t.skip(m as int).drop_first() =~= t.skip(m + 1));
        match c {
            Ordering::Equal => {},
            _ => {
                return c;
            },
        }
        m += 1;
    }
    if a + m == a_end {
        if b + m == b_end {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else {
        Ordering::Greater
    }
}

/// The group in `lo..hi` whose key is equivalent to `i`, if any, by binary search.
fn find<I, CI: Fn(&I, &I) -> Ordering>(
    keys: &[I],
    lo: usize,
    hi: usize,
    i: &I,
    cmp: &CI,
    Ghost(oi): Ghost<spec_fn(I, I) -> Ordering>,
) -> (r: Option<usize>)
    requires
        lo <= hi <= keys@.len(),
        total_order(oi),
        computes(*cmp, oi),
        forall|p: int, q: int| lo <= p < q < hi ==> lt(oi, #[trigger] keys@[p], #[trigger] keys@[q]),
    ensures
        r matches Some(g) ==> lo <= g < hi && eqv(oi, keys@[g as int], *i),
        r is None ==> forall|g: int| lo <= g < hi ==> !eqv(oi, #[trigger] keys@[g], *i),
{
    let mut l = lo;
    let mut h = hi;
    while l < h
        invariant
            lo <= l <= h <= hi <= keys@.len(),
            total_order(oi),
            computes(*cmp, oi),
            forall|p: int, q: int| lo <= p < q < hi ==> lt(oi, #[trigger] keys@[p], #[trigger] keys@[q]),
            forall|g: int| lo <= g < l ==> lt(oi, #[trigger] keys@[g], *i),
            forall|g: int| h <= g < hi ==> lt(oi, *i, #[trigger] keys@[g]),
        decreases h - l,
    {
        let mid = l + (h - l) / 2;
        match cmp(&keys[mid], i) {
            Ordering::Less => {
                proof {
                    assert forall|g: int| lo <= g < mid + 1 implies lt(oi, #[trigger] keys@[g], *i) by {
                        if g < mid {
                            lemma_trans(oi, keys@[g], keys@[mid as int], *i);
                        }
                    }
                }
                l = mid + 1;
            },
            Ordering::Greater => {
                proof {
                    assert(oi(keys@[mid as int], *i) == Ordering::Greater <==> oi(*i, keys@[mid as int]) == Ordering::Less);
                    assert forall|g: int| mid <= g < hi implies lt(oi, *i, #[trigger] keys@[g]) by {
                        if g > mid {
                            lemma_trans(oi, *i, keys@[mid as int], keys@[g]);
                        }
                    }
                }
                h = mid;
            },
            Ordering::Equal => {
                return Some(mid);
            },
        }
    }
    proof {
        assert forall|g: int| lo <= g < hi implies !eqv(oi, #[trigger] keys@[g], *i) by {
            if g < l {
                assert(lt(oi, keys@[g], *i));
            } else {
                assert(lt(oi, *i, keys@[g]));
                assert(oi(*i, keys@[g]) == Ordering::Less <==> oi(keys@[g], *i) == Ordering::Greater);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Ranking the groups by their facts.

/// The groups' order, by `lex` of their facts. Not verified: `build` checks the result.
#[verifier::external_body]
fn sort_groups<R, CR: Fn(&R, &R) -> Ordering>(
    facts: &[R],
    starts: &[usize],
    n: usize,
    cmp: &CR,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by(|&x, &y| {
        let (s, t) = (&facts[starts[x]..starts[x + 1]], &facts[starts[y]..starts[y + 1]]);
        for (a, b) in s.iter().zip(t) {
            match cmp(a, b) {
                Ordering::Equal => {},
                o => return o,
            }
        }
        s.len().cmp(&t.len())
    });
    order
}

/// Where each of `0..n` is in `order`, if `order` is a permutation of `0..n`.
fn positions(order: &[usize], n: usize) -> (r: Option<Vec<usize>>)
    requires
        order@.len() == n,
    ensures
        r matches Some(pos) ==> {
            &&& pos.len() == n
            &&& forall|p: int| 0 <= p < n ==> #[trigger] order@[p] < n && pos[order@[p] as int] == p
            &&& forall|x: int| 0 <= x < n ==> #[trigger] pos[x] < n && order@[pos[x] as int] == x
        },
{
    let mut pos: Vec<usize> = Vec::new();
    let mut x: usize = 0;
    while x < n
        invariant
            x <= n,
            pos.len() == x,
            forall|y: int| 0 <= y < x ==> #[trigger] pos[y] == n,
        decreases n - x,
    {
        pos.push(n);
        x += 1;
    }
    let mut k: usize = 0;
    while k < n
        invariant
            k <= n,
            order@.len() == n,
            pos.len() == n,
            forall|p: int| 0 <= p < k ==> #[trigger] order@[p] < n && pos[order@[p] as int] == p,
            forall|y: int| 0 <= y < n ==> (#[trigger] pos[y] == n || (pos[y] < k && order@[pos[y] as int] == y)),
        decreases n - k,
    {
        let y = order[k];
        if y >= n || pos[y] != n {
            return None;
        }
        pos.set(y, k);
        k += 1;
    }
    let mut x: usize = 0;
    while x < n
        invariant
            x <= n,
            pos.len() == n,
            order@.len() == n,
            forall|p: int| 0 <= p < n ==> #[trigger] order@[p] < n && pos[order@[p] as int] == p,
            forall|y: int| 0 <= y < n ==> (#[trigger] pos[y] == n || (pos[y] < n && order@[pos[y] as int] == y)),
            forall|y: int| 0 <= y < x ==> #[trigger] pos[y] < n,
        decreases n - x,
    {
        if pos[x] == n {
            return None;
        }
        x += 1;
    }
    Some(pos)
}

pub proof fn lemma_lex_trans<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>, u: Seq<T>)
    requires
        total_order(ord),
    ensures
        lex(ord, s, t) != Ordering::Greater && lex(ord, t, u) != Ordering::Greater ==> lex(ord, s, u)
            != Ordering::Greater,
        lex(ord, s, t) == Ordering::Less && lex(ord, t, u) != Ordering::Greater ==> lex(ord, s, u)
            == Ordering::Less,
        lex(ord, s, t) != Ordering::Greater && lex(ord, t, u) == Ordering::Less ==> lex(ord, s, u)
            == Ordering::Less,
        lex(ord, s, t) == Ordering::Equal && lex(ord, t, u) == Ordering::Equal ==> lex(ord, s, u)
            == Ordering::Equal,
        lex(ord, s, t) == Ordering::Less <==> lex(ord, t, s) == Ordering::Greater,
        lex(ord, s, t) == Ordering::Equal ==> lex(ord, t, s) == Ordering::Equal,
        lex(ord, s, s) == Ordering::Equal,
{
    lemma_lex_total(ord);
    let l = lex_ord(ord);
    lemma_trans(l, s, t, u);
    lemma_refl(l, s);
    assert(l(s, t) == lex(ord, s, t));
    assert(l(t, u) == lex(ord, t, u));
    assert(l(s, u) == lex(ord, s, u));
    assert(l(t, s) == lex(ord, t, s));
    assert(l(s, s) == lex(ord, s, s));
}

/// Each group's id: its rank among the groups' distinct sets of facts, from 1. Returns the ids
/// and, for each id from 1, a group that has it.
fn rank<R, CR: Fn(&R, &R) -> Ordering>(
    facts: &[R],
    starts: &[usize],
    order: &[usize],
    Ghost(pos): Ghost<Seq<usize>>,
    n: usize,
    cmp: &CR,
    Ghost(or): Ghost<spec_fn(R, R) -> Ordering>,
) -> (r: Option<(Vec<u32>, Vec<usize>)>)
    requires
        total_order(or),
        computes(*cmp, or),
        n < u32::MAX,
        shape(or, n as int, starts@, facts@),
        order@.len() == n,
        pos.len() == n,
        forall|p: int| 0 <= p < n ==> #[trigger] order@[p] < n && pos[order@[p] as int] == p,
        forall|x: int| 0 <= x < n ==> #[trigger] pos[x] < n && order@[pos[x] as int] == x,
    ensures
        r matches Some((ids, reps)) ==> ids_wf(or, n as int, starts@, facts@, ids@) && reps_wf(n as int, ids@, reps@),
{
    let ghost gr = |x: int| group_of(starts@, facts@, x);
    let mut ids: Vec<u32> = Vec::new();
    let mut x: usize = 0;
    while x < n
        invariant
            x <= n,
            ids@.len() == x,
            forall|y: int| 0 <= y < x ==> #[trigger] ids@[y] == 0,
        decreases n - x,
    {
        ids.push(0);
        x += 1;
    }
    let mut reps: Vec<usize> = Vec::new();
    let mut rank: u32 = 0;
    let mut k: usize = 0;
    while k < n
        invariant
            total_order(or),
            computes(*cmp, or),
            n < u32::MAX,
            shape(or, n as int, starts@, facts@),
            order@.len() == n,
            pos.len() == n,
            forall|p: int| 0 <= p < n ==> #[trigger] order@[p] < n && pos[order@[p] as int] == p,
            gr == (|x: int| group_of(starts@, facts@, x)),
            k <= n,
            ids@.len() == n,
            rank <= k,
            k > 0 ==> rank >= 1,
            k > 0 ==> ids@[order@[k - 1] as int] == rank,
            forall|p: int| k <= p < n ==> #[trigger] ids@[order@[p] as int] == 0,
            reps@.len() == rank,
            forall|q: int| 0 <= q < rank ==> #[trigger] reps@[q] < n && ids@[reps@[q] as int] == q + 1,
            forall|p: int| 0 <= p < k ==> 1 <= #[trigger] ids@[order@[p] as int] <= rank,
            forall|p: int|
                0 <= p < k ==> lex(or, gr(#[trigger] order@[p] as int), gr(order@[k - 1] as int))
                    != Ordering::Greater,
            forall|p: int, q: int|
                0 <= p < k && 0 <= q < k ==> (ids@[#[trigger] order@[p] as int] == ids@[#[trigger] order@[q] as int]
                    <==> lex(or, gr(order@[p] as int), gr(order@[q] as int)) == Ordering::Equal),
            forall|p: int, q: int|
                0 <= p < k && 0 <= q < k ==> (ids@[#[trigger] order@[p] as int] < ids@[#[trigger] order@[q] as int]
                    <==> lex(or, gr(order@[p] as int), gr(order@[q] as int)) == Ordering::Less),
        decreases n - k,
    {
        let x = order[k];
        let ghost ids0 = ids@;
        let ghost rank0 = rank;
        let new_rank = if k == 0 {
            true
        } else {
            let y = order[k - 1];
            proof {
                assert(starts@[y as int] < starts@[y + 1]);
                assert(starts@[x as int] < starts@[x + 1]);
            }
            match lex_cmp(facts, starts[y], starts[y + 1], starts[x], starts[x + 1], cmp, Ghost(or)) {
                Ordering::Less => true,
                Ordering::Equal => false,
                Ordering::Greater => {
                    return None;
                },
            }
        };
        let ghost reps0 = reps@;
        if new_rank {
            rank += 1;
            reps.push(x);
        }
        ids.set(x, rank);
        proof {
            assert forall|p: int| k < p < n implies #[trigger] ids@[order@[p] as int] == 0 by {
                assert(pos[order@[p] as int] == p);
                assert(pos[x as int] == k);
            }
            assert forall|q: int| 0 <= q < rank implies #[trigger] reps@[q] < n && ids@[reps@[q] as int] == q + 1 by {
                if q < rank0 {
                    assert(reps@[q] == reps0[q]);
                    assert(ids0[reps0[q] as int] == q + 1);
                    assert(ids0[x as int] == 0);
                }
            }
            let gx = gr(x as int);
            // `x` is new: no earlier position holds it.
            assert forall|p: int| 0 <= p < k implies #[trigger] order@[p] != x by {
                assert(pos[order@[p] as int] == p);
                assert(pos[x as int] == k);
            }
            assert forall|p: int| 0 <= p < k implies ids@[#[trigger] order@[p] as int] == ids0[order@[p] as int] by {
                assert(order@[p] != x);
            }
            if k > 0 {
                let gy = gr(order@[k - 1] as int);
                assert forall|p: int| 0 <= p < k implies {
                    let gp = gr(#[trigger] order@[p] as int);
                    &&& lex(or, gp, gx) != Ordering::Greater
                    &&& (lex(or, gp, gx) == Ordering::Equal <==> lex(or, gp, gy) == Ordering::Equal
                        && !new_rank)
                    &&& (lex(or, gp, gx) == Ordering::Less <==> (lex(or, gp, gy) == Ordering::Less
                        || new_rank))
                    &&& (lex(or, gx, gp) == Ordering::Less <==> false)
                    &&& (lex(or, gx, gp) == Ordering::Equal <==> lex(or, gp, gx) == Ordering::Equal)
                } by {
                    let gp = gr(order@[p] as int);
                    lemma_lex_trans(or, gp, gy, gx);
                    lemma_lex_trans(or, gx, gy, gp);
                    lemma_lex_trans(or, gp, gx, gy);
                    lemma_lex_trans(or, gy, gx, gp);
                    lemma_lex_trans(or, gx, gp, gy);
                    lemma_lex_trans(or, gy, gp, gx);
                    assert(lex(or, gp, gy) != Ordering::Greater);
                }
            }
            lemma_lex_trans(or, gx, gx, gx);
            assert forall|p: int| 0 <= p < k + 1 implies 1 <= #[trigger] ids@[order@[p] as int] <= rank by {
                if p < k {
                    assert(ids@[order@[p] as int] == ids0[order@[p] as int]);
                }
            }
            assert forall|p: int|
                0 <= p < k + 1 implies lex(or, gr(#[trigger] order@[p] as int), gr(order@[k as int] as int))
                    != Ordering::Greater by {
            }
            assert forall|p: int, q: int|
                0 <= p < k + 1 && 0 <= q < k + 1 implies (ids@[#[trigger] order@[p] as int]
                    == ids@[#[trigger] order@[q] as int] <==> lex(or, gr(order@[p] as int), gr(order@[q] as int))
                    == Ordering::Equal) by {
                if p < k {
                    assert(ids@[order@[p] as int] == ids0[order@[p] as int]);
                }
                if q < k {
                    assert(ids@[order@[q] as int] == ids0[order@[q] as int]);
                }
                if p < k && q == k && k > 0 {
                    assert(ids0[order@[k - 1] as int] == rank0);
                }
                if q < k && p == k && k > 0 {
                    assert(ids0[order@[k - 1] as int] == rank0);
                }
            }
            assert forall|p: int, q: int|
                0 <= p < k + 1 && 0 <= q < k + 1 implies (ids@[#[trigger] order@[p] as int]
                    < ids@[#[trigger] order@[q] as int] <==> lex(or, gr(order@[p] as int), gr(order@[q] as int))
                    == Ordering::Less) by {
                if p < k {
                    assert(ids@[order@[p] as int] == ids0[order@[p] as int]);
                }
                if q < k {
                    assert(ids@[order@[q] as int] == ids0[order@[q] as int]);
                }
                if p < k && q == k && k > 0 {
                    assert(ids0[order@[k - 1] as int] == rank0);
                }
                if q < k && p == k && k > 0 {
                    assert(ids0[order@[k - 1] as int] == rank0);
                }
            }
        }
        k += 1;
    }
    proof {
        assert forall|x: int, y: int| 0 <= x < n && 0 <= y < n implies (#[trigger] ids@[x] == #[trigger] ids@[y]
            <==> lex(or, group_of(starts@, facts@, x), group_of(starts@, facts@, y)) == Ordering::Equal) && (
        ids@[x] < ids@[y] <==> lex(or, group_of(starts@, facts@, x), group_of(starts@, facts@, y))
            == Ordering::Less) by {
            let (p, q) = (pos[x] as int, pos[y] as int);
            assert(order@[p] == x && order@[q] == y);
        }
        assert forall|x: int| 0 <= x < n implies #[trigger] ids@[x] >= 1 && ids@[x] <= reps@.len() by {
            assert(order@[pos[x] as int] == x);
        }
    }
    Some((ids, reps))
}

/// `reps[id - 1]` is a group named `id`, for every name but 0, and every group's name is one of
/// them.
pub open spec fn reps_wf(n: int, ids: Seq<u32>, reps: Seq<usize>) -> bool {
    &&& reps.len() <= n
    &&& forall|x: int| 0 <= x < n ==> #[trigger] ids[x] <= reps.len()
    &&& forall|q: int| 0 <= q < reps.len() ==> #[trigger] reps[q] < n && ids[reps[q] as int] == q + 1
}

/// Group ids: each at least 1, equal exactly when the groups' facts are, and ordered as the
/// groups' facts are.
pub open spec fn ids_wf<R>(or: spec_fn(R, R) -> Ordering, n: int, starts: Seq<usize>, facts: Seq<R>, ids: Seq<u32>) -> bool {
    &&& ids.len() == n
    &&& forall|x: int| 0 <= x < n ==> #[trigger] ids[x] >= 1
    &&& forall|x: int, y: int|
        0 <= x < n && 0 <= y < n ==> {
            &&& (#[trigger] ids[x] == #[trigger] ids[y] <==> lex(or, group_of(starts, facts, x), group_of(starts, facts, y))
                == Ordering::Equal)
            &&& (ids[x] < ids[y] <==> lex(or, group_of(starts, facts, x), group_of(starts, facts, y))
                == Ordering::Less)
        }
}

// ---------------------------------------------------------------------------
// Names.

/// Both sides' keys and the names of their sets of facts. Keys `0..n_a` are side A's, the rest
/// side B's.
#[verifier::reject_recursive_types(I)]
#[verifier::reject_recursive_types(R)]
pub struct Names<I: Copy, R: Copy> {
    keys: Vec<I>,
    starts: Vec<usize>,
    facts: Vec<R>,
    n_a: usize,
    ids: Vec<u32>,
    /// `reps[id - 1]` is a group named `id`.
    reps: Vec<usize>,
    // Read only by the proof; zero-sized.
    #[allow(dead_code)]
    oi: Ghost<spec_fn(I, I) -> Ordering>,
    #[allow(dead_code)]
    or: Ghost<spec_fn(R, R) -> Ordering>,
    #[allow(dead_code)]
    rows_a: Ghost<Seq<(I, R)>>,
    #[allow(dead_code)]
    rows_b: Ghost<Seq<(I, R)>>,
}

impl<I: Copy, R: Copy> Names<I, R> {
    /// What `build` establishes, and the rest relies on.
    pub closed spec fn wf(self) -> bool {
        let n = self.keys@.len() as int;
        &&& total_order(self.oi@)
        &&& total_order(self.or@)
        &&& self.n_a <= n
        &&& n < u32::MAX
        &&& shape(self.or@, n, self.starts@, self.facts@)
        &&& side_wf(self.oi@, self.or@, self.keys@, self.starts@, self.facts@, 0, self.n_a as int, self.rows_a@)
        &&& side_wf(self.oi@, self.or@, self.keys@, self.starts@, self.facts@, self.n_a as int, n, self.rows_b@)
        &&& ids_wf(self.or@, n, self.starts@, self.facts@, self.ids@)
        &&& reps_wf(n, self.ids@, self.reps@)
    }

    /// The order keys are compared in.
    pub closed spec fn ord_i(self) -> spec_fn(I, I) -> Ordering {
        self.oi@
    }

    /// The order facts are compared in.
    pub closed spec fn ord_r(self) -> spec_fn(R, R) -> Ordering {
        self.or@
    }

    /// Side A's rows, or side B's if `side_b`.
    pub closed spec fn rows(self, side_b: bool) -> Seq<(I, R)> {
        if side_b {
            self.rows_b@
        } else {
            self.rows_a@
        }
    }

    closed spec fn lo(self, side_b: bool) -> int {
        if side_b {
            self.n_a as int
        } else {
            0
        }
    }

    closed spec fn hi(self, side_b: bool) -> int {
        if side_b {
            self.keys@.len() as int
        } else {
            self.n_a as int
        }
    }

    closed spec fn present(self, side_b: bool, i: I) -> bool {
        exists|g: int| self.lo(side_b) <= g < self.hi(side_b) && eqv(self.oi@, #[trigger] self.keys@[g], i)
    }

    /// The name of key `i` on side A, or on side B if `side_b`.
    pub closed spec fn spec_name(self, side_b: bool, i: I) -> u32 {
        if self.present(side_b, i) {
            self.ids@[choose|g: int| self.lo(side_b) <= g < self.hi(side_b) && eqv(self.oi@, #[trigger] self.keys@[g], i)]
        } else {
            0
        }
    }

    /// Names the keys of `rows_a` and `rows_b`, each sorted by key and then by fact, by their
    /// sets of facts. `None` if either is not sorted, or there are `u32::MAX` keys or more.
    ///
    /// `cmp_i` and `cmp_r` must be total orders. Keys, and facts, they call `Equal` are the same
    /// key, and the same fact.
    pub fn build<CI: Fn(&I, &I) -> Ordering, CR: Fn(&R, &R) -> Ordering>(
        rows_a: &[(I, R)],
        rows_b: &[(I, R)],
        cmp_i: &CI,
        cmp_r: &CR,
    ) -> (r: Option<Self>)
        requires
            exists|oi: spec_fn(I, I) -> Ordering| total_order(oi) && #[trigger] computes(*cmp_i, oi),
            exists|or: spec_fn(R, R) -> Ordering| total_order(or) && #[trigger] computes(*cmp_r, or),
        ensures
            r matches Some(names) ==> {
                &&& names.wf()
                &&& computes(*cmp_i, names.ord_i())
                &&& computes(*cmp_r, names.ord_r())
                &&& names.rows(false) == rows_a@
                &&& names.rows(true) == rows_b@
            },
    {
        let ghost oi = choose|oi: spec_fn(I, I) -> Ordering| total_order(oi) && #[trigger] computes(*cmp_i, oi);
        let ghost or = choose|or: spec_fn(R, R) -> Ordering| total_order(or) && #[trigger] computes(*cmp_r, or);
        let mut keys: Vec<I> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        starts.push(0);
        let mut facts: Vec<R> = Vec::new();
        proof {
            assert(shape(or, 0, starts@, facts@));
        }
        if !group_side(&mut keys, &mut starts, &mut facts, rows_a, cmp_i, cmp_r, Ghost(oi), Ghost(or)) {
            return None;
        }
        let n_a = keys.len();
        let ghost (keys_a, starts_a, facts_a) = (keys@, starts@, facts@);
        if !group_side(&mut keys, &mut starts, &mut facts, rows_b, cmp_i, cmp_r, Ghost(oi), Ghost(or)) {
            return None;
        }
        let n = keys.len();
        proof {
            // Side A's groups are as they were.
            assert forall|g: int| 0 <= g < n_a implies #[trigger] group_of(starts@, facts@, g)
                == group_of(starts_a, facts_a, g) by {
                assert(starts@[g] == starts_a[g]);
                assert(starts@[g + 1] == starts_a[g + 1]);
                assert(starts_a[g + 1] <= facts_a.len());
                assert(facts@.subrange(0, facts_a.len() as int)[starts_a[g] as int] == facts_a[starts_a[g] as int]);
                assert(group_of(starts@, facts@, g) =~= group_of(starts_a, facts_a, g));
            }
            assert forall|g: int| 0 <= g < n_a implies #[trigger] keys@[g] == keys_a[g] by {
                assert(keys@.subrange(0, n_a as int)[g] == keys_a[g]);
            }
            assert(side_wf(oi, or, keys@, starts@, facts@, 0, n_a as int, rows_a@)) by {
                assert forall|p: int, q: int| 0 <= p < q < n_a implies lt(oi, #[trigger] keys@[p], #[trigger] keys@[q]) by {
                    assert(keys@[p] == keys_a[p] && keys@[q] == keys_a[q]);
                }
                assert forall|g: int, v: R|
                    #![trigger has_fact(oi, or, rows_a@, keys@[g], v)]
                    #![trigger mem(or, group_of(starts@, facts@, g), v)]
                    0 <= g < n_a implies (has_fact(oi, or, rows_a@, keys@[g], v) <==> mem(
                        or,
                        group_of(starts@, facts@, g),
                        v,
                    )) by {
                    assert(keys@[g] == keys_a[g]);
                    assert(group_of(starts@, facts@, g) == group_of(starts_a, facts_a, g));
                    assert(has_fact(oi, or, rows_a@, keys_a[g], v) <==> mem(or, group_of(starts_a, facts_a, g), v));
                }
                assert forall|j: I, v: R| #[trigger] has_fact(oi, or, rows_a@, j, v) implies exists|g: int|
                    0 <= g < n_a && eqv(oi, #[trigger] keys@[g], j) by {
                    let g = choose|g: int| 0 <= g < n_a && eqv(oi, #[trigger] keys_a[g], j);
                    assert(keys@[g] == keys_a[g]);
                }
            }
        }
        if n >= 0xffff_ffff {
            return None;
        }
        let order = sort_groups(&facts, &starts, n, cmp_r);
        if order.len() != n {
            return None;
        }
        // Checks that `order` is a permutation. Only the proof reads where each group is.
        let _pos = match positions(&order, n) {
            Some(pos) => pos,
            None => {
                return None;
            },
        };
        let (ids, reps) = match rank(&facts, &starts, &order, Ghost(_pos@), n, cmp_r, Ghost(or)) {
            Some(r) => r,
            None => {
                return None;
            },
        };
        Some(Names {
            keys,
            starts,
            facts,
            n_a,
            ids,
            reps,
            oi: Ghost(oi),
            or: Ghost(or),
            rows_a: Ghost(rows_a@),
            rows_b: Ghost(rows_b@),
        })
    }

    /// The name of key `i` on side A, or on side B if `side_b`: 0 if it has no facts. `cmp_i`
    /// must be the comparison `build` was given.
    pub fn name<CI: Fn(&I, &I) -> Ordering>(&self, side_b: bool, i: &I, cmp_i: &CI) -> (r: u32)
        requires
            self.wf(),
            computes(*cmp_i, self.ord_i()),
        ensures
            r == self.spec_name(side_b, *i),
    {
        let (lo, hi) = if side_b {
            (self.n_a, self.keys.len())
        } else {
            (0, self.n_a)
        };
        match find(&self.keys, lo, hi, i, cmp_i, Ghost(self.oi@)) {
            Some(g) => {
                proof {
                    let c = choose|c: int| self.lo(side_b) <= c < self.hi(side_b) && eqv(self.oi@, #[trigger] self.keys@[c], *i);
                    assert(self.present(side_b, *i));
                    if c != g {
                        // Two keys equivalent to `i` would be equivalent to each other.
                        lemma_eqv_symm(self.oi@, self.keys@[g as int], *i);
                        lemma_trans(self.oi@, self.keys@[c], *i, self.keys@[g as int]);
                        if c < g {
                            assert(lt(self.oi@, self.keys@[c], self.keys@[g as int]));
                        } else {
                            assert(lt(self.oi@, self.keys@[g as int], self.keys@[c]));
                            lemma_eqv_symm(self.oi@, self.keys@[c], self.keys@[g as int]);
                        }
                    }
                }
                self.ids[g]
            },
            None => 0,
        }
    }

    /// How many names there are: one per distinct non-empty set of facts, and 0.
    pub fn len(&self) -> (r: usize)
        requires
            self.wf(),
        ensures
            r == self.num_names(),
            forall|s: bool, i: I| #[trigger] self.spec_name(s, i) < r,
    {
        proof {
            assert forall|s: bool, i: I| #[trigger] self.spec_name(s, i) < self.num_names() by {
                if self.present(s, i) {
                    let g = choose|g: int| self.lo(s) <= g < self.hi(s) && eqv(self.oi@, #[trigger] self.keys@[g], i);
                    assert(self.ids@[g] <= self.reps@.len());
                }
            }
        }
        self.reps.len() + 1
    }

    /// How many names there are.
    pub closed spec fn num_names(self) -> nat {
        self.reps@.len() + 1
    }

    /// Always false: 0, the name of a key with no facts, is always a name.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The facts named `id`, strictly sorted: empty for 0, and otherwise the facts of a key
    /// named `id`. Not verified.
    #[verifier::external_body]
    pub fn facts_of(&self, id: u32) -> &[R] {
        if id == 0 {
            return &[];
        }
        let g = self.reps[id as usize - 1];
        &self.facts[self.starts[g]..self.starts[g + 1]]
    }

    /// Side `s`'s groups are its rows' keys with their facts.
    proof fn lemma_side(self, s: bool)
        requires
            self.wf(),
        ensures
            0 <= self.lo(s) <= self.hi(s) <= self.keys@.len(),
            side_wf(self.oi@, self.or@, self.keys@, self.starts@, self.facts@, self.lo(s), self.hi(s), self.rows(s)),
    {
    }

    /// A key with a group on side `s` has the group's facts.
    proof fn lemma_present_facts(self, s: bool, i: I) -> (g: int)
        requires
            self.wf(),
            self.present(s, i),
        ensures
            self.lo(s) <= g < self.hi(s),
            g == choose|g: int| self.lo(s) <= g < self.hi(s) && eqv(self.oi@, #[trigger] self.keys@[g], i),
            self.spec_name(s, i) == self.ids@[g],
            self.ids@[g] >= 1,
            strictly_sorted(self.or@, group_of(self.starts@, self.facts@, g)),
            group_of(self.starts@, self.facts@, g).len() > 0,
            forall|v: R|
                #![trigger has_fact(self.oi@, self.or@, self.rows(s), i, v)]
                #![trigger mem(self.or@, group_of(self.starts@, self.facts@, g), v)]
                has_fact(self.oi@, self.or@, self.rows(s), i, v) <==> mem(self.or@, group_of(self.starts@, self.facts@, g), v),
    {
        self.lemma_side(s);
        let g = choose|g: int| self.lo(s) <= g < self.hi(s) && eqv(self.oi@, #[trigger] self.keys@[g], i);
        assert(self.starts@[g] < self.starts@[g + 1]);
        assert forall|v: R|
            #![trigger has_fact(self.oi@, self.or@, self.rows(s), i, v)]
            #![trigger mem(self.or@, group_of(self.starts@, self.facts@, g), v)]
            has_fact(self.oi@, self.or@, self.rows(s), i, v) <==> mem(self.or@, group_of(self.starts@, self.facts@, g), v) by {
            lemma_has_fact_congr(self.oi@, self.or@, self.rows(s), self.keys@[g], i, v);
            assert(has_fact(self.oi@, self.or@, self.rows(s), self.keys@[g], v) <==> mem(
                self.or@,
                group_of(self.starts@, self.facts@, g),
                v,
            ));
        }
        g
    }

    /// A key with no group on side `s` has no facts there.
    proof fn lemma_absent_facts(self, s: bool, i: I, v: R)
        requires
            self.wf(),
            !self.present(s, i),
        ensures
            self.spec_name(s, i) == 0,
            !has_fact(self.oi@, self.or@, self.rows(s), i, v),
    {
        self.lemma_side(s);
        if has_fact(self.oi@, self.or@, self.rows(s), i, v) {
            let g = choose|g: int| self.lo(s) <= g < self.hi(s) && eqv(self.oi@, #[trigger] self.keys@[g], i);
            assert(self.present(s, i));
        }
    }

    /// For keys `i` and `j`, on either side, the names are equal exactly when the keys have the
    /// same facts.
    pub proof fn theorem_names(self, s: bool, i: I, t: bool, j: I)
        requires
            self.wf(),
        ensures
            self.spec_name(s, i) == self.spec_name(t, j) <==> same_facts(
                self.ord_i(),
                self.ord_r(),
                self.rows(s),
                i,
                self.rows(t),
                j,
            ),
    {
        let (oi, or) = (self.oi@, self.or@);
        let (rs, rt) = (self.rows(s), self.rows(t));
        if self.present(s, i) && self.present(t, j) {
            let g = self.lemma_present_facts(s, i);
            let h = self.lemma_present_facts(t, j);
            let (gg, gh) = (group_of(self.starts@, self.facts@, g), group_of(self.starts@, self.facts@, h));
            lemma_set_eq_lex(or, gg, gh);
            assert(self.ids@[g] == self.ids@[h] <==> lex(or, gg, gh) == Ordering::Equal);
            if self.ids@[g] == self.ids@[h] {
                assert forall|v: R| #[trigger] has_fact(oi, or, rs, i, v) <==> has_fact(oi, or, rt, j, v) by {
                    assert(mem(or, gg, v) <==> mem(or, gh, v));
                }
            }
            if same_facts(oi, or, rs, i, rt, j) {
                assert forall|v: R| #[trigger] mem(or, gg, v) <==> mem(or, gh, v) by {
                    assert(has_fact(oi, or, rs, i, v) <==> has_fact(oi, or, rt, j, v));
                }
            }
        } else if self.present(s, i) {
            // `i`'s first fact is not one of `j`'s.
            let g = self.lemma_present_facts(s, i);
            let v = group_of(self.starts@, self.facts@, g)[0];
            lemma_refl(or, v);
            assert(mem(or, group_of(self.starts@, self.facts@, g), v));
            self.lemma_absent_facts(t, j, v);
            assert(has_fact(oi, or, rs, i, v));
        } else if self.present(t, j) {
            let h = self.lemma_present_facts(t, j);
            let v = group_of(self.starts@, self.facts@, h)[0];
            lemma_refl(or, v);
            assert(mem(or, group_of(self.starts@, self.facts@, h), v));
            self.lemma_absent_facts(s, i, v);
            assert(has_fact(oi, or, rt, j, v));
        } else {
            assert forall|v: R| #[trigger] has_fact(oi, or, rs, i, v) <==> has_fact(oi, or, rt, j, v) by {
                self.lemma_absent_facts(s, i, v);
                self.lemma_absent_facts(t, j, v);
            }
        }
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::Names;

    #[test]
    fn fixed_example() {
        // Side A: key 1 has {10, 20} (20 twice); key 2 has {10}. Side B: key 7 has {10, 20};
        // key 9 has {30}.
        let a = [(1u8, 10u8), (1, 20), (1, 20), (2, 10)];
        let b = [(7u8, 10u8), (7, 20), (9, 30)];
        let names = Names::build(&a, &b, &u8::cmp, &u8::cmp).unwrap();
        let name = |side_b, i: u8| names.name(side_b, &i, &u8::cmp);
        assert_eq!(name(false, 1), name(true, 7));
        assert_ne!(name(false, 1), name(false, 2));
        assert_ne!(name(true, 9), name(false, 2));
        assert_eq!(name(false, 5), 0);
        assert_eq!(name(true, 1), 0);
        // Names are ranks by content: {10} < {10, 20} < {30}.
        assert!(name(false, 2) < name(false, 1) && name(false, 1) < name(true, 9));
        assert_eq!(names.len(), 4);
        assert_eq!(names.facts_of(name(false, 1)), [10, 20]);
        assert_eq!(names.facts_of(0), [] as [u8; 0]);
    }

    #[test]
    fn unsorted_rows_are_refused() {
        assert!(Names::build(&[(2u8, 1u8), (1, 1)], &[], &u8::cmp, &u8::cmp).is_none());
        assert!(Names::build(&[(1u8, 2u8), (1, 1)], &[], &u8::cmp, &u8::cmp).is_none());
    }
}
