use std::collections::HashSet;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::types::{
    BenchmarkConfig, BenchmarkSummary, DatasetBenchmarkResult, MetricWithCi, QueryMetrics,
};

pub(super) fn calculate_recall(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let retrieved_set: HashSet<_> = retrieved.iter().take(10).cloned().collect();
    relevant.intersection(&retrieved_set).count() as f64 / relevant.len() as f64
}

pub(super) fn calculate_reciprocal_rank(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    for (index, doc_id) in retrieved.iter().enumerate() {
        if relevant.contains(doc_id) {
            return 1.0 / (index + 1) as f64;
        }
    }
    0.0
}

pub(super) fn calculate_precision(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    let retrieved_set: HashSet<_> = retrieved.iter().take(10).cloned().collect();
    if retrieved_set.is_empty() {
        return 0.0;
    }
    relevant.intersection(&retrieved_set).count() as f64 / retrieved_set.len() as f64
}

pub(super) fn summarize(
    metrics: &[QueryMetrics],
    iterations: usize,
    seed: u64,
) -> BenchmarkSummary {
    let recalls: Vec<f64> = metrics.iter().map(|m| m.recall_at_10).collect();
    let mrrs: Vec<f64> = metrics.iter().map(|m| m.mrr).collect();
    let precisions: Vec<f64> = metrics.iter().map(|m| m.precision_at_10).collect();
    let latencies: Vec<f64> = metrics.iter().map(|m| m.latency_ms).collect();
    BenchmarkSummary {
        recall: bootstrap_ci(&recalls, iterations, seed),
        mrr: bootstrap_ci(&mrrs, iterations, seed + 1),
        precision: bootstrap_ci(&precisions, iterations, seed + 2),
        latency_ms: bootstrap_ci(&latencies, iterations, seed + 3),
    }
}

pub(super) fn summarize_cross_corpus(
    datasets: &[DatasetBenchmarkResult],
    iterations: usize,
    seed: u64,
) -> BenchmarkSummary {
    let recalls: Vec<f64> = datasets.iter().map(|d| d.summary.recall.mean).collect();
    let mrrs: Vec<f64> = datasets.iter().map(|d| d.summary.mrr.mean).collect();
    let precisions: Vec<f64> = datasets.iter().map(|d| d.summary.precision.mean).collect();
    let latencies: Vec<f64> = datasets.iter().map(|d| d.summary.latency_ms.mean).collect();
    BenchmarkSummary {
        recall: bootstrap_ci(&recalls, iterations, seed),
        mrr: bootstrap_ci(&mrrs, iterations, seed + 1),
        precision: bootstrap_ci(&precisions, iterations, seed + 2),
        latency_ms: bootstrap_ci(&latencies, iterations, seed + 3),
    }
}

