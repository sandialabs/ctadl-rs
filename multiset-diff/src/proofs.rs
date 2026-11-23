//! The four properties of `diff_from`, and a `u8` client that restates them.

use super::*;
#[cfg(verus_keep_ghost)]
use order_laws::{lemma_eqv_symm, lemma_refl, lemma_trans};

#[cfg(verus_keep_ghost)]
mod client;

verus! {

// ---------------------------------------------------------------------------
// Consequences of the order laws.

/// `run_end` depends on `v` only up to `eqv`.
proof fn lemma_run_end_congr<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, i: int, v: T, w: T)
    requires
        total_order(ord),
        eqv(ord, v, w),
    ensures
        run_end(ord, s, i, v) == run_end(ord, s, i, w),
    decreases s.len() - i,
{
    if 0 <= i < s.len() {
        lemma_eqv_symm(ord, v, w);
        lemma_trans(ord, s[i], v, w);
        lemma_trans(ord, s[i], w, v);
        lemma_run_end_congr(ord, s, i + 1, v, w);
    }
}

// ---------------------------------------------------------------------------
// Property 1: diffing a sequence with itself yields nothing.

proof fn lemma_self_diff<'a, T>(ord: spec_fn(T, T) -> Ordering, a: &'a [T], i: int)
    requires
        total_order(ord),
        0 <= i <= a@.len(),
    ensures
        diff_from(ord, a, a, i, i) == Seq::<Delta<'a, T>>::empty(),
    decreases a@.len() - i,
{
    if i < a@.len() {
        lemma_refl(ord, a@[i]);
        assert(head_from_a(ord, a@, a@, i, i));
        assert(run_end(ord, a@, i, a@[i]) == run_end(ord, a@, i + 1, a@[i]));
        lemma_step(ord, a, a, i, i);
        let (_, ia, _) = step(ord, a, a, i, i);
        lemma_self_diff(ord, a, ia);
    }
}

pub proof fn theorem_self_diff_is_empty<'a, T>(ord: spec_fn(T, T) -> Ordering, a: &'a [T])
    requires
        total_order(ord),
    ensures
        diff_from(ord, a, a, 0, 0) == Seq::<Delta<'a, T>>::empty(),
{
    lemma_self_diff(ord, a, 0);
}

// ---------------------------------------------------------------------------
// Property 2: swapping the inputs yields the same values, in the same order, with the
// counts exchanged.

