//! Common metrics infrastructure for evaluation suites
//!
//! Provides per-query metric tracking and statistical analysis using bootstrap CIs.

use crate::statistics::{bootstrap_ci, effect_size_cohens_d, paired_test, MetricsWithCI, PairedTestResult};

/// Per-query metrics
#[derive(Debug, Clone)]
pub struct QueryMetrics {
    pub query_id: String,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub precision_at_10: f64,
}

/// Aggregate metrics with confidence intervals
#[derive(Debug, Clone)]
pub struct AggregateMetrics {
    pub recall: MetricsWithCI,
    pub mrr: MetricsWithCI,
    pub precision: MetricsWithCI,
}

impl std::fmt::Display for AggregateMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Recall@10: {:.3} [{:.3}, {:.3}], MRR: {:.3} [{:.3}, {:.3}], P@10: {:.3} [{:.3}, {:.3}] (n={})",
            self.recall.mean, self.recall.ci_lower, self.recall.ci_upper,
            self.mrr.mean, self.mrr.ci_lower, self.mrr.ci_upper,
            self.precision.mean, self.precision.ci_lower, self.precision.ci_upper,
            self.recall.n
        )
    }
}

/// Compute aggregate metrics with bootstrap confidence intervals
///
/// # Arguments
/// * `query_metrics` - Vector of per-query metrics
/// * `alpha` - Significance level (default: 0.05 for 95% CI)
/// * `iterations` - Bootstrap iterations (default: 1000)
///
/// # Returns
/// AggregateMetrics containing means and CIs for all metrics
pub fn compute_aggregate_metrics(
    query_metrics: &[QueryMetrics],
    alpha: f64,
    iterations: usize,
) -> AggregateMetrics {
    let recall_values: Vec<f64> = query_metrics.iter().map(|q| q.recall_at_10).collect();
    let mrr_values: Vec<f64> = query_metrics.iter().map(|q| q.mrr).collect();
    let precision_values: Vec<f64> = query_metrics.iter().map(|q| q.precision_at_10).collect();

    AggregateMetrics {
        recall: bootstrap_ci(&recall_values, alpha, iterations),
        mrr: bootstrap_ci(&mrr_values, alpha, iterations),
        precision: bootstrap_ci(&precision_values, alpha, iterations),
    }
}

/// Comparison results between two models
#[derive(Debug, Clone)]
pub struct ComparisonResult {
    pub model_a_name: String,
    pub model_b_name: String,
    pub recall_comparison: PairedTestResult,
    pub mrr_comparison: PairedTestResult,
    pub precision_comparison: PairedTestResult,
    pub recall_effect_size: f64,
}

impl std::fmt::Display for ComparisonResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n=== Model Comparison: {} vs {} ===", self.model_a_name, self.model_b_name)?;
        writeln!(f, "\nRecall@10:")?;
        writeln!(f, "  Mean difference: {:.3} (B - A)", self.recall_comparison.mean_difference)?;
        writeln!(f, "  t-statistic: {:.3}, p-value: {:.3}", self.recall_comparison.t_statistic, self.recall_comparison.p_value)?;
        writeln!(f, "  Wins: {}, Losses: {}, Ties: {}",
            self.recall_comparison.wins, self.recall_comparison.losses, self.recall_comparison.ties)?;
        writeln!(f, "  Cohen's d: {:.3}", self.recall_effect_size)?;

        let significance = if self.recall_comparison.p_value < 0.01 {
            "highly significant **"
        } else if self.recall_comparison.p_value < 0.05 {
            "significant *"
        } else {
            "not significant"
        };
        writeln!(f, "  Interpretation: {}", significance)?;

        Ok(())
    }
}

