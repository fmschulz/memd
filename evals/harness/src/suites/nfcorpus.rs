//! BEIR NFCorpus evaluation suite
//!
//! Tests biomedical/nutrition retrieval using the NFCorpus dataset from BEIR benchmark.
//! Challenging dataset with short vague queries (avg 3.3 words) and long passages (avg 232 words).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure for NFCorpus tests
#[derive(Debug, Deserialize)]
struct NFCorpusDataset {
    description: String,
    version: String,
    #[serde(default)]
    note: Option<String>,
    query_types: Vec<String>,
    queries: Vec<NFCorpusQuery>,
    documents: Vec<NFCorpusDocument>,
}

#[derive(Debug, Deserialize, Clone)]
struct NFCorpusQuery {
    id: String,
    query: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    query_type: String,
    relevant: Vec<String>,
    #[allow(dead_code)]
    irrelevant: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NFCorpusDocument {
    id: String,
    text: String,
    #[serde(rename = "type", default = "default_doc_type")]
    doc_type: String,
}

fn default_doc_type() -> String {
    "biomedical".to_string()
}

/// Quality metrics
#[derive(Debug, Clone, Default)]
pub struct QualityMetrics {
    pub recall_at_10: f64,
    pub mrr: f64,
    pub precision_at_10: f64,
    pub query_count: usize,
}

impl std::fmt::Display for QualityMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Recall@10: {:.3}, MRR: {:.3}, P@10: {:.3} (n={})",
            self.recall_at_10, self.mrr, self.precision_at_10, self.query_count
        )
    }
}

/// Performance metrics
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub query_count: usize,
}

impl std::fmt::Display for PerformanceMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50: {:.1}ms, p90: {:.1}ms, p99: {:.1}ms, mean: {:.1}ms (n={})",
            self.p50_ms, self.p90_ms, self.p99_ms, self.mean_ms, self.query_count
        )
    }
}

/// Extract the text content from an MCP tool call response
fn extract_content_text(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
}

/// Run NFCorpus evaluation tests
pub fn run_nfcorpus_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let dataset_path = std::path::Path::new("evals/datasets/retrieval/beir_nfcorpus.json");
    let dataset = match load_dataset(dataset_path) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "NFCorpus_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== NFCorpus Biomedical Retrieval Suite ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {}", note);
    }
    println!(
        "Queries: {} ({} types), Documents: {}\n",
        dataset.queries.len(),
        dataset.query_types.len(),
        dataset.documents.len()
    );

    // Test 1: Index all documents and evaluate
    let (test_result, quality_metrics) = run_index_and_evaluate(memd_path, &dataset, embedding_model);
    results.push(test_result);

    let metrics = match quality_metrics {
        Some(m) => m,
        None => return results,
    };

    // Test 2: Quality threshold check
    results.push(check_quality_threshold(&metrics));

    // Test 3: Performance baseline
    results.push(run_performance_baseline(memd_path, &dataset, embedding_model));

    results
}

fn load_dataset(path: &std::path::Path) -> Result<NFCorpusDataset, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read file: {}", e))?;

    serde_json::from_str(&content)
        .map_err(|e| format!("parse JSON: {}", e))
}

fn create_indexed_client(
    memd_path: &PathBuf,
    dataset: &NFCorpusDataset,
    embedding_model: &str,
) -> Result<(McpClient, TempDir), String> {
    let data_dir = TempDir::new().map_err(|e| format!("tempdir: {}", e))?;

    let mut client = McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    )
    .map_err(|e| format!("start memd: {}", e))?;

    client
        .initialize()
        .map_err(|e| format!("initialize: {}", e))?;

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_nfcorpus",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        client
            .call_tool("memory.add", params)
            .map_err(|e| format!("add doc {}: {}", doc.id, e))?;
    }

    Ok((client, data_dir))
}

fn run_index_and_evaluate(
    memd_path: &PathBuf,
    dataset: &NFCorpusDataset,
    embedding_model: &str,
) -> (TestResult, Option<QualityMetrics>) {
    let start = Instant::now();
    let name = "NFCorpus_index_and_evaluate";

    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return (TestResult::fail_with_duration(name, &e, start), None);
        }
    };

    println!("  Indexed {} documents", dataset.documents.len());

    // Filter queries with relevance judgments
    let queries_with_qrels: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| !q.relevant.is_empty())
        .collect();

    println!("  Evaluating {} queries with relevance judgments", queries_with_qrels.len());

    // Evaluate queries
    let metrics = evaluate_queries(&mut client, &queries_with_qrels);

    println!("\n  Overall: {}", metrics);

    (
        TestResult::pass_with_duration(name, start),
        Some(metrics),
    )
}