fn bootstrap_ci(values: &[f64], iterations: usize, seed: u64) -> MetricWithCi {
    if values.is_empty() {
        return MetricWithCi {
            mean: 0.0,
            ci_lower: 0.0,
            ci_upper: 0.0,
            std_dev: 0.0,
            n: 0,
        };
    }
    if values.len() == 1 {
        return MetricWithCi {
            mean: values[0],
            ci_lower: values[0],
            ci_upper: values[0],
            std_dev: 0.0,
            n: 1,
        };
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance =
        values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    let std_dev = variance.sqrt();
    let rounds = iterations.max(10);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut means = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let mut sample_sum = 0.0;
        for _ in 0..values.len() {
            let idx = rng.gen_range(0..values.len());
            sample_sum += values[idx];
        }
        means.push(sample_sum / values.len() as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lower_idx = (0.025_f64 * rounds as f64) as usize;
    let upper_idx = (0.975_f64 * rounds as f64) as usize;
    MetricWithCi {
        mean,
        ci_lower: means[lower_idx.min(rounds - 1)],
        ci_upper: means[upper_idx.min(rounds - 1)],
        std_dev,
        n: values.len(),
    }
}

pub(super) fn evaluate_quality_gate(
    summary: &BenchmarkSummary,
    config: &BenchmarkConfig,
) -> (bool, String) {
    let mut failures = Vec::new();
    if let Some(threshold) = config.threshold_recall {
        if summary.recall.mean < threshold {
            failures.push(format!(
                "Recall@10 {:.3} below threshold {:.3}",
                summary.recall.mean, threshold
            ));
        }
    }
    if let Some(threshold) = config.threshold_mrr {
        if summary.mrr.mean < threshold {
            failures.push(format!(
                "MRR {:.3} below threshold {:.3}",
                summary.mrr.mean, threshold
            ));
        }
    }
    if let Some(threshold) = config.threshold_precision {
        if summary.precision.mean < threshold {
            failures.push(format!(
                "P@10 {:.3} below threshold {:.3}",
                summary.precision.mean, threshold
            ));
        }
    }
    if failures.is_empty() {
        (true, "All configured thresholds satisfied".to_string())
    } else {
        (false, failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(mean: f64) -> MetricWithCi {
        MetricWithCi {
            mean,
            ci_lower: mean,
            ci_upper: mean,
            std_dev: 0.0,
            n: 1,
        }
    }

    fn dataset_result(
        name: &str,
        recall: f64,
        mrr: f64,
        precision: f64,
        latency_ms: f64,
    ) -> DatasetBenchmarkResult {
        DatasetBenchmarkResult {
            dataset_path: format!("{name}.json"),
            dataset_description: name.to_string(),
            dataset_version: "1.0".to_string(),
            queries_evaluated: 1,
            documents_indexed: 1,
            summary: BenchmarkSummary {
                recall: metric(recall),
                mrr: metric(mrr),
                precision: metric(precision),
                latency_ms: metric(latency_ms),
            },
            quality_gate_passed: true,
            quality_gate_message: String::new(),
        }
    }

    #[test]
    fn bootstrap_ci_is_seed_deterministic() {
        let values = vec![0.1, 0.2, 0.3, 0.9];
        let a = bootstrap_ci(&values, 100, 42);
        let b = bootstrap_ci(&values, 100, 42);
        assert!((a.mean - b.mean).abs() < 1e-9);
        assert!((a.ci_lower - b.ci_lower).abs() < 1e-9);
        assert!((a.ci_upper - b.ci_upper).abs() < 1e-9);
    }

    #[test]
    fn recall_for_empty_relevant_is_one() {
        let relevant = HashSet::new();
        let retrieved = vec!["a".to_string(), "b".to_string()];
        assert_eq!(calculate_recall(&retrieved, &relevant), 1.0);
    }

    #[test]
    fn cross_corpus_summary_uses_macro_average() {
        let datasets = vec![
            dataset_result("small", 0.1, 0.2, 0.3, 100.0),
            dataset_result("large", 0.9, 0.8, 0.7, 200.0),
        ];

        let summary = summarize_cross_corpus(&datasets, 200, 42);

        assert!((summary.recall.mean - 0.5).abs() < 1e-9);
        assert!((summary.mrr.mean - 0.5).abs() < 1e-9);
        assert!((summary.precision.mean - 0.5).abs() < 1e-9);
        assert!((summary.latency_ms.mean - 150.0).abs() < 1e-9);
        assert_eq!(summary.recall.n, 2);
    }

    #[test]
    fn cross_corpus_summary_is_seed_deterministic() {
        let datasets = vec![
            dataset_result("fiqa", 0.4, 0.3, 0.2, 120.0),
            dataset_result("scidocs", 0.5, 0.4, 0.3, 130.0),
            dataset_result("trec", 0.6, 0.5, 0.4, 140.0),
        ];

        let a = summarize_cross_corpus(&datasets, 100, 42);
        let b = summarize_cross_corpus(&datasets, 100, 42);
        assert!((a.recall.ci_lower - b.recall.ci_lower).abs() < 1e-9);
        assert!((a.recall.ci_upper - b.recall.ci_upper).abs() < 1e-9);
        assert!((a.mrr.ci_lower - b.mrr.ci_lower).abs() < 1e-9);
        assert!((a.precision.ci_upper - b.precision.ci_upper).abs() < 1e-9);
    }
}
