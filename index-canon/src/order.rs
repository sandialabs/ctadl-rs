//! Sets of values kept as strictly sorted sequences.

use order_laws::*;
use std::cmp::Ordering;
use vstd::prelude::*;

verus! {

/// `s` is strictly increasing under `ord`: it holds no two equivalent values.
pub open spec fn strictly_sorted<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> lt(ord, #[trigger] s[p], #[trigger] s[q])
}

/// `s` holds a value equivalent to `v`.
pub open spec fn mem<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, v: T) -> bool {
    exists|m: int| 0 <= m < s.len() && eqv(ord, #[trigger] s[m], v)
}

// ---------------------------------------------------------------------------
// A strictly sorted sequence represents the set of its elements.

/// Two strictly sorted sequences hold equivalent elements exactly when they are `lex`-equal.
pub proof fn lemma_set_eq_lex<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
        strictly_sorted(ord, s),
        strictly_sorted(ord, t),
    ensures
        (forall|v: T| mem(ord, s, v) <==> mem(ord, t, v)) <==> lex(ord, s, t) == Ordering::Equal,
    decreases s.len() + t.len(),
{
    lemma_lex_equal(ord, s, t);
    if lex(ord, s, t) == Ordering::Equal {
        assert forall|v: T| mem(ord, s, v) <==> mem(ord, t, v) by {
            if mem(ord, s, v) {
                let m = choose|m: int| 0 <= m < s.len() && eqv(ord, #[trigger] s[m], v);
                lemma_eqv_symm(ord, s[m], t[m]);
                lemma_trans(ord, t[m], s[m], v);
            }
            if mem(ord, t, v) {
                let m = choose|m: int| 0 <= m < t.len() && eqv(ord, #[trigger] t[m], v);
                lemma_trans(ord, s[m], t[m], v);
            }
        }
    }
    if forall|v: T| mem(ord, s, v) <==> mem(ord, t, v) {
        if s.len() == 0 && t.len() > 0 {
            lemma_refl(ord, t[0]);
            assert(mem(ord, t, t[0]));
        } else if t.len() == 0 && s.len() > 0 {
            lemma_refl(ord, s[0]);
            assert(mem(ord, s, s[0]));
        } else if s.len() > 0 && t.len() > 0 {
            let (a, b) = (s[0], t[0]);
            // `a` is in `t`, so `b <= a`; `b` is in `s`, so `a <= b`.
            lemma_refl(ord, a);
            lemma_refl(ord, b);
            assert(mem(ord, s, a));
            assert(mem(ord, t, b));
            let m = choose|m: int| 0 <= m < t.len() && eqv(ord, #[trigger] t[m], a);
            let n = choose|n: int| 0 <= n < s.len() && eqv(ord, #[trigger] s[n], b);
            if m > 0 {
                assert(lt(ord, t[0], t[m]));
            }
            if n > 0 {
                assert(lt(ord, s[0], s[n]));
            }
            lemma_trans(ord, b, t[m], a);
            lemma_trans(ord, a, s[n], b);
            lemma_eqv_symm(ord, t[m], a);
            lemma_eqv_symm(ord, s[n], b);
            assert(le(ord, b, a));
            assert(le(ord, a, b));
            assert(ord(a, b) == Ordering::Less <==> ord(b, a) == Ordering::Greater);
            assert(eqv(ord, a, b));
            let (s1, t1) = (s.drop_first(), t.drop_first());
            assert(strictly_sorted(ord, s1)) by {
                assert forall|p: int, q: int| 0 <= p < q < s1.len() implies lt(
                    ord,
                    #[trigger] s1[p],
                    #[trigger] s1[q],
                ) by {
                    assert(lt(ord, s[p + 1], s[q + 1]));
                }
            }
            assert(strictly_sorted(ord, t1)) by {
                assert forall|p: int, q: int| 0 <= p < q < t1.len() implies lt(
                    ord,
                    #[trigger] t1[p],
                    #[trigger] t1[q],
                ) by {
                    assert(lt(ord, t[p + 1], t[q + 1]));
                }
            }
            // The tails hold the same values: a value in one tail is past its head, so it is
            // not equivalent to the other's head, and so is in the other's tail.
            assert forall|v: T| mem(ord, s1, v) <==> mem(ord, t1, v) by {
                if mem(ord, s1, v) {
                    let k = choose|k: int| 0 <= k < s1.len() && eqv(ord, #[trigger] s1[k], v);
                    assert(eqv(ord, s[k + 1], v));
                    assert(mem(ord, s, v));
                    let j = choose|j: int| 0 <= j < t.len() && eqv(ord, #[trigger] t[j], v);
                    if j == 0 {
                        // `s[k + 1]` would be equivalent to `b`, so to `a`, though it is past it.
                        lemma_eqv_symm(ord, t[0], v);
                        lemma_trans(ord, s[k + 1], v, b);
                        lemma_eqv_symm(ord, a, b);
                        lemma_trans(ord, s[k + 1], b, a);
                        assert(lt(ord, s[0], s[k + 1]));
                        lemma_eqv_symm(ord, s[k + 1], a);
                        assert(false);
                    }
                    assert(eqv(ord, t1[j - 1], v));
                }
                if mem(ord, t1, v) {
                    let k = choose|k: int| 0 <= k < t1.len() && eqv(ord, #[trigger] t1[k], v);
                    assert(eqv(ord, t[k + 1], v));
                    assert(mem(ord, t, v));
                    let j = choose|j: int| 0 <= j < s.len() && eqv(ord, #[trigger] s[j], v);
                    if j == 0 {
                        lemma_eqv_symm(ord, s[0], v);
                        lemma_trans(ord, t[k + 1], v, a);
                        lemma_trans(ord, t[k + 1], a, b);
                        assert(lt(ord, t[0], t[k + 1]));
                        lemma_eqv_symm(ord, t[k + 1], b);
                        assert(false);
                    }
                    assert(eqv(ord, s1[j - 1], v));
                }
            }
            lemma_set_eq_lex(ord, s1, t1);
            assert(lex(ord, s, t) == lex(ord, s1, t1));
        }
    }
}

} // verus!