pub open spec fn swapped<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    d: Seq<Delta<'a, T>>,
    e: Seq<Delta<'a, T>>,
) -> bool {
    &&& d.len() == e.len()
    &&& forall|k: int|
        0 <= k < d.len() ==> {
            &&& eqv(ord, *(#[trigger] d[k]).row, *e[k].row)
            &&& d[k].count_a == e[k].count_b
            &&& d[k].count_b == e[k].count_a
        }
}

proof fn lemma_swap<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
    ensures
        swapped(ord, diff_from(ord, a, b, i, j), diff_from(ord, b, a, j, i)),
    decreases a@.len() - i + b@.len() - j,
{
    if i == a@.len() && j == b@.len() {
        return;
    }
    lemma_step(ord, a, b, i, j);
    lemma_step(ord, b, a, j, i);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let (s, jb2, ia2) = step(ord, b, a, j, i);
    let ab = head_from_a(ord, a@, b@, i, j);
    let ba = head_from_a(ord, b@, a@, j, i);
    if ab && ba {
        // A tie: `a[i]` and `b[j]` are equivalent, and each side's run is the same whichever
        // of them it is measured against.
        assert(ord(a@[i], b@[j]) == Ordering::Less <==> ord(b@[j], a@[i])
            == Ordering::Greater);
        assert(ord(b@[j], a@[i]) == Ordering::Less <==> ord(a@[i], b@[j])
            == Ordering::Greater);
        assert(eqv(ord, a@[i], b@[j]));
        lemma_refl(ord, b@[j]);
        lemma_trans(ord, a@[i], b@[j], b@[j]);
        lemma_run_end_congr(ord, a@, i + 1, a@[i], b@[j]);
        lemma_run_end_congr(ord, b@, j, a@[i], b@[j]);
        lemma_refl(ord, a@[i]);
        lemma_eqv_symm(ord, a@[i], b@[j]);
        assert(run_end(ord, a@, i, b@[j]) == run_end(ord, a@, i + 1, b@[j]));
        assert(run_end(ord, b@, j, a@[i]) == run_end(ord, b@, j + 1, a@[i]));
        lemma_run_end_congr(ord, b@, j + 1, a@[i], b@[j]);
        assert(ia == ia2 && jb == jb2);
        assert(eqv(ord, *r, *s));
    } else if !ab && !ba {
        assert(ord(a@[i], b@[j]) == Ordering::Less <==> ord(b@[j], a@[i])
            == Ordering::Greater);
        assert(false);
    } else {
        // The same element comes first either way.
        lemma_refl(ord, *r);
        assert(ia == ia2 && jb == jb2 && r == s);
    }
    lemma_swap(ord, a, b, ia, jb);
    let d = diff_from(ord, a, b, i, j);
    let e = diff_from(ord, b, a, j, i);
    let d2 = diff_from(ord, a, b, ia, jb);
    let e2 = diff_from(ord, b, a, jb, ia);
    if ia - i != jb - j {
        assert(d == seq![Delta { row: r, count_a: (ia - i) as usize, count_b: (jb - j) as usize }] + d2);
        assert(e == seq![Delta { row: s, count_a: (jb - j) as usize, count_b: (ia - i) as usize }] + e2);
        assert forall|k: int| 0 <= k < d.len() implies {
            &&& eqv(ord, *(#[trigger] d[k]).row, *e[k].row)
            &&& d[k].count_a == e[k].count_b
            &&& d[k].count_b == e[k].count_a
        } by {
            if k > 0 {
                assert(d[k] == d2[k - 1]);
                assert(e[k] == e2[k - 1]);
            }
        }
    } else {
        assert(d == d2);
        assert(e == e2);
    }
}

pub proof fn theorem_swap_swaps_sides<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
    ensures
        swapped(ord, diff_from(ord, a, b, 0, 0), diff_from(ord, b, a, 0, 0)),
{
    lemma_swap(ord, a, b, 0, 0);
}

// ---------------------------------------------------------------------------
// Counting.

proof fn lemma_count_split<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    mid: int,
    hi: int,
    v: T,
)
    requires
        lo <= mid <= hi,
    ensures
        count(ord, s, lo, hi, v) == count(ord, s, lo, mid, v) + count(ord, s, mid, hi, v),
    decreases hi - mid,
{
    if mid < hi {
        lemma_count_split(ord, s, lo, mid, hi - 1, v);
    }
}

proof fn lemma_count_none<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
)
    requires
        forall|k: int| lo <= k < hi ==> !eqv(ord, #[trigger] s[k], v),
    ensures
        count(ord, s, lo, hi, v) == 0,
    decreases hi - lo,
{
    if lo < hi {
        lemma_count_none(ord, s, lo, hi - 1, v);
    }
}

proof fn lemma_count_all<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
)
    requires
        lo <= hi,
        forall|k: int| lo <= k < hi ==> eqv(ord, #[trigger] s[k], v),
    ensures
        count(ord, s, lo, hi, v) == hi - lo,
    decreases hi - lo,
{
    if lo < hi {
        lemma_count_all(ord, s, lo, hi - 1, v);
    }
}

proof fn lemma_count_congr<T>(
    ord: spec_fn(T, T) -> Ordering,
    s: Seq<T>,
    lo: int,
    hi: int,
    v: T,
    w: T,
)
    requires
        total_order(ord),
        eqv(ord, v, w),
    ensures
        count(ord, s, lo, hi, v) == count(ord, s, lo, hi, w),
    decreases hi - lo,
{
    if lo < hi {
        lemma_eqv_symm(ord, v, w);
        lemma_trans(ord, s[hi - 1], v, w);
        lemma_trans(ord, s[hi - 1], w, v);
        lemma_count_congr(ord, s, lo, hi - 1, v, w);
    }
}

// ---------------------------------------------------------------------------
// Properties 3 and 4, on sorted inputs.

/// `x` is above every element of `a[..i]` and `b[..j]`.
pub open spec fn above<T>(
    ord: spec_fn(T, T) -> Ordering,
    a: Seq<T>,
    b: Seq<T>,
    i: int,
    j: int,
    x: T,
) -> bool {
    &&& forall|p: int| 0 <= p < i ==> #[trigger] lt(ord, a[p], x)
    &&& forall|q: int| 0 <= q < j ==> #[trigger] lt(ord, b[q], x)
}

/// Positions `i` and `j` split both inputs: everything before them is below everything after.
pub open spec fn cut<T>(
    ord: spec_fn(T, T) -> Ordering,
    a: Seq<T>,
    b: Seq<T>,
    i: int,
    j: int,
) -> bool {
    &&& forall|p: int| i <= p < a.len() ==> #[trigger] above(ord, a, b, i, j, a[p])
    &&& forall|q: int| j <= q < b.len() ==> #[trigger] above(ord, a, b, i, j, b[q])
}

/// What the merge's step from a cut establishes, on sorted inputs: the value it looks at is
/// above what came before, its runs hold exactly its equivalents, everything after them is
/// above it, and the ends of the runs are a cut again.
proof fn lemma_step_sorted<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
        i < a@.len() || j < b@.len(),
        cut(ord, a@, b@, i, j),
    ensures
        ({
            let (r, ia, jb) = step(ord, a, b, i, j);
            &&& above(ord, a@, b@, i, j, *r)
            &&& forall|p: int| i <= p < ia ==> #[trigger] eqv(ord, a@[p], *r)
            &&& forall|q: int| j <= q < jb ==> #[trigger] eqv(ord, b@[q], *r)
            &&& forall|p: int| ia <= p < a@.len() ==> #[trigger] lt(ord, *r, a@[p])
            &&& forall|q: int| jb <= q < b@.len() ==> #[trigger] lt(ord, *r, b@[q])
            &&& count_all(ord, a@, *r) == ia - i
            &&& count_all(ord, b@, *r) == jb - j
            &&& cut(ord, a@, b@, ia, jb)
        }),
{
    lemma_step(ord, a, b, i, j);
    let from_a = head_from_a(ord, a@, b@, i, j);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let v = *r;
    lemma_refl(ord, v);
    if from_a {
        assert(above(ord, a@, b@, i, j, a@[i]));
        lemma_run_end(ord, a@, i + 1, v);
        lemma_run_end(ord, b@, j, v);
    } else {
        assert(above(ord, a@, b@, i, j, b@[j]));
        lemma_run_end(ord, a@, i, v);
        lemma_run_end(ord, b@, j + 1, v);
    }
    // Everything at or past the run ends is above `v`.
    if ia < a@.len() {
        if from_a {
            assert(le(ord, a@[i], a@[ia]));
        } else {
            // `b[j]` came first, so it is below `a[i]`.
            assert(ord(b@[j], a@[i]) == Ordering::Less <==> ord(a@[i], b@[j])
                == Ordering::Greater);
            if i < ia {
                assert(le(ord, a@[i], a@[ia]));
            }
            lemma_trans(ord, v, a@[i], a@[ia]);
        }
        assert(!eqv(ord, a@[ia], v));
        assert(ord(v, a@[ia]) == Ordering::Equal ==> ord(a@[ia], v) == Ordering::Equal);
        assert(lt(ord, v, a@[ia]));
        assert forall|p: int| ia <= p < a@.len() implies #[trigger] lt(ord, v, a@[p]) by {
            if ia < p {
                assert(le(ord, a@[ia], a@[p]));
            } else {
                lemma_refl(ord, a@[p]);
            }
            lemma_trans(ord, v, a@[ia], a@[p]);
        }
    }
    if jb < b@.len() {
        if !from_a {
            assert(le(ord, b@[j], b@[jb]));
        } else {
            if j < b@.len() {
                if j < jb {
                    assert(le(ord, b@[j], b@[jb]));
                }
                lemma_trans(ord, v, b@[j], b@[jb]);
            }
        }
        assert(!eqv(ord, b@[jb], v));
        assert(ord(v, b@[jb]) == Ordering::Equal ==> ord(b@[jb], v) == Ordering::Equal);
        assert(lt(ord, v, b@[jb]));
        assert forall|q: int| jb <= q < b@.len() implies #[trigger] lt(ord, v, b@[q]) by {
            if jb < q {
                assert(le(ord, b@[jb], b@[q]));
            } else {
                lemma_refl(ord, b@[q]);
            }
            lemma_trans(ord, v, b@[jb], b@[q]);
        }
    }
    // The counts: nothing before the cut or past the runs is equivalent to `v`.
    assert forall|p: int| 0 <= p < i implies !eqv(ord, #[trigger] a@[p], v) by {
        assert(lt(ord, a@[p], v));
    }
    assert forall|p: int| ia <= p < a@.len() implies !eqv(ord, #[trigger] a@[p], v) by {
        assert(lt(ord, v, a@[p]));
        assert(ord(v, a@[p]) == Ordering::Less <==> ord(a@[p], v) == Ordering::Greater);
    }
    assert forall|q: int| 0 <= q < j implies !eqv(ord, #[trigger] b@[q], v) by {
        assert(lt(ord, b@[q], v));
    }
    assert forall|q: int| jb <= q < b@.len() implies !eqv(ord, #[trigger] b@[q], v) by {
        assert(lt(ord, v, b@[q]));
        assert(ord(v, b@[q]) == Ordering::Less <==> ord(b@[q], v) == Ordering::Greater);
    }
    lemma_count_split(ord, a@, 0, i, a@.len() as int, v);
    lemma_count_split(ord, a@, i, ia, a@.len() as int, v);
    lemma_count_none(ord, a@, 0, i, v);
    lemma_count_all(ord, a@, i, ia, v);
    lemma_count_none(ord, a@, ia, a@.len() as int, v);
    lemma_count_split(ord, b@, 0, j, b@.len() as int, v);
    lemma_count_split(ord, b@, j, jb, b@.len() as int, v);
    lemma_count_none(ord, b@, 0, j, v);
    lemma_count_all(ord, b@, j, jb, v);
    lemma_count_none(ord, b@, jb, b@.len() as int, v);
    // The run ends are a cut: the runs are equivalent to `v`, which is below what follows.
    assert forall|x: T| above(ord, a@, b@, i, j, x) && lt(ord, v, x) implies #[trigger] above(
        ord,
        a@,
        b@,
        ia,
        jb,
        x,
    ) by {
        assert forall|p: int| 0 <= p < ia implies #[trigger] lt(ord, a@[p], x) by {
            if p >= i {
                lemma_trans(ord, a@[p], v, x);
            }
        }
        assert forall|q: int| 0 <= q < jb implies #[trigger] lt(ord, b@[q], x) by {
            if q >= j {
                lemma_trans(ord, b@[q], v, x);
            }
        }
    }
    assert forall|p: int| ia <= p < a@.len() implies #[trigger] above(ord, a@, b@, ia, jb, a@[p]) by {
        assert(above(ord, a@, b@, i, j, a@[p]));
        assert(lt(ord, v, a@[p]));
    }
    assert forall|q: int| jb <= q < b@.len() implies #[trigger] above(ord, a@, b@, ia, jb, b@[q]) by {
        assert(above(ord, a@, b@, i, j, b@[q]));
        assert(lt(ord, v, b@[q]));
    }
}

