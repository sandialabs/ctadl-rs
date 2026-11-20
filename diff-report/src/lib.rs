//! The pieces of a multiset diff's report that do not depend on what is compared: the bounded
//! sample of differing values on one side, and its text block.

use std::cmp::Ordering;
use std::io::{self, Write};

use multiset_diff::Delta;
use serde::Serialize;

/// The first differing values by content, and how many more there are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Samples {
    pub rows: Vec<Sample>,
    pub not_shown: usize,
}

/// A value, printed by content, and how many times each side holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub row: String,
    pub count_a: usize,
    pub count_b: usize,
}

impl Samples {
    /// How many values differ this way, shown or not.
    pub fn total(&self) -> usize {
        self.rows.len() + self.not_shown
    }
}

/// The first `limit` of the values pushed, by `cmp`, kept in a buffer of at most `2 * limit`.
pub struct Smallest<'r, T> {
    limit: usize,
    buf: Vec<Delta<'r, T>>,
    total: usize,
}

impl<'r, T> Smallest<'r, T> {
    pub fn new(limit: usize) -> Self {
        Smallest {
            limit,
            buf: Vec::new(),
            total: 0,
        }
    }

    pub fn push(&mut self, d: Delta<'r, T>, cmp: impl Fn(&T, &T) -> Ordering) {
        self.total += 1;
        if self.limit == 0 {
            return;
        }
        self.buf.push(d);
        if self.buf.len() >= self.limit.saturating_mul(2) {
            self.buf
                .select_nth_unstable_by(self.limit, |x, y| cmp(x.row, y.row));
            self.buf.truncate(self.limit);
        }
    }

    /// The first `limit` values pushed, in `cmp` order, each printed by `render`.
    pub fn finish(
        mut self,
        cmp: impl Fn(&T, &T) -> Ordering,
        render: impl Fn(&T) -> String,
    ) -> Samples {
        self.buf.sort_unstable_by(|x, y| cmp(x.row, y.row));
        self.buf.truncate(self.limit);
        Samples {
            not_shown: self.total - self.buf.len(),
            rows: self
                .buf
                .iter()
                .map(|d| Sample {
                    row: render(d.row),
                    count_a: d.count_a,
                    count_b: d.count_b,
                })
                .collect(),
        }
    }
}

/// Writes the values `side` has more of: a line `more in <side>: <total> <unit>, <n> not
/// shown`, then one line per sample, both after `indent` and the samples two spaces further
/// in. Writes nothing when no value differs this way.
pub fn write_samples(
    w: &mut impl Write,
    side: &str,
    samples: &Samples,
    unit: &str,
    indent: &str,
) -> io::Result<()> {
    if samples.total() == 0 {
        return Ok(());
    }
    writeln!(
        w,
        "{indent}more in {side}: {} {unit}, {} not shown",
        samples.total(),
        samples.not_shown
    )?;
    for s in &samples.rows {
        writeln!(w, "{indent}  [A {}, B {}] {}", s.count_a, s.count_b, s.row)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(row: &u32) -> Delta<'_, u32> {
        Delta {
            row,
            count_a: 1,
            count_b: 0,
        }
    }

    #[test]
    fn smallest_keeps_first_limit() {
        let values: Vec<u32> = (0..30).collect();
        let mut s = Smallest::new(20);
        for v in values.iter().rev() {
            s.push(delta(v), Ord::cmp);
        }
        let samples = s.finish(Ord::cmp, u32::to_string);
        let shown: Vec<String> = samples.rows.iter().map(|r| r.row.clone()).collect();
        let want: Vec<String> = (0..20).map(|v: u32| v.to_string()).collect();
        assert_eq!(shown, want);
        assert_eq!(samples.not_shown, 10);
        assert_eq!(samples.total(), 30);
    }

    #[test]
    fn smallest_limit_zero() {
        let values: Vec<u32> = (0..5).collect();
        let mut s = Smallest::new(0);
        for v in &values {
            s.push(delta(v), Ord::cmp);
        }
        let samples = s.finish(Ord::cmp, u32::to_string);
        assert!(samples.rows.is_empty());
        assert_eq!(samples.not_shown, 5);
    }

    #[test]
    fn write_samples_layout() {
        let samples = Samples {
            rows: vec![
                Sample {
                    row: "x".to_string(),
                    count_a: 1,
                    count_b: 0,
                },
                Sample {
                    row: "y".to_string(),
                    count_a: 3,
                    count_b: 2,
                },
            ],
            not_shown: 0,
        };
        let mut out = Vec::new();
        write_samples(&mut out, "A", &samples, "results", "").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "more in A: 2 results, 0 not shown\n  [A 1, B 0] x\n  [A 3, B 2] y\n"
        );

        let mut out = Vec::new();
        write_samples(&mut out, "B", &Samples::default(), "results", "").unwrap();
        assert!(out.is_empty());
    }
}
