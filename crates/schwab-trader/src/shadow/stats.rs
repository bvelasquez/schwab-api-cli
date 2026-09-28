//! Small, deterministic statistics for shadow-arm reports.

/// Fixed so a report re-run on the same journals prints the same intervals.
pub const BOOTSTRAP_SEED: u64 = 0x5eed_0a2b_5ad0_0001;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;

pub fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    Some(xs.iter().sum::<f64>() / xs.len() as f64)
}

pub fn win_rate_pct(pnls: &[f64]) -> Option<f64> {
    if pnls.is_empty() {
        return None;
    }
    Some(pnls.iter().filter(|p| **p > 0.0).count() as f64 / pnls.len() as f64 * 100.0)
}

/// Gross wins / gross losses. `None` when there are no losses (undefined).
pub fn profit_factor(pnls: &[f64]) -> Option<f64> {
    let wins: f64 = pnls.iter().filter(|p| **p > 0.0).sum();
    let losses: f64 = pnls.iter().filter(|p| **p < 0.0).map(|p| -p).sum();
    (losses > 0.0).then(|| wins / losses)
}

/// Percentile bootstrap CI for the mean at `level` (e.g. 0.90). Needs ≥ 2 samples.
pub fn bootstrap_mean_ci(xs: &[f64], level: f64, resamples: usize, seed: u64) -> Option<(f64, f64)> {
    if xs.len() < 2 || resamples == 0 {
        return None;
    }
    let mut rng = SplitMix64(seed);
    let n = xs.len();
    let mut means: Vec<f64> = (0..resamples)
        .map(|_| (0..n).map(|_| xs[rng.below(n)]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let alpha = (1.0 - level).clamp(0.0, 1.0);
    let last = (resamples - 1) as f64;
    let lo = means[(alpha / 2.0 * last).floor() as usize];
    let hi = means[((1.0 - alpha / 2.0) * last).ceil() as usize];
    Some((lo, hi))
}

pub fn ci_excludes_zero(ci: Option<(f64, f64)>) -> bool {
    ci.is_some_and(|(lo, hi)| lo > 0.0 || hi < 0.0)
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_and_win_rate() {
        assert_eq!(mean(&[]), None);
        assert_eq!(mean(&[1.0, 2.0, 3.0]), Some(2.0));
        assert_eq!(win_rate_pct(&[1.0, -1.0, 0.0, 2.0]), Some(50.0));
    }

    #[test]
    fn profit_factor_is_gross_win_over_gross_loss() {
        assert_eq!(profit_factor(&[3.0, 1.0, -2.0]), Some(2.0));
        assert_eq!(profit_factor(&[1.0, 2.0]), None);
        assert_eq!(profit_factor(&[-1.0]), Some(0.0));
    }

    #[test]
    fn bootstrap_ci_brackets_mean_and_is_deterministic() {
        let xs = [-2.0, -1.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0];
        let ci = bootstrap_mean_ci(&xs, 0.90, 2_000, BOOTSTRAP_SEED).unwrap();
        let m = mean(&xs).unwrap();
        assert!(ci.0 < m && m < ci.1, "{ci:?} vs mean {m}");
        assert!(ci.0 > -2.0 && ci.1 < 4.0);
        assert_eq!(ci, bootstrap_mean_ci(&xs, 0.90, 2_000, BOOTSTRAP_SEED).unwrap());
    }

    #[test]
    fn bootstrap_ci_degenerate_inputs() {
        assert_eq!(bootstrap_mean_ci(&[1.0], 0.9, 100, 1), None);
        assert_eq!(bootstrap_mean_ci(&[2.0, 2.0, 2.0], 0.9, 100, 1), Some((2.0, 2.0)));
    }

    #[test]
    fn ci_zero_exclusion() {
        assert!(ci_excludes_zero(Some((0.1, 2.0))));
        assert!(ci_excludes_zero(Some((-2.0, -0.1))));
        assert!(!ci_excludes_zero(Some((-0.1, 2.0))));
        assert!(!ci_excludes_zero(None));
    }
}
