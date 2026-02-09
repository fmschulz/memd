use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::statistics::{effect_size_cohens_d, paired_test};
use crate::TestResult;

use super::types::{BenchmarkReport, QueryMetrics};

#[derive(Debug, Clone)]
pub struct RegressionConfig {
    pub baseline_report: PathBuf,
    pub candidate_report: PathBuf,
    pub alpha: f64,
    pub min_effect_size: f64,
    pub report_json: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct RegressionMetric {
    metric: String,
    baseline_mean: f64,
    candidate_mean: f64,
    mean_difference: f64,
    p_value: f64,
    effect_size: f64,
    wins: usize,
    losses: usize,
    ties: usize,
    n_pairs: usize,
    gate_passed: bool,
    gate_reason: String,
}

#[derive(Debug, Serialize)]
struct RegressionGateReport {
    generated_unix_seconds: u64,
    baseline_report: String,
    candidate_report: String,
    alpha: f64,
    min_effect_size: f64,
    overall_passed: bool,
    paired_query_count: usize,
    metrics: Vec<RegressionMetric>,
}

pub fn run_regression_gate(config: RegressionConfig) -> Vec<TestResult> {
    let mut results = Vec::new();

    let baseline = match load_report(&config.baseline_report) {
        Ok(report) => {
            results.push(TestResult::pass("P6_regression_load_baseline"));
            report
        }
        Err(err) => {
            results.push(TestResult::fail(
                "P6_regression_load_baseline",
                &format!("Failed to load baseline report: {err}"),
            ));
            return results;
        }
    };

    let candidate = match load_report(&config.candidate_report) {
        Ok(report) => {
            results.push(TestResult::pass("P6_regression_load_candidate"));
            report
        }
        Err(err) => {
            results.push(TestResult::fail(
                "P6_regression_load_candidate",
                &format!("Failed to load candidate report: {err}"),
            ));
            return results;
        }
    };

    let aligned = align_metrics(&baseline.query_metrics, &candidate.query_metrics);
    if aligned.is_empty() {
        results.push(TestResult::fail(
            "P6_regression_align_queries",
            "No overlapping query_id entries between baseline and candidate reports",
        ));
        return results;
    }
    results.push(TestResult::pass("P6_regression_align_queries"));

    let metrics = vec![
        evaluate_metric(
            "recall_at_10",
            aligned.iter().map(|(b, c)| (b.recall_at_10, c.recall_at_10)),
            config.alpha,
            config.min_effect_size,
        ),
        evaluate_metric(
            "mrr",
            aligned.iter().map(|(b, c)| (b.mrr, c.mrr)),
            config.alpha,
            config.min_effect_size,
        ),
        evaluate_metric(
            "precision_at_10",
            aligned.iter().map(|(b, c)| (b.precision_at_10, c.precision_at_10)),
            config.alpha,
            config.min_effect_size,
        ),
    ];

    for metric in &metrics {
        let test_name = format!("P6_regression_gate_{}", metric.metric);
        if metric.gate_passed {
            results.push(TestResult::pass(&test_name));
        } else {
            results.push(TestResult::fail(&test_name, &metric.gate_reason));
        }
    }

    let overall_passed = metrics.iter().all(|metric| metric.gate_passed);
    if overall_passed {
        results.push(TestResult::pass("P6_regression_gate"));
    } else {
        results.push(TestResult::fail(
            "P6_regression_gate",
            "Candidate shows statistically meaningful degradation on at least one metric",
        ));
    }

    if let Some(path) = config.report_json {
        let report = RegressionGateReport {
            generated_unix_seconds: now_unix_seconds(),
            baseline_report: config.baseline_report.display().to_string(),
            candidate_report: config.candidate_report.display().to_string(),
            alpha: config.alpha,
            min_effect_size: config.min_effect_size,
            overall_passed,
            paired_query_count: aligned.len(),
            metrics,
        };
        if let Err(err) = write_report(&path, &report) {
            results.push(TestResult::fail(
                "P6_regression_report_write",
                &format!("Failed to write regression report: {err}"),
            ));
        } else {
            results.push(TestResult::pass("P6_regression_report_write"));
        }
    }

    results
}

fn load_report(path: &PathBuf) -> Result<BenchmarkReport, String> {
    let content = fs::read_to_string(path).map_err(|err| format!("read file: {err}"))?;
    serde_json::from_str(&content).map_err(|err| format!("parse json: {err}"))
}

fn align_metrics<'a>(
    baseline: &'a [QueryMetrics],
    candidate: &'a [QueryMetrics],
) -> Vec<(&'a QueryMetrics, &'a QueryMetrics)> {
    let mut baseline_by_query: HashMap<&str, &QueryMetrics> = HashMap::with_capacity(baseline.len());
    for metric in baseline {
        baseline_by_query.insert(metric.query_id.as_str(), metric);
    }

    let mut aligned = Vec::new();
    for metric in candidate {
        if let Some(base) = baseline_by_query.get(metric.query_id.as_str()) {
            aligned.push((*base, metric));
        }
    }
    aligned
}

fn evaluate_metric<I>(
    name: &str,
    pairs: I,
    alpha: f64,
    min_effect_size: f64,
) -> RegressionMetric
where
    I: IntoIterator<Item = (f64, f64)>,
{
    let pairs: Vec<(f64, f64)> = pairs.into_iter().collect();
    let n_pairs = pairs.len();

    let baseline_mean = if n_pairs == 0 {
        0.0
    } else {
        pairs.iter().map(|(b, _)| *b).sum::<f64>() / n_pairs as f64
    };
    let candidate_mean = if n_pairs == 0 {
        0.0
    } else {
        pairs.iter().map(|(_, c)| *c).sum::<f64>() / n_pairs as f64
    };

    let paired = paired_test(&pairs);
    let effect_size = effect_size_cohens_d(&pairs);

    let significant_degradation = paired.mean_difference < 0.0
        && paired.p_value <= alpha
        && effect_size.abs() >= min_effect_size;

    let gate_passed = !significant_degradation;
    let gate_reason = if gate_passed {
        "No statistically significant practical regression detected".to_string()
    } else {
        format!(
            "candidate mean ({:.4}) < baseline mean ({:.4}) with p={:.4} and |d|={:.4}",
            candidate_mean,
            baseline_mean,
            paired.p_value,
            effect_size.abs()
        )
    };

    RegressionMetric {
        metric: name.to_string(),
        baseline_mean,
        candidate_mean,
        mean_difference: paired.mean_difference,
        p_value: paired.p_value,
        effect_size,
        wins: paired.wins,
        losses: paired.losses,
        ties: paired.ties,
        n_pairs,
        gate_passed,
        gate_reason,
    }
}

fn write_report(path: &PathBuf, report: &RegressionGateReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create report dir: {err}"))?;
    }
    let content =
        serde_json::to_string_pretty(report).map_err(|err| format!("serialize report: {err}"))?;
    fs::write(path, content).map_err(|err| format!("write report file: {err}"))
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_fails_on_significant_degradation() {
        let metric = evaluate_metric(
            "recall_at_10",
            vec![(0.9, 0.2), (0.8, 0.1), (0.7, 0.2), (0.9, 0.3)],
            0.05,
            0.1,
        );
        assert!(!metric.gate_passed);
    }

    #[test]
    fn gate_passes_on_non_significant_shift() {
        let metric = evaluate_metric(
            "mrr",
            vec![(0.6, 0.61), (0.62, 0.60), (0.61, 0.62), (0.63, 0.62)],
            0.05,
            0.2,
        );
        assert!(metric.gate_passed);
    }
}