/// Compare two sets of query metrics (paired test)
///
/// # Arguments
/// * `model_a_name` - Name of first model
/// * `model_a_metrics` - Per-query metrics for model A
/// * `model_b_name` - Name of second model
/// * `model_b_metrics` - Per-query metrics for model B
///
/// # Returns
/// ComparisonResult with paired test statistics
///
/// # Panics
/// Panics if the two metric vectors have different lengths or query IDs don't match
pub fn compare_models(
    model_a_name: &str,
    model_a_metrics: &[QueryMetrics],
    model_b_name: &str,
    model_b_metrics: &[QueryMetrics],
) -> ComparisonResult {
    assert_eq!(
        model_a_metrics.len(),
        model_b_metrics.len(),
        "Metric vectors must have same length"
    );

    // Verify query IDs match
    for (a, b) in model_a_metrics.iter().zip(model_b_metrics.iter()) {
        assert_eq!(
            a.query_id, b.query_id,
            "Query IDs must match for paired comparison"
        );
    }

    // Create paired data
    let recall_pairs: Vec<(f64, f64)> = model_a_metrics
        .iter()
        .zip(model_b_metrics.iter())
        .map(|(a, b)| (a.recall_at_10, b.recall_at_10))
        .collect();

    let mrr_pairs: Vec<(f64, f64)> = model_a_metrics
        .iter()
        .zip(model_b_metrics.iter())
        .map(|(a, b)| (a.mrr, b.mrr))
        .collect();

    let precision_pairs: Vec<(f64, f64)> = model_a_metrics
        .iter()
        .zip(model_b_metrics.iter())
        .map(|(a, b)| (a.precision_at_10, b.precision_at_10))
        .collect();

    ComparisonResult {
        model_a_name: model_a_name.to_string(),
        model_b_name: model_b_name.to_string(),
        recall_comparison: paired_test(&recall_pairs),
        mrr_comparison: paired_test(&mrr_pairs),
        precision_comparison: paired_test(&precision_pairs),
        recall_effect_size: effect_size_cohens_d(&recall_pairs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_aggregate_metrics() {
        let queries = vec![
            QueryMetrics {
                query_id: "Q1".to_string(),
                recall_at_10: 1.0,
                mrr: 1.0,
                precision_at_10: 0.5,
            },
            QueryMetrics {
                query_id: "Q2".to_string(),
                recall_at_10: 0.8,
                mrr: 0.5,
                precision_at_10: 0.4,
            },
            QueryMetrics {
                query_id: "Q3".to_string(),
                recall_at_10: 0.6,
                mrr: 0.33,
                precision_at_10: 0.3,
            },
        ];

        let metrics = compute_aggregate_metrics(&queries, 0.05, 1000);

        assert_eq!(metrics.recall.n, 3);
        assert!((metrics.recall.mean - 0.8).abs() < 0.01);
        assert!(metrics.recall.ci_lower < metrics.recall.mean);
        assert!(metrics.recall.ci_upper > metrics.recall.mean);
    }

    #[test]
    fn test_compare_models() {
        let model_a = vec![
            QueryMetrics {
                query_id: "Q1".to_string(),
                recall_at_10: 0.8,
                mrr: 0.8,
                precision_at_10: 0.4,
            },
            QueryMetrics {
                query_id: "Q2".to_string(),
                recall_at_10: 0.6,
                mrr: 0.6,
                precision_at_10: 0.3,
            },
        ];

        let model_b = vec![
            QueryMetrics {
                query_id: "Q1".to_string(),
                recall_at_10: 0.9,
                mrr: 0.9,
                precision_at_10: 0.5,
            },
            QueryMetrics {
                query_id: "Q2".to_string(),
                recall_at_10: 0.7,
                mrr: 0.7,
                precision_at_10: 0.4,
            },
        ];

        let comparison = compare_models("model-a", &model_a, "model-b", &model_b);

        assert_eq!(comparison.recall_comparison.wins, 2);
        assert_eq!(comparison.recall_comparison.losses, 0);
        assert!(comparison.recall_comparison.mean_difference > 0.0);
    }
}
