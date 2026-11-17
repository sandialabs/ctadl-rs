//! A client: `u8` under its usual order, with `cmp` a closure that calls `Ord::cmp`. Only
//! Verus compiles it.

use super::*;
use crate::*;

verus! {


pub open spec fn u8_ord(x: u8, y: u8) -> Ordering {
    if x < y {
        Ordering::Less
    } else if x > y {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// The number of elements of `s` equal to `v`, counted without the merge.
pub open spec fn count_u8(s: Seq<u8>, v: u8) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        count_u8(s.drop_last(), v) + if s.last() == v {
            1nat
        } else {
            0nat
        }
    }
}

pub open spec fn sorted_u8(s: Seq<u8>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> s[p] <= s[q]
}

proof fn lemma_u8_count(s: Seq<u8>, v: u8)
    ensures
        count_all(u8_spec_ord(), s, v) == count_u8(s, v),
    decreases s.len(),
{
    let ord = u8_spec_ord();
    if s.len() > 0 {
        let t = s.drop_last();
        lemma_u8_count(t, v);
        // `count` over `s[0..n-1]` is `count` over `t`.
        lemma_count_prefix(ord, s, t, v, t.len() as int);
    }
}

/// `count` over a prefix only looks at that prefix.
proof fn lemma_count_prefix<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>, v: T, hi: int)
    requires
        0 <= hi <= t.len(),
        t.len() <= s.len(),
        forall|k: int| 0 <= k < t.len() ==> s[k] == t[k],
    ensures
        count(ord, s, 0, hi, v) == count(ord, t, 0, hi, v),
    decreases hi,
{
    if hi > 0 {
        lemma_count_prefix(ord, s, t, v, hi - 1);
    }
}

pub open spec fn u8_spec_ord() -> spec_fn(u8, u8) -> Ordering {
    |x: u8, y: u8| u8_ord(x, y)
}

