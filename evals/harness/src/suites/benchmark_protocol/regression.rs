use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::statistics::{effect_size_cohens_d, paired_test};
use crate::TestResult;

use super::types::{BenchmarkReport, CrossCorpusReport, DatasetBenchmarkResult, QueryMetrics};

#[derive(Debug, Clone)]
pub struct RegressionConfig {
    pub baseline_report: PathBuf,
    pub candidate_report: PathBuf,
    pub alpha: f64,
    pub min_effect_size: f64,
    pub report_json: Option<PathBuf>,
    /// Metric(s) to gate on. `all` evaluates recall_at_10, mrr,
    /// precision_at_10, ndcg_at_10. Single-metric values of
    /// `recall_at_10`, `mrr`, `precision_at_10`, or `ndcg_at_<k>` (e.g.
    /// `ndcg_at_10`, `ndcg_at_100`) gate only that metric.
    pub metric: String,
}

#[derive(Debug, Clone)]
enum SingleMetric {
    RecallAt10,
    Mrr,
    PrecisionAt10,
    NdcgAt(usize),
}

impl SingleMetric {
    fn label(&self) -> String {
        match self {
            Self::RecallAt10 => "recall_at_10".into(),
            Self::Mrr => "mrr".into(),
            Self::PrecisionAt10 => "precision_at_10".into(),
            Self::NdcgAt(k) => format!("ndcg_at_{k}"),
        }
    }

    fn extract(&self, q: &QueryMetrics) -> Option<f64> {
        match self {
            Self::RecallAt10 => Some(q.recall_at_10),
            Self::Mrr => Some(q.mrr),
            Self::PrecisionAt10 => Some(q.precision_at_10),
            Self::NdcgAt(k) => q.ndcg_at_k.get(k).copied(),
        }
    }
}

fn parse_metric(raw: &str) -> Result<Vec<SingleMetric>, String> {
    match raw {
        "all" => Ok(vec![
            SingleMetric::RecallAt10,
            SingleMetric::Mrr,
            SingleMetric::PrecisionAt10,
            SingleMetric::NdcgAt(10),
        ]),
        "recall_at_10" => Ok(vec![SingleMetric::RecallAt10]),
        "mrr" => Ok(vec![SingleMetric::Mrr]),
        "precision_at_10" => Ok(vec![SingleMetric::PrecisionAt10]),
        other if other.starts_with("ndcg_at_") => {
            let k: usize = other["ndcg_at_".len()..]
                .parse()
                .map_err(|err| format!("could not parse k in '{other}': {err}"))?;
            if k == 0 {
                return Err(format!("ndcg cutoff must be > 0, got '{other}'"));
            }
            Ok(vec![SingleMetric::NdcgAt(k)])
        }
        _ => Err(format!(
            "unknown --metric '{raw}'; supported: all, recall_at_10, mrr, precision_at_10, ndcg_at_<k>"
        )),
    }
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
    baseline_report_shape: &'static str,
    candidate_report_shape: &'static str,
    alpha: f64,
    min_effect_size: f64,
    metric: String,
    overall_passed: bool,
    paired_query_count: usize,
    metrics: Vec<RegressionMetric>,
}

enum LoadedReport {
    Single(BenchmarkReport),
    CrossCorpus(CrossCorpusReport),
}

impl LoadedReport {
    fn shape(&self) -> &'static str {
        match self {
            Self::Single(_) => "BenchmarkReport",
            Self::CrossCorpus(_) => "CrossCorpusReport",
        }
    }

    fn system_variant(&self) -> &str {
        match self {
            Self::Single(r) => &r.system_variant,
            Self::CrossCorpus(r) => &r.system_variant,
        }
    }
}

