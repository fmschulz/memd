use rand::seq::SliceRandom;
use rand::thread_rng;

/// Result of a bootstrap confidence interval calculation
#[derive(Debug, Clone)]
pub struct MetricsWithCI {
    pub mean: f64,
    pub ci_lower: f64,
    pub ci_upper: f64,
    pub std_dev: f64,
    pub n: usize,
}

/// Result of a paired statistical test
#[derive(Debug, Clone)]
pub struct PairedTestResult {
    pub mean_difference: f64,
    pub std_dev: f64,
    pub t_statistic: f64,
    pub p_value: f64,
    pub n: usize,
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
}

/// Compute bootstrap confidence interval using percentile method
///
/// # Arguments
/// * `values` - Sample values to bootstrap from
/// * `alpha` - Significance level (e.g., 0.05 for 95% CI)
/// * `iterations` - Number of bootstrap iterations (default: 1000)
///
/// # Returns
/// MetricsWithCI containing mean, CI bounds, standard deviation, and sample size
pub fn bootstrap_ci(values: &[f64], alpha: f64, iterations: usize) -> MetricsWithCI {
    let n = values.len();

    if n == 0 {
        return MetricsWithCI {
            mean: 0.0,
            ci_lower: 0.0,
            ci_upper: 0.0,
            std_dev: 0.0,
            n: 0,
        };
    }

    if n == 1 {
        let val = values[0];
        return MetricsWithCI {
            mean: val,
            ci_lower: val,
            ci_upper: val,
            std_dev: 0.0,
            n: 1,
        };
    }

    // Calculate sample mean and std dev
    let mean: f64 = values.iter().sum::<f64>() / n as f64;
    let variance = values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let std_dev = variance.sqrt();

    // Bootstrap resampling
    let mut rng = thread_rng();
    let mut bootstrap_means = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        // Resample with replacement
        let resample: Vec<f64> = (0..n)
            .map(|_| *values.choose(&mut rng).unwrap())
            .collect();

        let bootstrap_mean = resample.iter().sum::<f64>() / n as f64;
        bootstrap_means.push(bootstrap_mean);
    }

    // Sort to find percentiles
    bootstrap_means.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Calculate percentile indices
    let lower_idx = ((alpha / 2.0) * iterations as f64) as usize;
    let upper_idx = ((1.0 - alpha / 2.0) * iterations as f64) as usize;

    let ci_lower = bootstrap_means[lower_idx.min(iterations - 1)];
    let ci_upper = bootstrap_means[upper_idx.min(iterations - 1)];

    MetricsWithCI {
        mean,
        ci_lower,
        ci_upper,
        std_dev,
        n,
    }
}

/// Compute paired t-test for dependent samples
///
/// # Arguments
/// * `pairs` - Vector of (value_a, value_b) tuples representing paired measurements
///
/// # Returns
/// PairedTestResult containing test statistics, p-value, and win/loss/tie counts
pub fn paired_test(pairs: &[(f64, f64)]) -> PairedTestResult {
    let n = pairs.len();

    if n == 0 {
        return PairedTestResult {
            mean_difference: 0.0,
            std_dev: 0.0,
            t_statistic: 0.0,
            p_value: 1.0,
            n: 0,
            wins: 0,
            losses: 0,
            ties: 0,
        };
    }

    // Calculate differences (B - A)
    let differences: Vec<f64> = pairs.iter().map(|(a, b)| b - a).collect();

    // Count wins/losses/ties
    let wins = differences.iter().filter(|&&d| d > 0.0).count();
    let losses = differences.iter().filter(|&&d| d < 0.0).count();
    let ties = differences.iter().filter(|&&d| d == 0.0).count();

    // Calculate mean difference
    let mean_diff: f64 = differences.iter().sum::<f64>() / n as f64;

    if n == 1 {
        return PairedTestResult {
            mean_difference: mean_diff,
            std_dev: 0.0,
            t_statistic: 0.0,
            p_value: 1.0,
            n: 1,
            wins,
            losses,
            ties,
        };
    }

    // Calculate standard deviation of differences
    let variance = differences.iter()
        .map(|d| (d - mean_diff).powi(2))
        .sum::<f64>() / (n - 1) as f64;
    let std_dev = variance.sqrt();

    // Calculate t-statistic
    let t_statistic = if std_dev > 0.0 {
        mean_diff / (std_dev / (n as f64).sqrt())
    } else {
        0.0
    };

    // Calculate two-tailed p-value using t-distribution approximation
    // For simplicity, using normal approximation for n >= 30
    // For smaller samples, this is less accurate but sufficient for our purposes
    let p_value = if n >= 30 {
        // Normal approximation
        2.0 * (1.0 - approx_normal_cdf(t_statistic.abs()))
    } else {
        // For small samples, use conservative estimate
        // A proper implementation would use the t-distribution
        if t_statistic.abs() < 2.0 {
            1.0 // Not significant
        } else if t_statistic.abs() < 3.0 {
            0.05 // Marginally significant
        } else {
            0.01 // Highly significant
        }
    };

    PairedTestResult {
        mean_difference: mean_diff,
        std_dev,
        t_statistic,
        p_value,
        n,
        wins,
        losses,
        ties,
    }
}