/// Properties 3 and 4 from a cut, and that every row is above the cut.
proof fn lemma_sorted<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
    i: int,
    j: int,
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
        0 <= i <= a@.len(),
        0 <= j <= b@.len(),
        cut(ord, a@, b@, i, j),
    ensures
        ({
            let d = diff_from(ord, a, b, i, j);
            &&& forall|k: int| 0 <= k < d.len() ==> {
                &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
                &&& d[k].count_b == count_all(ord, b@, *d[k].row)
                &&& d[k].count_a != d[k].count_b
                &&& above(ord, a@, b@, i, j, *d[k].row)
            }
            &&& forall|k1: int, k2: int|
                0 <= k1 < k2 < d.len() ==> lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row)
            &&& forall|v: T|
                #![trigger count_all(ord, a@, v)]
                #![trigger count_all(ord, b@, v)]
                count_all(ord, a@, v) != count_all(ord, b@, v)
                && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], v))
                && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], v))
                ==> exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v)
        }),
    decreases a@.len() - i + b@.len() - j,
{
    let d = diff_from(ord, a, b, i, j);
    if i == a@.len() && j == b@.len() {
        assert forall|v: T|
            count_all(ord, a@, v) != count_all(ord, b@, v)
            && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], v))
            && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], v))
            implies exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v) by {
            lemma_count_none(ord, a@, 0, i, v);
            lemma_count_none(ord, b@, 0, j, v);
        }
        return;
    }
    lemma_step(ord, a, b, i, j);
    lemma_step_sorted(ord, a, b, i, j);
    let from_a = head_from_a(ord, a@, b@, i, j);
    let (r, ia, jb) = step(ord, a, b, i, j);
    let v = *r;
    lemma_sorted(ord, a, b, ia, jb);
    let rest = diff_from(ord, a, b, ia, jb);
    // Rows of the rest are above the runs, so above `v` and above the old cut.
    assert forall|k: int| 0 <= k < rest.len() implies lt(ord, v, *(#[trigger] rest[k]).row)
        && above(ord, a@, b@, i, j, *rest[k].row) by {
        assert(above(ord, a@, b@, ia, jb, *rest[k].row));
        if from_a {
            assert(lt(ord, a@[i], *rest[k].row));
        } else {
            assert(lt(ord, b@[j], *rest[k].row));
        }
    }
    // Completeness: a value with differing counts is `v`'s equivalent, or lies past the runs.
    assert forall|w: T|
        count_all(ord, a@, w) != count_all(ord, b@, w)
        && (forall|p: int| 0 <= p < i ==> !eqv(ord, #[trigger] a@[p], w))
        && (forall|q: int| 0 <= q < j ==> !eqv(ord, #[trigger] b@[q], w))
        implies exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, w) by {
        if eqv(ord, v, w) {
            lemma_count_congr(ord, a@, 0, a@.len() as int, v, w);
            lemma_count_congr(ord, b@, 0, b@.len() as int, v, w);
            assert(d[0].row == r);
        } else {
            assert forall|p: int| 0 <= p < ia implies !eqv(ord, #[trigger] a@[p], w) by {
                if p >= i {
                    assert(eqv(ord, a@[p], v));
                    lemma_eqv_symm(ord, a@[p], v);
                    lemma_trans(ord, v, a@[p], w);
                }
            }
            assert forall|q: int| 0 <= q < jb implies !eqv(ord, #[trigger] b@[q], w) by {
                if q >= j {
                    assert(eqv(ord, b@[q], v));
                    lemma_eqv_symm(ord, b@[q], v);
                    lemma_trans(ord, v, b@[q], w);
                }
            }
            let k = choose|k: int| 0 <= k < rest.len() && eqv(ord, *(#[trigger] rest[k]).row, w);
            if ia - i != jb - j {
                assert(d[k + 1] == rest[k]);
            } else {
                assert(d[k] == rest[k]);
            }
        }
    }
    if ia - i != jb - j {
        assert(d == seq![Delta { row: r, count_a: (ia - i) as usize, count_b: (jb - j) as usize }] + rest);
        assert forall|k: int| 0 <= k < d.len() implies {
            &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
            &&& d[k].count_b == count_all(ord, b@, *d[k].row)
            &&& d[k].count_a != d[k].count_b
            &&& above(ord, a@, b@, i, j, *d[k].row)
        } by {
            if k > 0 {
                assert(d[k] == rest[k - 1]);
            } else {
                assert(d[0].row == r);
            }
        }
        assert forall|k1: int, k2: int|
            0 <= k1 < k2 < d.len() implies lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row) by {
            assert(d[k2] == rest[k2 - 1]);
            if k1 > 0 {
                assert(d[k1] == rest[k1 - 1]);
            }
        }
    } else {
        assert(d == rest);
    }
}

pub proof fn theorem_reports_exactly_count_differences<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        ({
            let d = diff_from(ord, a, b, 0, 0);
            &&& forall|k: int| 0 <= k < d.len() ==> {
                &&& (#[trigger] d[k]).count_a == count_all(ord, a@, *d[k].row)
                &&& d[k].count_b == count_all(ord, b@, *d[k].row)
                &&& d[k].count_a != d[k].count_b
            }
            &&& forall|v: T|
                #![trigger count_all(ord, a@, v)]
                #![trigger count_all(ord, b@, v)]
                count_all(ord, a@, v) != count_all(ord, b@, v)
                ==> exists|k: int| 0 <= k < d.len() && eqv(ord, *(#[trigger] d[k]).row, v)
        }),
{
    lemma_sorted(ord, a, b, 0, 0);
}

pub proof fn theorem_no_value_twice<'a, T>(
    ord: spec_fn(T, T) -> Ordering,
    a: &'a [T],
    b: &'a [T],
)
    requires
        total_order(ord),
        sorted(ord, a@),
        sorted(ord, b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        ({
            let d = diff_from(ord, a, b, 0, 0);
            forall|k1: int, k2: int|
                0 <= k1 < k2 < d.len() ==> lt(ord, *(#[trigger] d[k1]).row, *(#[trigger] d[k2]).row)
        }),
{
    lemma_sorted(ord, a, b, 0, 0);
}
} // verus!
