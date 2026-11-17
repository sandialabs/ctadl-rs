//! The difference of two sorted sequences, taken as multisets.

use std::cmp::Ordering;

/// A value whose number of occurrences differs between the two inputs, with
/// both numbers.
pub struct Delta<'a, T> {
    pub row: &'a T,
    pub count_a: usize,
    pub count_b: usize,
}

/// Both inputs sorted by `cmp`. Yields each value whose counts differ, once, in `cmp` order.
///
/// `cmp` must be a total order, and both `a` and `b` must be sorted by it; the result is
/// unspecified otherwise. A value is yielded exactly when the number of elements of `a` equal
/// to it under `cmp` differs from the number in `b`; its `Delta` carries those two numbers as
/// `count_a` and `count_b` (either may be 0), and `row` refers to one of those elements.
pub fn multiset_diff<'a, T, F>(a: &'a [T], b: &'a [T], cmp: F) -> impl Iterator<Item = Delta<'a, T>>
where
    F: Fn(&T, &T) -> Ordering,
{
    debug_assert!(a.is_sorted_by(|x, y| cmp(x, y) != Ordering::Greater));
    debug_assert!(b.is_sorted_by(|x, y| cmp(x, y) != Ordering::Greater));
    MultisetDiff {
        a,
        b,
        cmp,
        i: 0,
        j: 0,
    }
}

/// The merge. `verus/multiset_diff.rs` holds a copy of `next`, statement for statement, with
/// the proof; keep the two in step.
struct MultisetDiff<'a, T, F> {
    a: &'a [T],
    b: &'a [T],
    cmp: F,
    i: usize,
    j: usize,
}

impl<'a, T, F: Fn(&T, &T) -> Ordering> Iterator for MultisetDiff<'a, T, F> {
    type Item = Delta<'a, T>;

    fn next(&mut self) -> Option<Delta<'a, T>> {
        let a = self.a;
        let b = self.b;
        let mut i = self.i;
        let mut j = self.j;
        while i < a.len() || j < b.len() {
            // The next value is the smaller head, `a`'s on a tie. Its own side's run starts
            // past it.
            let from_a = if j >= b.len() {
                true
            } else if i >= a.len() {
                false
            } else {
                !matches!((self.cmp)(&a[i], &b[j]), Ordering::Greater)
            };
            let row: &'a T = if from_a { &a[i] } else { &b[j] };
            let mut ia = if from_a { i + 1 } else { i };
            let mut jb = if from_a { j } else { j + 1 };
            while ia < a.len() && matches!((self.cmp)(&a[ia], row), Ordering::Equal) {
                ia += 1;
            }
            while jb < b.len() && matches!((self.cmp)(&b[jb], row), Ordering::Equal) {
                jb += 1;
            }
            let count_a = ia - i;
            let count_b = jb - j;
            i = ia;
            j = jb;
            if count_a != count_b {
                self.i = i;
                self.j = j;
                return Some(Delta {
                    row,
                    count_a,
                    count_b,
                });
            }
        }
        self.i = i;
        self.j = j;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::multiset_diff;

    #[test]
    fn fixed_examples() {
        let a = [1u8, 1, 2];
        let b = [1u8, 2, 2, 3];
        let got: Vec<_> = multiset_diff(&a, &b, Ord::cmp)
            .map(|d| (*d.row, d.count_a, d.count_b))
            .collect();
        assert_eq!(got, [(1, 2, 1), (2, 1, 2), (3, 0, 1)]);
    }
}