fn check_quality_threshold(metrics: &QualityMetrics) -> TestResult {
    let start = Instant::now();
    let name = "NFCorpus_quality_threshold";

    // Target: 0.25 recall for biomedical semantic retrieval (challenging domain)
    if metrics.recall_at_10 >= 0.25 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "Recall@10 {:.3} below threshold 0.25",
                metrics.recall_at_10
            ),
            start,
        )
    }
}

fn run_performance_baseline(
    memd_path: &PathBuf,
    dataset: &NFCorpusDataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "NFCorpus_performance_baseline";

    // Create fresh client for performance testing
    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(name, &e, start);
        }
    };

    let mut latencies: Vec<f64> = Vec::new();

    // Get queries with qrels only
    let queries_with_qrels: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| !q.relevant.is_empty())
        .collect();

    // Run queries multiple times to get good sample
    let iterations = 3;
    for _ in 0..iterations {
        for query in &queries_with_qrels {
            let query_start = Instant::now();

            let params = serde_json::json!({
                "tenant_id": "eval_nfcorpus",
                "query": query.query,
                "k": 10
            });

            if client.call_tool("memory.search", params).is_ok() {
                latencies.push(query_start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    let metrics = calculate_percentiles(&latencies);

    println!("\n=== Performance Baseline ===");
    println!("  {}", metrics);

    // Targets: p50 < 100ms, p99 < 500ms
    let mut failures = Vec::new();
    if metrics.p50_ms > 100.0 {
        failures.push(format!("p50 {:.1}ms > 100ms", metrics.p50_ms));
    }
    if metrics.p99_ms > 500.0 {
        failures.push(format!("p99 {:.1}ms > 500ms", metrics.p99_ms));
    }

    if failures.is_empty() {
        println!("  Performance targets met (p50 < 100ms, p99 < 500ms)");
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("Performance targets not met: {}", failures.join(", ")),
            start,
        )
    }
}

fn evaluate_queries(client: &mut McpClient, queries: &[&NFCorpusQuery]) -> QualityMetrics {
    let mut total_recall = 0.0;
    let mut total_mrr = 0.0;
    let mut total_precision = 0.0;
    let mut query_count = 0;

    for query in queries {
        if query.relevant.is_empty() {
            continue;
        }

        let params = serde_json::json!({
            "tenant_id": "eval_nfcorpus",
            "query": query.query,
            "k": 10
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let search_results = match parse_search_results(&response) {
            Some(r) => r,
            None => continue,
        };

        let relevant_set: HashSet<&str> = query.relevant.iter().map(|s| s.as_str()).collect();

        // Calculate recall@10
        let found_relevant = search_results
            .iter()
            .filter(|doc_id| relevant_set.contains(doc_id.as_str()))
            .count();
        let recall = found_relevant as f64 / relevant_set.len() as f64;

        // Calculate MRR
        let reciprocal_rank = search_results
            .iter()
            .position(|doc_id| relevant_set.contains(doc_id.as_str()))
            .map(|pos| 1.0 / (pos + 1) as f64)
            .unwrap_or(0.0);

        // Calculate precision@10
        let precision = found_relevant as f64 / search_results.len().min(10) as f64;

        total_recall += recall;
        total_mrr += reciprocal_rank;
        total_precision += precision;
        query_count += 1;
    }

    QualityMetrics {
        recall_at_10: total_recall / query_count as f64,
        mrr: total_mrr / query_count as f64,
        precision_at_10: total_precision / query_count as f64,
        query_count,
    }
}

fn parse_search_results(response: &Value) -> Option<Vec<String>> {
    let text = extract_content_text(response)?;

    let parsed: Value = serde_json::from_str(text).ok()?;

    parsed
        .get("results")
        .and_then(|results| results.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item
                        .get("tags")
                        .and_then(|tags| tags.as_array())
                        .and_then(|tag_arr| tag_arr.first())
                        .and_then(|tag| tag.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
}

fn calculate_percentiles(latencies: &[f64]) -> PerformanceMetrics {
    if latencies.is_empty() {
        return PerformanceMetrics::default();
    }

    let mut sorted = latencies.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50_idx = (sorted.len() as f64 * 0.50) as usize;
    let p90_idx = (sorted.len() as f64 * 0.90) as usize;
    let p99_idx = (sorted.len() as f64 * 0.99) as usize;

    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;

    PerformanceMetrics {
        p50_ms: sorted[p50_idx],
        p90_ms: sorted[p90_idx],
        p99_ms: sorted[p99_idx],
        mean_ms: mean,
        query_count: latencies.len(),
    }
}
