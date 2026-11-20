//! A verified merge sort.

use std::cmp::Ordering;
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use crate::order::equal_means_eq;
#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, le, sorted, total_order};

verus! {

// ---------------------------------------------------------------------------
// Consequences of the order laws.

/// Any two values are ordered one way or the other, and each is at most itself.
pub proof fn lemma_le_total<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T)
    requires
        total_order(ord),
    ensures
        le(ord, x, y) || le(ord, y, x),
        le(ord, x, x),
{
    assert(ord(y, x) == Ordering::Less <==> ord(x, y) == Ordering::Greater);
    assert(ord(x, x) == Ordering::Less <==> ord(x, x) == Ordering::Greater);
}

/// `ord(x, y)` is not `Less` exactly when `y` is at most `x`.
pub proof fn lemma_not_less<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T)
    requires
        total_order(ord),
    ensures
        ord(x, y) != Ordering::Less <==> le(ord, y, x),
{
    assert(ord(x, y) == Ordering::Less <==> ord(y, x) == Ordering::Greater);
}

/// Each element is at least every later one.
pub open spec fn sorted_desc<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> le(ord, #[trigger] s[q], #[trigger] s[p])
}

/// Every element of `s` is at most every element of `t`.
pub open spec fn all_le<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>) -> bool {
    forall|p: int, q: int|
        0 <= p < s.len() && 0 <= q < t.len() ==> le(ord, #[trigger] s[p], #[trigger] t[q])
}

/// An empty sequence has the empty multiset.
pub proof fn lemma_empty_multiset<T>(s: Seq<T>)
    requires
        s.len() == 0,
    ensures
        s.to_multiset() == vstd::multiset::Multiset::<T>::empty(),
        forall|m: vstd::multiset::Multiset<T>| #[trigger] m.add(s.to_multiset()) == m,
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures, vstd::multiset::group_multiset_properties;

    assert(s.to_multiset().len() == 0);
    assert forall|m: vstd::multiset::Multiset<T>| #[trigger] m.add(s.to_multiset()) == m by {
        assert(m.add(s.to_multiset()) =~= m);
    }
}

// ---------------------------------------------------------------------------
// The code.

/// `v`'s elements sorted by the order `cmp` computes, none dropped or merged. Not `std`'s sort,
/// whose output is not specified to Verus.
pub fn sort_by<T, F: Fn(&T, &T) -> Ordering>(
    v: Vec<T>,
    cmp: &F,
    Ghost(ord): Ghost<spec_fn(T, T) -> Ordering>,
) -> (r: Vec<T>)
    requires
        total_order(ord),
        computes(*cmp, ord),
    ensures
        sorted(ord, r@),
        r@.to_multiset() == v@.to_multiset(),
    decreases v@.len(),
{
    let n = v.len();
    if n <= 1 {
        return v;
    }
    let ghost v0 = v@;
    // Move the last half of `v` to `right`, so both halves are shorter than `v`.
    let mut left = v;
    let mut right: Vec<T> = Vec::new();
    let half = n / 2;
    proof {
        lemma_empty_multiset(right@);
    }
    while right.len() < half
        invariant
            0 < half < n,
            left@.len() + right@.len() == n,
            right@.len() <= half,
            left@.to_multiset().add(right@.to_multiset()) == v0.to_multiset(),
        decreases half - right@.len(),
    {
        let ghost l0 = left@;
        let x = left.pop().unwrap();
        proof {
            broadcast use vstd::seq_lib::group_to_multiset_ensures;

            assert(l0 =~= left@.push(x));
        }
        right.push(x);
        proof {
            broadcast use vstd::seq_lib::group_to_multiset_ensures;

            assert(left@.to_multiset().add(right@.to_multiset()) =~= v0.to_multiset());
        }
    }
    let sl = sort_by(left, cmp, Ghost(ord));
    let sr = sort_by(right, cmp, Ghost(ord));
    merge(sl, sr, cmp, Ghost(ord))
}

