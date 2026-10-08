use hashbrown::hash_map::HashMap;
use serde::Serialize;

/// Precondition: the vec is sorted.
#[inline]
pub fn median(data: &[usize]) -> Option<usize> {
    let len = data.len();
    if len == 0 {
        return None;
    }
    if len.is_multiple_of(2) {
        Some((data[len / 2 - 1] + data[len / 2]) / 2)
    } else {
        Some(data[len / 2])
    }
}

pub fn quartiles(data: &mut [usize]) -> (Option<usize>, Option<usize>, Option<usize>) {
    data.sort();
    let len = data.len();
    let q2 = median(data);

    if q2.is_none() {
        return (None, None, None);
    }

    let lower_half = &data[0..len / 2];
    let q1 = median(lower_half);
    let upper_half = &data[len / 2..];
    let q3 = median(upper_half);
    (q1, q2, q3)
}

pub fn modes(counts: &[usize]) -> Vec<usize> {
    let mut map = HashMap::new();
    for n in counts {
        let count = map.entry(n).or_insert(0);
        *count += 1;
    }

    let max_value = map.values().max().cloned().unwrap_or_default();

    map.into_iter()
        .filter(|&(_, v)| v == max_value)
        .map(|(&k, _)| k)
        .collect()
}

// --- weighted summaries ----------------------------------------------------
//
// The helpers below take `(value, weight)` pairs rather than a plain slice of values.
// That is not a generalization for its own sake: their caller ([`crate::report`]) measures a
// program whose call sites are grouped by signature, where thousands of sites share one
// target count. Expanding those groups back into one entry per site would cost tens of
// millions of `usize`s to compute a number that the grouped form gives exactly. Pass
// `weight: 1` to get the ordinary unweighted meaning.

/// The `q`-quantile of a weighted sample, by the nearest-rank definition: the smallest value
/// whose cumulative weight reaches `q` of the total.
///
/// `sorted` must be sorted ascending by value; `q` is clamped to `0.0..=1.0`. `None` when the
/// total weight is zero. Weights of zero contribute nothing and are skipped.
///
/// Nearest rank never invents a value that is not in the sample, which is what makes
/// `percentile(&[(v, n)], q)` equal `percentile` over the same sample written out one entry
/// per unit of weight. It therefore disagrees with [`median`] on an even-sized sample, where
/// that function averages the two middle values.
pub fn percentile(sorted: &[(usize, usize)], q: f64) -> Option<usize> {
    let total: usize = sorted.iter().map(|(_, w)| *w).sum();
    if total == 0 {
        return None;
    }
    let q = q.clamp(0.0, 1.0);
    // `ceil`, and at least 1, so q=0 names the first observation and q=1 the last.
    let rank = ((q * total as f64).ceil() as usize).clamp(1, total);
    let mut seen = 0usize;
    for &(value, weight) in sorted {
        seen += weight;
        if seen >= rank {
            return Some(value);
        }
    }
    // Unreachable while `total` is the sum of the same weights, but returning the last value
    // is the right answer if it ever is not.
    sorted.last().map(|(v, _)| *v)
}

/// A summary of a weighted sample: enough of the shape to see a long tail hiding behind a
/// small mean, which an average alone cannot show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Distribution {
    /// Total weight -- the number of observations.
    pub count: usize,
    /// Sum of `value * weight`.
    pub total: usize,
    pub mean: f64,
    pub p50: usize,
    pub p90: usize,
    pub p99: usize,
    pub max: usize,
}

impl Distribution {
    /// Summarizes `(value, weight)` pairs, sorting them ascending by value in place. An empty
    /// sample, or one of total weight zero, gives the all-zero summary.
    pub fn from_weighted(data: &mut [(usize, usize)]) -> Self {
        data.sort_unstable();
        let count: usize = data.iter().map(|(_, w)| *w).sum();
        if count == 0 {
            return Self::default();
        }
        let total: usize = data.iter().map(|(v, w)| v * w).sum();
        Self {
            count,
            total,
            mean: total as f64 / count as f64,
            p50: percentile(data, 0.50).unwrap_or(0),
            p90: percentile(data, 0.90).unwrap_or(0),
            p99: percentile(data, 0.99).unwrap_or(0),
            max: data
                .iter()
                .rev()
                .find(|(_, w)| *w > 0)
                .map(|(v, _)| *v)
                .unwrap_or(0),
        }
    }
}