/// The four properties of `out`, the deltas of `a` and `b`.
pub open spec fn client_properties(a: Seq<u8>, b: Seq<u8>, out: Seq<(u8, usize, usize)>) -> bool {
    // reports_exactly_count_differences
    &&& forall|k: int|
        0 <= k < out.len() ==> {
            &&& (#[trigger] out[k]).1 == count_u8(a, out[k].0)
            &&& out[k].2 == count_u8(b, out[k].0)
            &&& out[k].1 != out[k].2
        }
    &&& forall|v: u8|
        count_u8(a, v) != count_u8(b, v) ==> exists|k: int|
            0 <= k < out.len() && (#[trigger] out[k]).0 == v
    // no_value_twice
    &&& forall|k1: int, k2: int|
        0 <= k1 < k2 < out.len() ==> (#[trigger] out[k1]).0 < (#[trigger] out[k2]).0
    // self_diff_is_empty
    &&& a == b ==> out.len() == 0
}

proof fn lemma_client_properties<'a>(a: &'a [u8], b: &'a [u8])
    requires
        sorted_u8(a@),
        sorted_u8(b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
    ensures
        client_properties(a@, b@, deltas_u8(diff_from(u8_spec_ord(), a, b, 0, 0))),
{
    let ord = u8_spec_ord();
    let all = diff_from(ord, a, b, 0, 0);
    let out = deltas_u8(all);
    assert(total_order(ord));
    assert(sorted(ord, a@));
    assert(sorted(ord, b@));
    theorem_reports_exactly_count_differences(ord, a, b);
    theorem_no_value_twice(ord, a, b);
    assert forall|v: u8| #[trigger] count_all(ord, a@, v) == count_u8(a@, v) by {
        lemma_u8_count(a@, v);
    }
    assert forall|v: u8| #[trigger] count_all(ord, b@, v) == count_u8(b@, v) by {
        lemma_u8_count(b@, v);
    }
    assert forall|k: int| 0 <= k < out.len() implies {
        &&& (#[trigger] out[k]).1 == count_u8(a@, out[k].0)
        &&& out[k].2 == count_u8(b@, out[k].0)
        &&& out[k].1 != out[k].2
    } by {
        assert(out[k].0 == *all[k].row);
        assert(count_all(ord, a@, *all[k].row) == count_u8(a@, *all[k].row));
        assert(count_all(ord, b@, *all[k].row) == count_u8(b@, *all[k].row));
    }
    // Diffing a sequence with itself: every delta's counts would be equal.
    if a@ == b@ && out.len() > 0 {
        assert(out[0].1 != out[0].2);
    }
    assert forall|v: u8| count_u8(a@, v) != count_u8(b@, v) implies exists|k: int|
        0 <= k < out.len() && (#[trigger] out[k]).0 == v by {
        assert(count_all(ord, a@, v) == count_u8(a@, v));
        assert(count_all(ord, b@, v) == count_u8(b@, v));
        let k = choose|k: int| 0 <= k < all.len() && eqv(ord, *(#[trigger] all[k]).row, v);
        assert(out[k].0 == *all[k].row);
    }
    assert forall|k1: int, k2: int| 0 <= k1 < k2 < out.len() implies (#[trigger] out[k1]).0 < (
    #[trigger] out[k2]).0 by {
        assert(out[k1].0 == *all[k1].row);
        assert(out[k2].0 == *all[k2].row);
        assert(lt(ord, *all[k1].row, *all[k2].row));
    }
}

/// Every delta the core yields for `a` and `b`, in order, as `(value, count_a, count_b)`.
fn diff(a: &[u8], b: &[u8]) -> (out: Vec<(u8, usize, usize)>)
    requires
        sorted_u8(a@),
        sorted_u8(b@),
    ensures
        out@ == deltas_u8(diff_from(u8_spec_ord(), a, b, 0, 0)),
        client_properties(a@, b@, out@),
{
    let ghost ord = u8_spec_ord();
    let cmp = |x: &u8, y: &u8| -> (o: Ordering)
        ensures
            o == u8_ord(*x, *y),
        { x.cmp(y) };
    assert(total_order(ord));
    assert(computes(cmp, ord));
    let _ = (a.len(), b.len());
    let ghost all = diff_from(ord, a, b, 0, 0);
    let mut it = multiset_diff_by(a, b, cmp, Ghost(ord));
    let mut out: Vec<(u8, usize, usize)> = Vec::new();
    loop
        invariant
            it.obeys_prophetic_iter_laws(),
            it.decrease() is Some,
            out.len() <= all.len(),
            it.remaining() == all.skip(out.len() as int),
            out@ == deltas_u8(all.take(out.len() as int)),
        ensures
            out@ == deltas_u8(all),
        decreases it.decrease()->0,
    {
        match it.next() {
            Some(d) => {
                proof {
                    assert(all.skip(out.len() as int).drop_first() =~= all.skip(out.len() + 1));
                    assert(deltas_u8(all.take(out.len() + 1)) =~= deltas_u8(
                        all.take(out.len() as int),
                    ).push((*d.row, d.count_a, d.count_b)));
                }
                out.push((*d.row, d.count_a, d.count_b));
            },
            None => {
                assert(all.skip(out.len() as int).len() == 0);
                assert(out.len() == all.len());
                assert(all.take(out.len() as int) =~= all);
                break ;
            },
        }
    }
    proof {
        lemma_client_properties(a, b);
    }
    out
}

/// What the core yields, as `diff` records it.
pub open spec fn deltas_u8<'a>(d: Seq<Delta<'a, u8>>) -> Seq<(u8, usize, usize)> {
    d.map_values(|e: Delta<'a, u8>| (*e.row, e.count_a, e.count_b))
}

/// Diffing `(b, a)` yields the same values, in the same order, as diffing `(a, b)`, each with
/// its two counts exchanged.
fn swap_swaps_sides(a: &[u8], b: &[u8])
    requires
        sorted_u8(a@),
        sorted_u8(b@),
{
    let ab = diff(a, b);
    let ba = diff(b, a);
    proof {
        let ord = u8_spec_ord();
        assert(total_order(ord));
        theorem_swap_swaps_sides(ord, a, b);
        let d = diff_from(ord, a, b, 0, 0);
        let e = diff_from(ord, b, a, 0, 0);
        assert forall|k: int| 0 <= k < d.len() implies *(#[trigger] d[k]).row == *e[k].row by {
            assert(eqv(ord, *d[k].row, *e[k].row));
        }
    }
    assert(ab.len() == ba.len());
    assert(forall|k: int|
        0 <= k < ab.len() ==> {
            &&& (#[trigger] ab[k]).0 == ba[k].0
            &&& ab[k].1 == ba[k].2
            &&& ab[k].2 == ba[k].1
        });
}

} // verus!
