pub fn series_len(series: &[Vec<f64>]) -> usize {
    series.iter().map(Vec::len).max().unwrap_or(0)
}

pub fn stack(series: &[Vec<f64>]) -> Vec<Vec<(f64, f64)>> {
    let len = series_len(series);
    let mut base = vec![0.0; len];
    series
        .iter()
        .map(|values| {
            (0..len)
                .map(|i| {
                    let v = values.get(i).copied().unwrap_or(0.0).max(0.0);
                    let y0 = base[i];
                    base[i] += v;
                    (y0, base[i])
                })
                .collect()
        })
        .collect()
}

pub fn totals(series: &[Vec<f64>]) -> Vec<f64> {
    let len = series_len(series);
    (0..len)
        .map(|i| {
            series
                .iter()
                .map(|s| s.get(i).copied().unwrap_or(0.0).max(0.0))
                .sum()
        })
        .collect()
}

pub fn max_value(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_sum_to_totals() {
        let s = vec![vec![1.0, 2.0, 0.0], vec![3.0, 0.5], vec![0.0, 1.0, 4.0]];
        let st = stack(&s);
        assert_eq!(st[0], vec![(0.0, 1.0), (0.0, 2.0), (0.0, 0.0)]);
        assert_eq!(st[1], vec![(1.0, 4.0), (2.0, 2.5), (0.0, 0.0)]);
        assert_eq!(st[2], vec![(4.0, 4.0), (2.5, 3.5), (0.0, 4.0)]);
        let t = totals(&s);
        assert_eq!(t, vec![4.0, 3.5, 4.0]);
        for (i, total) in t.iter().enumerate() {
            assert_eq!(st.last().unwrap()[i].1, *total);
        }
        assert_eq!(max_value(&t), 4.0);
    }

    #[test]
    fn negatives_and_empty() {
        let st = stack(&[vec![-1.0, 2.0]]);
        assert_eq!(st[0], vec![(0.0, 0.0), (0.0, 2.0)]);
        assert!(stack(&[]).is_empty());
        assert!(totals(&[]).is_empty());
        assert_eq!(max_value(&[]), 0.0);
    }
}
