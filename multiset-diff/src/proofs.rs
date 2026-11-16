//! Kani proofs of `multiset_diff`'s properties.
//!
//! Each harness diffs inputs of type `u8` under `Ord::cmp`, a total order. An input is a
//! `kani::any` array of `N` = 6 elements cut to a `kani::any` length of at most `N` and assumed
//! sorted, so every pair of sorted sequences of at most 6 bytes is covered, the empty one
//! included. The properties are proved up to that bound only.
//!
//! Every harness has `#[kani::unwind(13)]`, that is `2 * N + 1`. Two inputs hold at most
//! `2 * N` distinct values, so the core yields at most `2 * N` deltas, and a loop over them
//! enters its head `2 * N + 1` times. That bounds the other loops too: the core's merge pass
//! takes at most `2 * N` steps, a run of equal values has at most `N` elements, and the
//! harnesses' own loops go over one input (`N`) or the deltas (`2 * N`). An implementation
//! that loops longer fails the unwinding assertion rather than passing unchecked.
//!
//! Until the core exists its body is `todo!()`, so each harness is marked
//! `#[kani::should_panic]`: the panic is the expected failure, and the check passes. That shows
//! the harnesses compile and reach the core, not that the properties hold.

use crate::multiset_diff;

/// The length bound on each input.
const N: usize = 6;

/// A sorted sequence of at most `N` bytes, any one of them.
struct Input {
    buf: [u8; N],
    len: usize,
}

impl Input {
    fn any_sorted() -> Self {
        let input = Input {
            buf: kani::any(),
            len: kani::any(),
        };
        kani::assume(input.len <= N);
        kani::assume(input.get().windows(2).all(|w| w[0] <= w[1]));
        input
    }

    fn get(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// What `multiset_diff` yields, in order, as `(value, count_a, count_b)`.
struct Deltas {
    buf: [(u8, usize, usize); 2 * N],
    len: usize,
}

impl Deltas {
    fn get(&self) -> &[(u8, usize, usize)] {
        &self.buf[..self.len]
    }
}

/// Runs the core on `a` and `b` under `Ord::cmp` and records everything it yields.
fn diff(a: &[u8], b: &[u8]) -> Deltas {
    let mut out = Deltas {
        buf: [(0, 0, 0); 2 * N],
        len: 0,
    };
    for d in multiset_diff(a, b, Ord::cmp) {
        assert!(
            out.len < 2 * N,
            "yielded more deltas than the inputs have distinct values"
        );
        out.buf[out.len] = (*d.row, d.count_a, d.count_b);
        out.len += 1;
    }
    out
}

/// The number of elements of `s` equal to `v`, counted without the core.
fn count(s: &[u8], v: u8) -> usize {
    s.iter().filter(|&&x| x == v).count()
}

/// Diffing a sequence with itself yields nothing.
#[kani::proof]
#[kani::unwind(13)]
// Expected failure: the core is `todo!()` until it is written.
#[kani::should_panic]
fn self_diff_is_empty() {
    let a = Input::any_sorted();
    assert_eq!(diff(a.get(), a.get()).len, 0);
}

/// Diffing `(b, a)` yields the same values, in the same order, as diffing `(a, b)`, each with
/// its two counts exchanged.
#[kani::proof]
#[kani::unwind(13)]
// Expected failure: the core is `todo!()` until it is written.
#[kani::should_panic]
fn swap_swaps_sides() {
    let a = Input::any_sorted();
    let b = Input::any_sorted();
    let ab = diff(a.get(), b.get());
    let ba = diff(b.get(), a.get());
    assert_eq!(ab.len, ba.len);
    for (&(v, count_a, count_b), &(w, swapped_a, swapped_b)) in ab.get().iter().zip(ba.get()) {
        assert_eq!(v, w);
        assert_eq!(count_a, swapped_b);
        assert_eq!(count_b, swapped_a);
    }
}

/// A value is yielded exactly when its counts in `a` and `b` differ, and with those counts.
///
/// Both directions: every yielded value has the counts it reports, and they differ; and any
/// value at all (symbolic, so every byte) whose counts differ is among those yielded.
#[kani::proof]
#[kani::unwind(13)]
// Expected failure: the core is `todo!()` until it is written.
#[kani::should_panic]
fn reports_exactly_count_differences() {
    let a = Input::any_sorted();
    let b = Input::any_sorted();
    let deltas = diff(a.get(), b.get());
    for &(v, count_a, count_b) in deltas.get() {
        assert_eq!(count_a, count(a.get(), v));
        assert_eq!(count_b, count(b.get(), v));
        assert_ne!(count_a, count_b);
    }
    let v: u8 = kani::any();
    if count(a.get(), v) != count(b.get(), v) {
        assert!(deltas.get().iter().any(|&(w, _, _)| w == v));
    }
}

/// The yielded values are strictly increasing under `cmp`: in order, and none twice.
#[kani::proof]
#[kani::unwind(13)]
// Expected failure: the core is `todo!()` until it is written.
#[kani::should_panic]
fn no_value_twice() {
    let a = Input::any_sorted();
    let b = Input::any_sorted();
    let deltas = diff(a.get(), b.get());
    for w in deltas.get().windows(2) {
        assert!(w[0].0 < w[1].0);
    }
}
