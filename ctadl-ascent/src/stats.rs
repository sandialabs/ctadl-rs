use hashbrown::hash_map::HashMap;

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
        let mut nums = vec![1, 2, 3];
        let the_median = median(&mut nums);
        assert_eq!(the_median, Some(2));
    }

    #[test]
    fn test_modes_empty() {
        let empty = Vec::new();
        let the_modes = modes(&empty);
        assert_eq!(the_modes.len(), 0);
    }
}