/// Compute Cohen's d effect size for paired samples
///
/// # Arguments
/// * `pairs` - Vector of (value_a, value_b) tuples representing paired measurements
///
/// # Returns
/// Effect size (standardized mean difference)
pub fn effect_size_cohens_d(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len();

    if n == 0 {
        return 0.0;
    }

    let differences: Vec<f64> = pairs.iter().map(|(a, b)| b - a).collect();
    let mean_diff: f64 = differences.iter().sum::<f64>() / n as f64;

    if n == 1 {
        return 0.0;
    }

    let variance = differences.iter()
        .map(|d| (d - mean_diff).powi(2))
        .sum::<f64>() / (n - 1) as f64;
    let std_dev = variance.sqrt();

    if std_dev > 0.0 {
        mean_diff / std_dev
    } else {
        0.0
    }
}

/// Approximate normal CDF using error function approximation
fn approx_normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / 2f64.sqrt()))
}

/// Error function approximation using Abramowitz and Stegun formula
fn erf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();

    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootstrap_ci_known_distribution() {
        // Test with known mean
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let result = bootstrap_ci(&values, 0.05, 1000);

        assert_eq!(result.n, 5);
        assert!((result.mean - 3.0).abs() < 0.01);
        assert!(result.ci_lower < result.mean);
        assert!(result.ci_upper > result.mean);
    }

    #[test]
    fn test_bootstrap_ci_empty() {
        let values: Vec<f64> = vec![];
        let result = bootstrap_ci(&values, 0.05, 1000);

        assert_eq!(result.n, 0);
        assert_eq!(result.mean, 0.0);
    }

    #[test]
    fn test_paired_test_no_difference() {
        let pairs = vec![(1.0, 1.0), (2.0, 2.0), (3.0, 3.0)];
        let result = paired_test(&pairs);

        assert_eq!(result.mean_difference, 0.0);
        assert_eq!(result.wins, 0);
        assert_eq!(result.losses, 0);
        assert_eq!(result.ties, 3);
    }

    #[test]
    fn test_paired_test_positive_difference() {
        let pairs = vec![(1.0, 2.0), (2.0, 3.0), (3.0, 4.0)];
        let result = paired_test(&pairs);

        assert_eq!(result.mean_difference, 1.0);
        assert_eq!(result.wins, 3);
        assert_eq!(result.losses, 0);
        assert_eq!(result.ties, 0);
    }

    #[test]
    fn test_effect_size_cohens_d() {
        // Test with varying differences
        let pairs = vec![(1.0, 2.0), (2.0, 4.0), (3.0, 5.0)];
        let d = effect_size_cohens_d(&pairs);

        // Mean difference is 1.67, std_dev is ~0.58, so d should be ~2.88
        assert!(d > 0.0);
        assert!(d < 5.0);
    }
}