/// Two sorted vectors merged into one.
// Verus does not support `while let`.
#[allow(clippy::manual_while_let_some)]
fn merge<T, F: Fn(&T, &T) -> Ordering>(
    a: Vec<T>,
    b: Vec<T>,
    cmp: &F,
    Ghost(ord): Ghost<spec_fn(T, T) -> Ordering>,
) -> (r: Vec<T>)
    requires
        total_order(ord),
        computes(*cmp, ord),
        sorted(ord, a@),
        sorted(ord, b@),
    ensures
        sorted(ord, r@),
        r@.to_multiset() == a@.to_multiset().add(b@.to_multiset()),
{
    let ghost total = a@.to_multiset().add(b@.to_multiset());
    let mut a = a;
    let mut b = b;
    // The greatest remaining element first, so each is taken off the end of its input.
    let mut desc: Vec<T> = Vec::new();
    proof {
        lemma_empty_multiset(desc@);
    }
    while !a.is_empty() || !b.is_empty()
        invariant
            total_order(ord),
            computes(*cmp, ord),
            sorted(ord, a@),
            sorted(ord, b@),
            sorted_desc(ord, desc@),
            all_le(ord, a@, desc@),
            all_le(ord, b@, desc@),
            a@.to_multiset().add(b@.to_multiset()).add(desc@.to_multiset()) == total,
        decreases a@.len() + b@.len(),
    {
        let mut take_a = b.is_empty();
        if !a.is_empty() && !b.is_empty() {
            take_a = !matches!((*cmp)(&a[a.len() - 1], &b[b.len() - 1]), Ordering::Less);
        }
        let ghost a0 = a@;
        let ghost b0 = b@;
        let ghost d0 = desc@;
        if take_a {
            let x = a.pop().unwrap();
            proof {
                if b0.len() > 0 {
                    assert(ord(a0[a0.len() - 1], b0[b0.len() - 1]) != Ordering::Less);
                    lemma_not_less(ord, x, b0.last());
                    assert(le(ord, b0.last(), x));
                }
                assert forall|p: int| 0 <= p < b0.len() implies le(ord, #[trigger] b0[p], x) by {
                    if p < b0.len() - 1 {
                        assert(le(ord, b0[p], b0.last()));
                    } else {
                        lemma_le_total(ord, x, x);
                    }
                }
                assert forall|p: int| 0 <= p < a@.len() implies le(ord, #[trigger] a@[p], x) by {
                    assert(a@[p] == a0[p]);
                    assert(le(ord, a0[p], a0[a0.len() - 1]));
                }
                assert(a0 =~= a@.push(x));
            }
            desc.push(x);
            proof {
                broadcast use vstd::seq_lib::group_to_multiset_ensures;

                assert(desc@ =~= d0.push(x));
                assert(sorted_desc(ord, desc@)) by {
                    assert forall|p: int, q: int| 0 <= p < q < desc@.len() implies le(
                        ord,
                        #[trigger] desc@[q],
                        #[trigger] desc@[p],
                    ) by {
                        if q == desc@.len() - 1 {
                            assert(le(ord, a0[a0.len() - 1], d0[p]));
                        } else {
                            assert(le(ord, d0[q], d0[p]));
                        }
                    }
                }
                assert(all_le(ord, a@, desc@)) by {
                    assert forall|p: int, q: int|
                        0 <= p < a@.len() && 0 <= q < desc@.len() implies le(
                        ord,
                        #[trigger] a@[p],
                        #[trigger] desc@[q],
                    ) by {
                        assert(a@[p] == a0[p]);
                        if q < d0.len() {
                            assert(le(ord, a0[p], d0[q]));
                        } else {
                            assert(le(ord, a@[p], x));
                        }
                    }
                }
                assert(all_le(ord, b@, desc@)) by {
                    assert forall|p: int, q: int|
                        0 <= p < b@.len() && 0 <= q < desc@.len() implies le(
                        ord,
                        #[trigger] b@[p],
                        #[trigger] desc@[q],
                    ) by {
                        if q < d0.len() {
                            assert(le(ord, b0[p], d0[q]));
                        } else {
                            assert(le(ord, b0[p], x));
                        }
                    }
                }
                assert(a@.to_multiset().add(b@.to_multiset()).add(desc@.to_multiset()) =~= total);
            }
        } else {
            let x = b.pop().unwrap();
            proof {
                if a0.len() > 0 {
                    assert(ord(a0[a0.len() - 1], b0[b0.len() - 1]) == Ordering::Less);
                    lemma_not_less(ord, a0.last(), x);
                    lemma_le_total(ord, a0.last(), x);
                }
                assert forall|p: int| 0 <= p < a0.len() implies le(ord, #[trigger] a0[p], x) by {
                    if p < a0.len() - 1 {
                        assert(le(ord, a0[p], a0.last()));
                    }
                }
                assert forall|p: int| 0 <= p < b@.len() implies le(ord, #[trigger] b@[p], x) by {
                    assert(b@[p] == b0[p]);
                    assert(le(ord, b0[p], b0[b0.len() - 1]));
                }
                assert(b0 =~= b@.push(x));
            }
            desc.push(x);
            proof {
                broadcast use vstd::seq_lib::group_to_multiset_ensures;

                assert(desc@ =~= d0.push(x));
                assert(sorted_desc(ord, desc@)) by {
                    assert forall|p: int, q: int| 0 <= p < q < desc@.len() implies le(
                        ord,
                        #[trigger] desc@[q],
                        #[trigger] desc@[p],
                    ) by {
                        if q == desc@.len() - 1 {
                            assert(le(ord, b0[b0.len() - 1], d0[p]));
                        } else {
                            assert(le(ord, d0[q], d0[p]));
                        }
                    }
                }
                assert(all_le(ord, a@, desc@)) by {
                    assert forall|p: int, q: int|
                        0 <= p < a@.len() && 0 <= q < desc@.len() implies le(
                        ord,
                        #[trigger] a@[p],
                        #[trigger] desc@[q],
                    ) by {
                        if q < d0.len() {
                            assert(le(ord, a0[p], d0[q]));
                        } else {
                            assert(le(ord, a0[p], x));
                        }
                    }
                }
                assert(all_le(ord, b@, desc@)) by {
                    assert forall|p: int, q: int|
                        0 <= p < b@.len() && 0 <= q < desc@.len() implies le(
                        ord,
                        #[trigger] b@[p],
                        #[trigger] desc@[q],
                    ) by {
                        assert(b@[p] == b0[p]);
                        if q < d0.len() {
                            assert(le(ord, b0[p], d0[q]));
                        } else {
                            assert(le(ord, b@[p], x));
                        }
                    }
                }
                assert(a@.to_multiset().add(b@.to_multiset()).add(desc@.to_multiset()) =~= total);
            }
        }
    }
    // Reverse `desc`, smallest first.
    let mut r: Vec<T> = Vec::new();
    proof {
        lemma_empty_multiset(a@);
        lemma_empty_multiset(b@);
        lemma_empty_multiset(r@);
        assert(a@.to_multiset().add(b@.to_multiset()) =~= b@.to_multiset());
        assert(r@.to_multiset().add(desc@.to_multiset()) =~= desc@.to_multiset());
    }
    while !desc.is_empty()
        invariant
            total_order(ord),
            sorted(ord, r@),
            sorted_desc(ord, desc@),
            all_le(ord, r@, desc@),
            r@.to_multiset().add(desc@.to_multiset()) == total,
        decreases desc@.len(),
    {
        let ghost d0 = desc@;
        let ghost r0 = r@;
        let x = desc.pop().unwrap();
        r.push(x);
        proof {
            broadcast use vstd::seq_lib::group_to_multiset_ensures;

            assert(d0 =~= desc@.push(x));
            assert(r@ =~= r0.push(x));
            assert(sorted(ord, r@)) by {
                assert forall|p: int, q: int| 0 <= p < q < r@.len() implies le(
                    ord,
                    #[trigger] r@[p],
                    #[trigger] r@[q],
                ) by {
                    if q == r@.len() - 1 {
                        assert(le(ord, r0[p], d0[d0.len() - 1]));
                    } else {
                        assert(le(ord, r0[p], r0[q]));
                    }
                }
            }
            assert(all_le(ord, r@, desc@)) by {
                assert forall|p: int, q: int|
                    0 <= p < r@.len() && 0 <= q < desc@.len() implies le(
                    ord,
                    #[trigger] r@[p],
                    #[trigger] desc@[q],
                ) by {
                    assert(desc@[q] == d0[q]);
                    if p < r0.len() {
                        assert(le(ord, r0[p], d0[q]));
                    } else {
                        assert(le(ord, d0[d0.len() - 1], d0[q]));
                    }
                }
            }
            assert(r@.to_multiset().add(desc@.to_multiset()) =~= total);
        }
    }
    proof {
        lemma_empty_multiset(desc@);
    }
    r
}

