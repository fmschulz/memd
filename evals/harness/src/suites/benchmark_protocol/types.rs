use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Cutoffs at which the benchmark protocol computes nDCG. Must stay sorted
/// ascending; the maximum controls how many hits the retriever must return.
pub(super) const NDCG_K_VALUES: &[usize] = &[1, 5, 10, 100];

#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub dataset_paths: Vec<PathBuf>,
    pub system_variant: String,
    pub bootstrap_iterations: usize,
    pub seed: u64,
    pub report_json: Option<PathBuf>,
    pub threshold_recall: Option<f64>,
    pub threshold_mrr: Option<f64>,
    pub threshold_precision: Option<f64>,
    pub max_queries: Option<usize>,
    pub max_documents: Option<usize>,
    pub include_abstention: bool,
    pub max_sessions_per_query: Option<usize>,
    pub max_session_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Dataset {
    pub(super) description: String,
    pub(super) version: String,
    #[serde(default)]
    pub(super) note: Option<String>,
    pub(super) queries: Vec<Query>,
    pub(super) documents: Vec<Document>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Query {
    pub(super) id: String,
    pub(super) query: String,
    pub(super) relevant: Vec<String>,
    /// Graded qrels: doc_id → relevance grade (0 = irrelevant, 1+ = relevant).
    /// When absent, `relevant` entries are synthesized as grade 1 at read time
    /// so existing boolean-relevance datasets are treated as binary qrels.
    #[serde(default)]
    pub(super) relevance_grades: HashMap<String, u8>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Document {
    pub(super) id: String,
    pub(super) text: String,
    #[serde(rename = "type")]
    pub(super) doc_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct QueryMetrics {
    pub(super) query_id: String,
    pub(super) recall_at_10: f64,
    pub(super) mrr: f64,
    pub(super) precision_at_10: f64,
    pub(super) latency_ms: f64,
    /// Per-query nDCG at each cutoff in [`NDCG_K_VALUES`]. Keyed by cutoff
    /// so the regression gate can align metrics across baseline/candidate
    /// reports even if the set of cutoffs changes across versions.
    #[serde(default)]
    pub(super) ndcg_at_k: BTreeMap<usize, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MetricWithCi {
    pub(super) mean: f64,
    pub(super) ci_lower: f64,
    pub(super) ci_upper: f64,
    pub(super) std_dev: f64,
    pub(super) n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BenchmarkSummary {
    pub(super) recall: MetricWithCi,
    pub(super) mrr: MetricWithCi,
    pub(super) precision: MetricWithCi,
    pub(super) latency_ms: MetricWithCi,
    /// Mean nDCG at each cutoff. BTreeMap for deterministic JSON output.
    /// Old baselines without this field deserialize to an empty map.
    #[serde(default)]
    pub(super) ndcg_at_k: BTreeMap<usize, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct PhaseTiming {
    pub(super) load_convert_ms: f64,
    pub(super) cap_filter_ms: f64,
    pub(super) index_ms: f64,
    pub(super) query_ms: f64,
    pub(super) measured_total_ms: f64,
    pub(super) load_convert_pct: f64,
    pub(super) cap_filter_pct: f64,
    pub(super) index_pct: f64,
    pub(super) query_pct: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BenchmarkReport {
    pub(super) generated_unix_seconds: u64,
    pub(super) dataset_path: String,
    pub(super) dataset_description: String,
    pub(super) dataset_version: String,
    #[serde(default = "default_system_variant")]
    pub(super) system_variant: String,
    pub(super) embedding_model: String,
    pub(super) bootstrap_iterations: usize,
    pub(super) seed: u64,
    pub(super) queries_evaluated: usize,
    pub(super) documents_indexed: usize,
    pub(super) thresholds: Thresholds,
    pub(super) summary: BenchmarkSummary,
    #[serde(default)]
    pub(super) phase_timing: PhaseTiming,
    pub(super) quality_gate_passed: bool,
    pub(super) quality_gate_message: String,
    pub(super) query_metrics: Vec<QueryMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DatasetBenchmarkResult {
    pub(super) dataset_path: String,
    pub(super) dataset_description: String,
    pub(super) dataset_version: String,
    pub(super) queries_evaluated: usize,
    pub(super) documents_indexed: usize,
    pub(super) summary: BenchmarkSummary,
    pub(super) quality_gate_passed: bool,
    pub(super) quality_gate_message: String,
    /// Per-query metrics from the originating `BenchmarkReport`. Carried on
    /// the cross-corpus view so the regression gate can do paired-query
    /// tests across datasets. Legacy `CrossCorpusReport`s without the field
    /// deserialize to an empty vector and the gate skips them cleanly.
    #[serde(default)]
    pub(super) query_metrics: Vec<QueryMetrics>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CrossCorpusReport {
    pub(super) generated_unix_seconds: u64,
    pub(super) embedding_model: String,
    pub(super) system_variant: String,
    pub(super) bootstrap_iterations: usize,
    pub(super) seed: u64,
    pub(super) max_queries: Option<usize>,
    pub(super) max_documents: Option<usize>,
    pub(super) normalization: String,
    pub(super) thresholds: Thresholds,
    pub(super) datasets: Vec<DatasetBenchmarkResult>,
    pub(super) normalized_summary: BenchmarkSummary,
    pub(super) quality_gate_passed: bool,
    pub(super) quality_gate_message: String,
}

fn default_system_variant() -> String {
    "hybrid-feature".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Thresholds {
    pub(super) recall: Option<f64>,
    pub(super) mrr: Option<f64>,
    pub(super) precision: Option<f64>,
}