/// The share of the weighted total that the `n` largest observations account for, in
/// `0.0..=1.0`.
///
/// `sorted_desc` must be sorted descending by value. `n` counts *observations*, not entries:
/// an entry of weight 5 supplies five of them, and is taken partially when `n` falls inside
/// it. Zero when the total is zero.
///
/// This is the "do the worst ten call sites explain it?" number, and the partial take is why
/// it has to be weighted -- the ten worst sites in a program routinely all share one
/// signature.
pub fn top_n_share(sorted_desc: &[(usize, usize)], n: usize) -> f64 {
    let total: usize = sorted_desc.iter().map(|(v, w)| v * w).sum();
    if total == 0 {
        return 0.0;
    }
    let mut taken = 0usize;
    let mut sum = 0usize;
    for &(value, weight) in sorted_desc {
        if taken >= n {
            break;
        }
        let take = weight.min(n - taken);
        sum += value * take;
        taken += take;
    }
    sum as f64 / total as f64
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_median_empty() {
        let empty = Vec::new();
        let the_median = median(&empty);
        assert_eq!(the_median, None);
    }

    #[test]
    fn test_median() {
        let nums = vec![1, 2, 3];
        let the_median = median(&nums);
        assert_eq!(the_median, Some(2));
    }

    /// The whole point of the weighted form: it must agree with the sample written out.
    fn expand(data: &[(usize, usize)]) -> Vec<(usize, usize)> {
        data.iter()
            .flat_map(|&(v, w)| std::iter::repeat_n((v, 1), w))
            .collect()
    }

    #[test]
    fn percentile_matches_the_expanded_sample() {
        let mut weighted = vec![(1usize, 3usize), (4, 1), (9, 6)];
        let mut expanded = expand(&weighted);
        weighted.sort_unstable();
        expanded.sort_unstable();
        for q in [0.0, 0.1, 0.25, 0.5, 0.9, 0.99, 1.0] {
            assert_eq!(
                percentile(&weighted, q),
                percentile(&expanded, q),
                "q = {q}"
            );
        }
        assert_eq!(
            Distribution::from_weighted(&mut weighted),
            Distribution::from_weighted(&mut expanded)
        );
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let data = [(1usize, 1usize), (2, 1), (3, 1)];
        assert_eq!(percentile(&data, 0.0), Some(1));
        assert_eq!(percentile(&data, 0.5), Some(2));
        assert_eq!(percentile(&data, 1.0), Some(3));
        assert_eq!(percentile(&[], 0.5), None);
        // Weight zero is not an observation.
        assert_eq!(percentile(&[(7, 0)], 0.5), None);
    }

    #[test]
    fn distribution_summarizes_a_long_tail() {
        // 99 sites with one target and one with a thousand: the mean says 11, and every
        // other number says where the cost actually is.
        let mut data = vec![(1usize, 99usize), (1000, 1)];
        let d = Distribution::from_weighted(&mut data);
        assert_eq!(d.count, 100);
        assert_eq!(d.total, 1099);
        assert_eq!(d.p50, 1);
        assert_eq!(d.p90, 1);
        assert_eq!(d.p99, 1);
        assert_eq!(d.max, 1000);
        assert!((d.mean - 10.99).abs() < 1e-9);
        assert_eq!(
            Distribution::from_weighted(&mut []),
            Distribution::default()
        );
    }

    #[test]
    fn top_n_share_takes_partial_entries() {
        // One signature at 100 targets used by 5 sites, and 95 sites at 1 target each.
        let data = [(100usize, 5usize), (1, 95)];
        assert_eq!(top_n_share(&data, 0), 0.0);
        // The three worst sites are three of those five, so 300 of 595.
        assert!((top_n_share(&data, 3) - 300.0 / 595.0).abs() < 1e-12);
        // Past the end, everything.
        assert!((top_n_share(&data, 1000) - 1.0).abs() < 1e-12);
        assert_eq!(top_n_share(&[], 10), 0.0);
        assert_eq!(top_n_share(&[(0, 4)], 10), 0.0);
    }

    #[test]
    fn test_modes_empty() {
        let empty = Vec::new();
        let the_modes = modes(&empty);
        assert_eq!(the_modes.len(), 0);
    }
}