pub fn run_regression_gate(config: RegressionConfig) -> Vec<TestResult> {
    let mut results = Vec::new();

    let metrics_to_eval = match parse_metric(&config.metric) {
        Ok(v) => v,
        Err(err) => {
            results.push(TestResult::fail(
                "P6_regression_parse_metric",
                &format!("--metric parse failure: {err}"),
            ));
            return results;
        }
    };

    let baseline = match load_report_either(&config.baseline_report) {
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

    let candidate = match load_report_either(&config.candidate_report) {
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

    if baseline.system_variant() != candidate.system_variant() {
        results.push(TestResult::fail(
            "P6_regression_align_reports",
            &format!(
                "system_variant mismatch: baseline='{}' candidate='{}'",
                baseline.system_variant(),
                candidate.system_variant()
            ),
        ));
        return results;
    }

    let pairs = match build_paired_queries(&baseline, &candidate) {
        Ok(pairs) => pairs,
        Err(err) => {
            results.push(TestResult::fail("P6_regression_align_reports", &err));
            return results;
        }
    };
    results.push(TestResult::pass("P6_regression_align_reports"));

    if pairs.is_empty() {
        results.push(TestResult::fail(
            "P6_regression_align_queries",
            "No overlapping query_id entries between baseline and candidate reports",
        ));
        return results;
    }
    results.push(TestResult::pass("P6_regression_align_queries"));

    let mut metric_reports = Vec::with_capacity(metrics_to_eval.len());
    for metric in &metrics_to_eval {
        let report = evaluate_metric(metric, &pairs, config.alpha, config.min_effect_size);
        let test_name = format!("P6_regression_gate_{}", report.metric);
        if report.gate_passed {
            results.push(TestResult::pass(&test_name));
        } else {
            results.push(TestResult::fail(&test_name, &report.gate_reason));
        }
        metric_reports.push(report);
    }

    let overall_passed = metric_reports.iter().all(|m| m.gate_passed);
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
            baseline_report_shape: baseline.shape(),
            candidate_report_shape: candidate.shape(),
            alpha: config.alpha,
            min_effect_size: config.min_effect_size,
            metric: config.metric.clone(),
            overall_passed,
            paired_query_count: pairs.len(),
            metrics: metric_reports,
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

fn load_report_either(path: &PathBuf) -> Result<LoadedReport, String> {
    let content = fs::read_to_string(path).map_err(|err| format!("read file: {err}"))?;
    match serde_json::from_str::<CrossCorpusReport>(&content) {
        Ok(cross) => Ok(LoadedReport::CrossCorpus(cross)),
        Err(cross_err) => match serde_json::from_str::<BenchmarkReport>(&content) {
            Ok(single) => Ok(LoadedReport::Single(single)),
            Err(single_err) => Err(format!(
                "could not parse as CrossCorpusReport ({cross_err}) or BenchmarkReport ({single_err})"
            )),
        },
    }
}

fn build_paired_queries(
    baseline: &LoadedReport,
    candidate: &LoadedReport,
) -> Result<Vec<(QueryMetrics, QueryMetrics)>, String> {
    match (baseline, candidate) {
        (LoadedReport::Single(b), LoadedReport::Single(c)) => {
            if b.dataset_path != c.dataset_path {
                return Err(format!(
                    "dataset_path mismatch: baseline='{}' candidate='{}'",
                    b.dataset_path, c.dataset_path
                ));
            }
            Ok(align_queries_by_id(&b.query_metrics, &c.query_metrics))
        }
        (LoadedReport::CrossCorpus(b), LoadedReport::CrossCorpus(c)) => {
            let b_by_path: HashMap<&str, &DatasetBenchmarkResult> =
                b.datasets.iter().map(|d| (d.dataset_path.as_str(), d)).collect();
            let mut all_pairs = Vec::new();
            for c_ds in &c.datasets {
                if let Some(b_ds) = b_by_path.get(c_ds.dataset_path.as_str()) {
                    all_pairs.extend(align_queries_by_id(
                        &b_ds.query_metrics,
                        &c_ds.query_metrics,
                    ));
                }
            }
            Ok(all_pairs)
        }
        _ => Err(format!(
            "baseline and candidate report shapes differ ({} vs {}); regenerate one side so both are {} or both are {}",
            baseline.shape(),
            candidate.shape(),
            baseline.shape(),
            candidate.shape(),
        )),
    }
}

fn align_queries_by_id(
    baseline: &[QueryMetrics],
    candidate: &[QueryMetrics],
) -> Vec<(QueryMetrics, QueryMetrics)> {
    let baseline_by_id: HashMap<&str, &QueryMetrics> =
        baseline.iter().map(|m| (m.query_id.as_str(), m)).collect();
    let mut aligned = Vec::new();
    for m in candidate {
        if let Some(base) = baseline_by_id.get(m.query_id.as_str()) {
            aligned.push(((*base).clone(), m.clone()));
        }
    }
    aligned
}

fn evaluate_metric(
    metric: &SingleMetric,
    pairs: &[(QueryMetrics, QueryMetrics)],
    alpha: f64,
    min_effect_size: f64,
) -> RegressionMetric {
    let value_pairs: Vec<(f64, f64)> = pairs
        .iter()
        .filter_map(|(b, c)| {
            let bv = metric.extract(b)?;
            let cv = metric.extract(c)?;
            Some((bv, cv))
        })
        .collect();

    let n_pairs = value_pairs.len();
    if n_pairs == 0 {
        return RegressionMetric {
            metric: metric.label(),
            baseline_mean: 0.0,
            candidate_mean: 0.0,
            mean_difference: 0.0,
            p_value: 1.0,
            effect_size: 0.0,
            wins: 0,
            losses: 0,
            ties: 0,
            n_pairs: 0,
            gate_passed: true,
            gate_reason: format!(
                "skipped: metric '{}' absent from both reports; regenerate baseline with this metric to gate on it",
                metric.label()
            ),
        };
    }

    let baseline_mean = value_pairs.iter().map(|(b, _)| *b).sum::<f64>() / n_pairs as f64;
    let candidate_mean = value_pairs.iter().map(|(_, c)| *c).sum::<f64>() / n_pairs as f64;

    let paired = paired_test(&value_pairs);
    let effect_size = effect_size_cohens_d(&value_pairs);

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
        metric: metric.label(),
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
    use super::super::types::{BenchmarkSummary, MetricWithCi, PhaseTiming, Thresholds};
    use super::*;
    use std::collections::BTreeMap;

    fn metric_ci() -> MetricWithCi {
        MetricWithCi {
            mean: 0.0,
            ci_lower: 0.0,
            ci_upper: 0.0,
            std_dev: 0.0,
            n: 0,
        }
    }

    fn empty_summary() -> BenchmarkSummary {
        BenchmarkSummary {
            recall: metric_ci(),
            mrr: metric_ci(),
            precision: metric_ci(),
            latency_ms: metric_ci(),
            ndcg_at_k: BTreeMap::new(),
        }
    }

    fn dummy_report(dataset_path: &str, system_variant: &str) -> BenchmarkReport {
        BenchmarkReport {
            generated_unix_seconds: 0,
            dataset_path: dataset_path.to_string(),
            dataset_description: "dummy".to_string(),
            dataset_version: "v1".to_string(),
            system_variant: system_variant.to_string(),
            embedding_model: "all-minilm".to_string(),
            bootstrap_iterations: 100,
            seed: 42,
            queries_evaluated: 0,
            documents_indexed: 0,
            thresholds: Thresholds {
                recall: None,
                mrr: None,
                precision: None,
            },
            summary: empty_summary(),
            phase_timing: PhaseTiming::default(),
            quality_gate_passed: true,
            quality_gate_message: "ok".to_string(),
            query_metrics: Vec::new(),
        }
    }

    fn ndcg_metric(query_id: &str, recall: f64, ndcg_at_10: f64) -> QueryMetrics {
        let mut ndcg_at_k = BTreeMap::new();
        ndcg_at_k.insert(10, ndcg_at_10);
        QueryMetrics {
            query_id: query_id.to_string(),
            recall_at_10: recall,
            mrr: 0.0,
            precision_at_10: 0.0,
            latency_ms: 0.0,
            ndcg_at_k,
        }
    }

    #[test]
    fn parse_metric_supports_named_fields_and_ndcg_at_k() {
        assert!(matches!(
            parse_metric("recall_at_10").unwrap().as_slice(),
            [SingleMetric::RecallAt10]
        ));
        assert!(matches!(
            parse_metric("mrr").unwrap().as_slice(),
            [SingleMetric::Mrr]
        ));
        assert!(matches!(
            parse_metric("ndcg_at_10").unwrap().as_slice(),
            [SingleMetric::NdcgAt(10)]
        ));
        assert!(matches!(
            parse_metric("ndcg_at_100").unwrap().as_slice(),
            [SingleMetric::NdcgAt(100)]
        ));
        assert_eq!(parse_metric("all").unwrap().len(), 4);
    }

    #[test]
    fn parse_metric_rejects_unknown_and_zero_cutoff() {
        assert!(parse_metric("garbage").is_err());
        assert!(parse_metric("ndcg_at_0").is_err());
        assert!(parse_metric("ndcg_at_foo").is_err());
    }

    #[test]
    fn gate_fails_on_significant_degradation() {
        let pairs = vec![
            (ndcg_metric("q1", 0.9, 0.0), ndcg_metric("q1", 0.2, 0.0)),
            (ndcg_metric("q2", 0.8, 0.0), ndcg_metric("q2", 0.1, 0.0)),
            (ndcg_metric("q3", 0.7, 0.0), ndcg_metric("q3", 0.2, 0.0)),
            (ndcg_metric("q4", 0.9, 0.0), ndcg_metric("q4", 0.3, 0.0)),
        ];
        let report = evaluate_metric(&SingleMetric::RecallAt10, &pairs, 0.05, 0.1);
        assert!(!report.gate_passed, "{}", report.gate_reason);
    }

    #[test]
    fn gate_passes_on_non_significant_shift() {
        let pairs = vec![
            (ndcg_metric("q1", 0.60, 0.0), ndcg_metric("q1", 0.61, 0.0)),
            (ndcg_metric("q2", 0.62, 0.0), ndcg_metric("q2", 0.60, 0.0)),
            (ndcg_metric("q3", 0.61, 0.0), ndcg_metric("q3", 0.62, 0.0)),
            (ndcg_metric("q4", 0.63, 0.0), ndcg_metric("q4", 0.62, 0.0)),
        ];
        let report = evaluate_metric(&SingleMetric::RecallAt10, &pairs, 0.05, 0.2);
        assert!(report.gate_passed, "{}", report.gate_reason);
    }

    #[test]
    fn gate_skips_metric_absent_from_both_reports() {
        let pairs = vec![
            (ndcg_metric("q1", 0.5, 0.8), ndcg_metric("q1", 0.5, 0.8)),
            (ndcg_metric("q2", 0.5, 0.8), ndcg_metric("q2", 0.5, 0.8)),
        ];
        // ndcg_at_5 is not populated in either side → skip, not fail.
        let report = evaluate_metric(&SingleMetric::NdcgAt(5), &pairs, 0.05, 0.1);
        assert_eq!(report.n_pairs, 0);
        assert!(report.gate_passed);
        assert!(report.gate_reason.contains("skipped"));
    }

    #[test]
    fn align_queries_pairs_only_shared_ids() {
        let baseline = vec![ndcg_metric("q1", 0.5, 0.7), ndcg_metric("q2", 0.5, 0.7)];
        let candidate = vec![ndcg_metric("q2", 0.4, 0.6), ndcg_metric("q3", 0.4, 0.6)];
        let aligned = align_queries_by_id(&baseline, &candidate);
        assert_eq!(aligned.len(), 1);
        assert_eq!(aligned[0].0.query_id, "q2");
        assert_eq!(aligned[0].1.query_id, "q2");
    }

    #[test]
    fn load_report_either_detects_single_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.json");
        let report = dummy_report("dataset.json", "hybrid-feature");
        fs::write(&path, serde_json::to_string(&report).unwrap()).unwrap();
        let loaded = load_report_either(&path).unwrap();
        assert!(matches!(loaded, LoadedReport::Single(_)));
    }

    #[test]
    fn compatibility_rejects_variant_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_path = dir.path().join("baseline.json");
        let candidate_path = dir.path().join("candidate.json");
        fs::write(
            &baseline_path,
            serde_json::to_string(&dummy_report("ds.json", "hybrid-feature")).unwrap(),
        )
        .unwrap();
        fs::write(
            &candidate_path,
            serde_json::to_string(&dummy_report("ds.json", "dense-only")).unwrap(),
        )
        .unwrap();
        let config = RegressionConfig {
            baseline_report: baseline_path,
            candidate_report: candidate_path,
            alpha: 0.05,
            min_effect_size: 0.1,
            report_json: None,
            metric: "all".to_string(),
        };
        let results = run_regression_gate(config);
        assert!(results
            .iter()
            .any(|r| r.name == "P6_regression_align_reports" && !r.passed));
    }

    #[test]
    fn rejects_mixed_report_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_path = dir.path().join("baseline.json");
        let candidate_path = dir.path().join("candidate.json");

        fs::write(
            &baseline_path,
            serde_json::to_string(&dummy_report("ds.json", "hybrid-feature")).unwrap(),
        )
        .unwrap();

        let cross = CrossCorpusReport {
            generated_unix_seconds: 0,
            embedding_model: "all-minilm".to_string(),
            system_variant: "hybrid-feature".to_string(),
            bootstrap_iterations: 100,
            seed: 42,
            max_queries: None,
            max_documents: None,
            normalization: "macro_average_by_dataset".to_string(),
            thresholds: Thresholds {
                recall: None,
                mrr: None,
                precision: None,
            },
            datasets: vec![DatasetBenchmarkResult {
                dataset_path: "ds.json".to_string(),
                dataset_description: "dummy".to_string(),
                dataset_version: "v1".to_string(),
                queries_evaluated: 0,
                documents_indexed: 0,
                summary: empty_summary(),
                quality_gate_passed: true,
                quality_gate_message: "ok".to_string(),
                query_metrics: Vec::new(),
            }],
            normalized_summary: empty_summary(),
            quality_gate_passed: true,
            quality_gate_message: "ok".to_string(),
        };
        fs::write(&candidate_path, serde_json::to_string(&cross).unwrap()).unwrap();

        let config = RegressionConfig {
            baseline_report: baseline_path,
            candidate_report: candidate_path,
            alpha: 0.05,
            min_effect_size: 0.1,
            report_json: None,
            metric: "all".to_string(),
        };
        let results = run_regression_gate(config);
        let failure = results
            .iter()
            .find(|r| r.name == "P6_regression_align_reports" && !r.passed)
            .expect("expected align_reports failure for mixed shapes");
        assert!(failure.message.contains("shapes differ"));
        // Remediation hint must name BOTH shapes so the operator knows the
        // two normalization paths (not just one).
        assert!(failure.message.contains("BenchmarkReport"));
        assert!(failure.message.contains("CrossCorpusReport"));
    }
}
