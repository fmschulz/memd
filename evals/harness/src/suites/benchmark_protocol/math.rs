use std::collections::{HashMap, HashSet};

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

/// Standard BEIR nDCG@k using `2^rel - 1` gain with `log2(rank + 1)` discount.
///
/// * `retrieved` — ordered doc IDs, index 0 = rank 1.
/// * `grades` — qrels: doc ID → graded relevance (0 = irrelevant, 1+ = relevant).
///   Missing doc IDs are treated as grade 0.
/// * `k` — cutoff rank (only the first `k` retrieved positions contribute to DCG;
///   only the top-`k` grades contribute to iDCG).
///
/// Returns `0.0` when `iDCG@k == 0` (no relevant documents known for this
/// query or `k == 0`). Callers decide whether such queries are dropped from
/// the dataset average.
// Wired into QueryMetrics in P2 of docs/plans/active/2026-04-21-beir-retrieval-gate.md.
#[allow(dead_code)]
pub(super) fn calculate_ndcg(
    retrieved: &[String],
    grades: &HashMap<String, u8>,
    k: usize,
) -> f64 {
    if k == 0 {
        return 0.0;
    }

    let dcg: f64 = retrieved
        .iter()
        .take(k)
        .enumerate()
        .map(|(index, doc_id)| {
            let grade = grades.get(doc_id).copied().unwrap_or(0) as f64;
            if grade == 0.0 {
                0.0
            } else {
                (2f64.powf(grade) - 1.0) / ((index + 2) as f64).log2()
            }
        })
        .sum();

    let mut ideal: Vec<u8> = grades.values().copied().filter(|g| *g > 0).collect();
    ideal.sort_by(|a, b| b.cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(k)
        .enumerate()
        .map(|(index, grade)| (2f64.powf(*grade as f64) - 1.0) / ((index + 2) as f64).log2())
        .sum();

    if idcg <= 0.0 {
        0.0
    } else {
        dcg / idcg
    }
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

    mod ndcg {
        use super::super::*;
        use std::collections::HashMap;

        fn grades(entries: &[(&str, u8)]) -> HashMap<String, u8> {
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect()
        }

        #[test]
        fn binary_perfect_ranking_is_one() {
            let retrieved = vec!["a".to_string()];
            let g = grades(&[("a", 1)]);
            assert!((calculate_ndcg(&retrieved, &g, 10) - 1.0).abs() < 1e-12);
        }

        #[test]
        fn graded_perfect_ranking_is_one() {
            let retrieved = vec!["a".to_string(), "b".to_string(), "c".to_string()];
            let g = grades(&[("a", 3), ("b", 2), ("c", 1)]);
            assert!((calculate_ndcg(&retrieved, &g, 3) - 1.0).abs() < 1e-12);
        }

        #[test]
        fn graded_reverse_ranking_matches_textbook_value() {
            // Classic textbook case: perfect ideal ranking is [3, 2, 1];
            // we hand the model the reverse [1, 2, 3].
            // DCG@3  = 1/log2(2) + 3/log2(3) + 7/log2(4)
            //        = 1 + 1.8927892607143721 + 3.5
            //        = 6.3927892607143721
            // iDCG@3 = 7/log2(2) + 3/log2(3) + 1/log2(4)
            //        = 7 + 1.8927892607143721 + 0.5
            //        = 9.392789260714372
            // nDCG@3 = 0.6806060567602009
            let retrieved = vec!["c".to_string(), "b".to_string(), "a".to_string()];
            let g = grades(&[("a", 3), ("b", 2), ("c", 1)]);
            let got = calculate_ndcg(&retrieved, &g, 3);
            assert!((got - 0.680_606_056_760_200_9).abs() < 1e-12, "got {got}");
        }

        #[test]
        fn retrieved_outside_qrels_is_zero() {
            let retrieved = vec!["x".to_string(), "y".to_string()];
            let g = grades(&[("a", 1)]);
            assert_eq!(calculate_ndcg(&retrieved, &g, 10), 0.0);
        }

        #[test]
        fn empty_grades_is_zero() {
            let retrieved = vec!["a".to_string()];
            let g: HashMap<String, u8> = HashMap::new();
            assert_eq!(calculate_ndcg(&retrieved, &g, 10), 0.0);
        }

        #[test]
        fn zero_k_is_zero() {
            let retrieved = vec!["a".to_string()];
            let g = grades(&[("a", 1)]);
            assert_eq!(calculate_ndcg(&retrieved, &g, 0), 0.0);
        }

        #[test]
        fn idcg_caps_at_known_relevant_docs() {
            // Only 1 retrieved, 2 relevant:
            //   DCG@5  = 1/log2(2) = 1
            //   iDCG@5 = 1/log2(2) + 1/log2(3) = 1 + 0.6309297535714574
            //          = 1.6309297535714574
            //   nDCG@5 = 0.6131471927654585
            let retrieved = vec!["a".to_string()];
            let g = grades(&[("a", 1), ("b", 1)]);
            let got = calculate_ndcg(&retrieved, &g, 5);
            assert!((got - 0.613_147_192_765_458_5).abs() < 1e-12, "got {got}");
        }

        #[test]
        fn cutoff_excludes_positions_beyond_k() {
            // Perfect ranking but ask only @1 — the highest-grade doc alone
            // against an ideal of the single highest grade still equals 1.0.
            let retrieved = vec!["a".to_string(), "b".to_string()];
            let g = grades(&[("a", 3), ("b", 2)]);
            assert!((calculate_ndcg(&retrieved, &g, 1) - 1.0).abs() < 1e-12);
        }

        #[test]
        fn mixed_ranking_with_gap_matches_manual_calculation() {
            // retrieved = [a, b, c, d]; grades = a=3, b=2, c=3, d=0
            // DCG@4  = 7/log2(2) + 3/log2(3) + 7/log2(4) + 0
            //        = 7 + 1.8927892607143721 + 3.5
            //        = 12.392789260714372
            // iDCG@4 (grades sorted desc [3,3,2]) = 7/log2(2) + 7/log2(3) + 3/log2(4)
            //        = 7 + 4.416508274709534 + 1.5
            //        = 12.916508274709534
            // nDCG@4 = 0.9594535145926796
            let retrieved = vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ];
            let g = grades(&[("a", 3), ("b", 2), ("c", 3), ("d", 0)]);
            let got = calculate_ndcg(&retrieved, &g, 4);
            assert!((got - 0.959_453_514_592_679_6).abs() < 1e-12, "got {got}");
        }

        #[test]
        fn k_larger_than_retrieved_still_normalizes_correctly() {
            let retrieved = vec!["a".to_string()];
            let g = grades(&[("a", 1)]);
            // k=100 but only one retrieved → DCG=1, iDCG=1 (only one relevant) → 1.0
            assert!((calculate_ndcg(&retrieved, &g, 100) - 1.0).abs() < 1e-12);
        }
    }
}
