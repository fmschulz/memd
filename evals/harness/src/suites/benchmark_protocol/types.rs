use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub dataset_paths: Vec<PathBuf>,
    pub bootstrap_iterations: usize,
    pub seed: u64,
    pub report_json: Option<PathBuf>,
    pub threshold_recall: Option<f64>,
    pub threshold_mrr: Option<f64>,
    pub threshold_precision: Option<f64>,
    pub max_queries: Option<usize>,
    pub max_documents: Option<usize>,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BenchmarkReport {
    pub(super) generated_unix_seconds: u64,
    pub(super) dataset_path: String,
    pub(super) dataset_description: String,
    pub(super) dataset_version: String,
    pub(super) embedding_model: String,
    pub(super) bootstrap_iterations: usize,
    pub(super) seed: u64,
    pub(super) queries_evaluated: usize,
    pub(super) documents_indexed: usize,
    pub(super) thresholds: Thresholds,
    pub(super) summary: BenchmarkSummary,
    pub(super) quality_gate_passed: bool,
    pub(super) quality_gate_message: String,
    pub(super) query_metrics: Vec<QueryMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct DatasetBenchmarkResult {
    pub(super) dataset_path: String,
    pub(super) dataset_description: String,
    pub(super) dataset_version: String,
    pub(super) queries_evaluated: usize,
    pub(super) documents_indexed: usize,
    pub(super) summary: BenchmarkSummary,
    pub(super) quality_gate_passed: bool,
    pub(super) quality_gate_message: String,
}

#[derive(Debug, Serialize)]
pub(super) struct CrossCorpusReport {
    pub(super) generated_unix_seconds: u64,
    pub(super) embedding_model: String,
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

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Thresholds {
    pub(super) recall: Option<f64>,
    pub(super) mrr: Option<f64>,
    pub(super) precision: Option<f64>,
}