// ---------------------------------------------------------------------------
// Uniqueness.

/// Under a total order whose `Equal` is equality, a multiset has one sorted sequence.
pub proof fn lemma_sorted_unique<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
        equal_means_eq(ord),
        sorted(ord, s),
        sorted(ord, t),
        s.to_multiset() == t.to_multiset(),
    ensures
        s == t,
    decreases s.len(),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;

    assert(s.len() == s.to_multiset().len());
    assert(t.len() == t.to_multiset().len());
    if s.len() == 0 {
        assert(s =~= t);
        return;
    }
    let x = s[0];
    let y = t[0];
    // Each head is in the other sequence, so it is at least the other's head.
    assert(s.contains(x));
    assert(t.to_multiset().count(x) > 0);
    assert(t.contains(x));
    let k = choose|k: int| 0 <= k < t.len() && t[k] == x;
    assert(t.contains(y));
    assert(s.to_multiset().count(y) > 0);
    assert(s.contains(y));
    let j = choose|j: int| 0 <= j < s.len() && s[j] == y;
    lemma_le_total(ord, x, x);
    lemma_le_total(ord, y, y);
    if k > 0 {
        assert(le(ord, t[0], t[k]));
    }
    if j > 0 {
        assert(le(ord, s[0], s[j]));
    }
    assert(le(ord, y, x));
    assert(le(ord, x, y));
    assert(ord(x, y) == Ordering::Less <==> ord(y, x) == Ordering::Greater);
    assert(ord(x, y) == Ordering::Equal);
    assert(x == y);
    // The tails have equal multisets and are sorted.
    assert(s.remove(0) =~= s.drop_first());
    assert(t.remove(0) =~= t.drop_first());
    assert(s.drop_first().to_multiset() == s.to_multiset().remove(x));
    assert(t.drop_first().to_multiset() == t.to_multiset().remove(y));
    assert(sorted(ord, s.drop_first())) by {
        assert forall|p: int, q: int| 0 <= p < q < s.drop_first().len() implies le(
            ord,
            #[trigger] s.drop_first()[p],
            #[trigger] s.drop_first()[q],
        ) by {
            assert(le(ord, s[p + 1], s[q + 1]));
        }
    }
    assert(sorted(ord, t.drop_first())) by {
        assert forall|p: int, q: int| 0 <= p < q < t.drop_first().len() implies le(
            ord,
            #[trigger] t.drop_first()[p],
            #[trigger] t.drop_first()[q],
        ) by {
            assert(le(ord, t[p + 1], t[q + 1]));
        }
    }
    lemma_sorted_unique(ord, s.drop_first(), t.drop_first());
    assert(s =~= t) by {
        assert forall|i: int| 0 <= i < s.len() implies s[i] == t[i] by {
            if i > 0 {
                assert(s[i] == s.drop_first()[i - 1]);
                assert(t[i] == t.drop_first()[i - 1]);
            }
        }
    }
}

} // verus!
