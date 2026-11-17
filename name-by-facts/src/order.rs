//! Orders, the lexicographic order they induce on sequences, and sets of values kept as
//! strictly sorted sequences.

use std::cmp::Ordering;
use vstd::prelude::*;

verus! {

pub open spec fn eqv<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) == Ordering::Equal
}

pub open spec fn lt<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) == Ordering::Less
}

pub open spec fn le<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T) -> bool {
    ord(x, y) != Ordering::Greater
}

/// `ord` is a total order: antisymmetric and transitive (reflexivity follows). Values it
/// calls `Equal` need not be identical.
pub open spec fn total_order<T>(ord: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|x: T, y: T|
        (#[trigger] ord(x, y) == Ordering::Less) <==> ord(y, x) == Ordering::Greater
    &&& forall|x: T, y: T| (#[trigger] ord(x, y) == Ordering::Equal) ==> ord(y, x) == Ordering::Equal
    &&& forall|x: T, y: T, z: T|
        #[trigger] le(ord, x, y) && #[trigger] le(ord, y, z) ==> le(ord, x, z)
}

/// `cmp` can be called on any two values, and what it returns is `ord` of them.
pub open spec fn computes<T, F: Fn(&T, &T) -> Ordering>(cmp: F, ord: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|x: &T, y: &T| #[trigger] cmp.requires((x, y))
    &&& forall|x: &T, y: &T, o: Ordering| #[trigger] cmp.ensures((x, y), o) ==> o == ord(*x, *y)
}

/// `s` is strictly increasing under `ord`: it holds no two equivalent values.
pub open spec fn strictly_sorted<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>) -> bool {
    forall|p: int, q: int| 0 <= p < q < s.len() ==> lt(ord, #[trigger] s[p], #[trigger] s[q])
}

/// `s` holds a value equivalent to `v`.
pub open spec fn mem<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, v: T) -> bool {
    exists|m: int| 0 <= m < s.len() && eqv(ord, #[trigger] s[m], v)
}

/// The lexicographic order `ord` induces: element by element, a proper prefix first.
pub open spec fn lex<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>) -> Ordering
    decreases s.len(),
{
    if s.len() == 0 {
        if t.len() == 0 {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else if t.len() == 0 {
        Ordering::Greater
    } else if ord(s[0], t[0]) == Ordering::Equal {
        lex(ord, s.drop_first(), t.drop_first())
    } else {
        ord(s[0], t[0])
    }
}

pub open spec fn lex_ord<T>(ord: spec_fn(T, T) -> Ordering) -> spec_fn(Seq<T>, Seq<T>) -> Ordering {
    |s: Seq<T>, t: Seq<T>| lex(ord, s, t)
}

// ---------------------------------------------------------------------------
// Consequences of the order laws.

pub proof fn lemma_refl<T>(ord: spec_fn(T, T) -> Ordering, x: T)
    requires
        total_order(ord),
    ensures
        eqv(ord, x, x),
{
    assert(ord(x, x) == Ordering::Less <==> ord(x, x) == Ordering::Greater);
}

pub proof fn lemma_eqv_symm<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T)
    requires
        total_order(ord),
        eqv(ord, x, y),
    ensures
        eqv(ord, y, x),
{
    assert(ord(x, y) == Ordering::Equal);
}

/// `le` and `lt` chain, and `eqv` is transitive.
pub proof fn lemma_trans<T>(ord: spec_fn(T, T) -> Ordering, x: T, y: T, z: T)
    requires
        total_order(ord),
    ensures
        le(ord, x, y) && le(ord, y, z) ==> le(ord, x, z),
        lt(ord, x, y) && le(ord, y, z) ==> lt(ord, x, z),
        le(ord, x, y) && lt(ord, y, z) ==> lt(ord, x, z),
        eqv(ord, x, y) && eqv(ord, y, z) ==> eqv(ord, x, z),
{
    assert(le(ord, x, y) && le(ord, y, z) ==> le(ord, x, z));
    assert(le(ord, z, x) && le(ord, x, y) ==> le(ord, z, y));
    assert(le(ord, y, z) && le(ord, z, x) ==> le(ord, y, x));
    assert(ord(x, y) == Ordering::Less <==> ord(y, x) == Ordering::Greater);
    assert(ord(y, z) == Ordering::Less <==> ord(z, y) == Ordering::Greater);
    assert(ord(x, z) == Ordering::Less <==> ord(z, x) == Ordering::Greater);
    assert(ord(z, x) == Ordering::Less <==> ord(x, z) == Ordering::Greater);
    assert(ord(y, x) == Ordering::Less <==> ord(x, y) == Ordering::Greater);
    assert(ord(z, y) == Ordering::Less <==> ord(y, z) == Ordering::Greater);
    if eqv(ord, x, y) && eqv(ord, y, z) {
        lemma_eqv_symm(ord, x, y);
        lemma_eqv_symm(ord, y, z);
        assert(le(ord, x, y) && le(ord, y, z));
        assert(le(ord, x, z));
        assert(le(ord, z, y) && le(ord, y, x));
        assert(le(ord, z, x));
        assert(ord(x, z) != Ordering::Less);
    }
}

// ---------------------------------------------------------------------------
// The lexicographic order is a total order.

/// `lex` is `Equal` exactly when the sequences have one length and are equivalent element by
/// element.
pub proof fn lemma_lex_equal<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    ensures
        lex(ord, s, t) == Ordering::Equal <==> (s.len() == t.len() && forall|k: int|
            0 <= k < s.len() ==> eqv(ord, #[trigger] s[k], t[k])),
    decreases s.len(),
{
    if s.len() > 0 && t.len() > 0 {
        lemma_lex_equal(ord, s.drop_first(), t.drop_first());
        if s.len() == t.len() && forall|k: int| 0 <= k < s.len() ==> eqv(ord, #[trigger] s[k], t[k]) {
            assert(eqv(ord, s[0], t[0]));
            assert forall|k: int| 0 <= k < s.drop_first().len() implies eqv(
                ord,
                #[trigger] s.drop_first()[k],
                t.drop_first()[k],
            ) by {
                assert(eqv(ord, s[k + 1], t[k + 1]));
            }
        }
        if lex(ord, s, t) == Ordering::Equal {
            assert forall|k: int| 0 <= k < s.len() implies eqv(ord, #[trigger] s[k], t[k]) by {
                if k > 0 {
                    assert(eqv(ord, s.drop_first()[k - 1], t.drop_first()[k - 1]));
                }
            }
        }
    }
}

proof fn lemma_lex_antisym<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>)
    requires
        total_order(ord),
    ensures
        lex(ord, s, t) == Ordering::Less <==> lex(ord, t, s) == Ordering::Greater,
        lex(ord, s, t) == Ordering::Equal ==> lex(ord, t, s) == Ordering::Equal,
    decreases s.len(),
{
    if s.len() > 0 && t.len() > 0 {
        assert(ord(s[0], t[0]) == Ordering::Less <==> ord(t[0], s[0]) == Ordering::Greater);
        assert(ord(t[0], s[0]) == Ordering::Less <==> ord(s[0], t[0]) == Ordering::Greater);
        if ord(s[0], t[0]) == Ordering::Equal {
            lemma_eqv_symm(ord, s[0], t[0]);
        }
        if ord(t[0], s[0]) == Ordering::Equal {
            lemma_eqv_symm(ord, t[0], s[0]);
        }
        lemma_lex_antisym(ord, s.drop_first(), t.drop_first());
    }
}

proof fn lemma_lex_le_trans<T>(ord: spec_fn(T, T) -> Ordering, s: Seq<T>, t: Seq<T>, u: Seq<T>)
    requires
        total_order(ord),
        lex(ord, s, t) != Ordering::Greater,
        lex(ord, t, u) != Ordering::Greater,
    ensures
        lex(ord, s, u) != Ordering::Greater,
    decreases s.len(),
{
    if s.len() > 0 && t.len() > 0 && u.len() > 0 {
        let (a, b, c) = (s[0], t[0], u[0]);
        lemma_trans(ord, a, b, c);
        assert(le(ord, a, b));
        assert(le(ord, b, c));
        if eqv(ord, a, c) {
            // `a <= b <= c` and `a` equivalent to `c` pin `b` between them.
            lemma_eqv_symm(ord, a, c);
            lemma_trans(ord, c, a, b);
            lemma_trans(ord, b, c, a);
            assert(ord(a, b) == Ordering::Less <==> ord(b, a) == Ordering::Greater);
            assert(ord(b, c) == Ordering::Less <==> ord(c, b) == Ordering::Greater);
            assert(eqv(ord, a, b));
            assert(eqv(ord, b, c));
            lemma_lex_le_trans(ord, s.drop_first(), t.drop_first(), u.drop_first());
        }
    }
}

pub proof fn lemma_lex_total<T>(ord: spec_fn(T, T) -> Ordering)
    requires
        total_order(ord),
    ensures
        total_order(lex_ord(ord)),
{
    let l = lex_ord(ord);
    assert forall|s: Seq<T>, t: Seq<T>|
        (#[trigger] l(s, t) == Ordering::Less) <==> l(t, s) == Ordering::Greater by {
        lemma_lex_antisym(ord, s, t);
    }
    assert forall|s: Seq<T>, t: Seq<T>|
        (#[trigger] l(s, t) == Ordering::Equal) implies l(t, s) == Ordering::Equal by {
        lemma_lex_antisym(ord, s, t);
    }
    assert forall|s: Seq<T>, t: Seq<T>, u: Seq<T>|
        #[trigger] le(l, s, t) && #[trigger] le(l, t, u) implies le(l, s, u) by {
        lemma_lex_le_trans(ord, s, t, u);
    }
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
