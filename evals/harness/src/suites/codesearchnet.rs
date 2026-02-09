//! CodeSearchNet evaluation suite
//!
//! Tests code search using natural language queries on Python code snippets.
//! This suite evaluates semantic retrieval quality on code documentation.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure for CodeSearchNet tests
#[derive(Debug, Deserialize)]
struct CodeSearchNetDataset {
    description: String,
    version: String,
    #[serde(default)]
    note: Option<String>,
    query_types: Vec<String>,
    queries: Vec<CodeSearchQuery>,
    documents: Vec<CodeSearchDocument>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodeSearchQuery {
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
struct CodeSearchDocument {
    id: String,
    text: String,
    #[serde(rename = "type")]
    doc_type: String,
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

/// Run CodeSearchNet evaluation tests
pub fn run_codesearchnet_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let dataset_path =
        crate::resolve_dataset_path("evals/datasets/retrieval/codesearchnet_python.json");
    let dataset = match load_dataset(dataset_path.as_path()) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "CodeSearchNet_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== CodeSearchNet Retrieval Suite ===");
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
    let (test_result, quality_metrics) =
        run_index_and_evaluate(memd_path, &dataset, embedding_model);
    results.push(test_result);

    let metrics = match quality_metrics {
        Some(m) => m,
        None => return results,
    };

    // Test 2: Quality threshold check
    results.push(check_quality_threshold(&metrics));

    // Test 3: Performance baseline
    results.push(run_performance_baseline(
        memd_path,
        &dataset,
        embedding_model,
    ));

    results
}

fn load_dataset(path: &std::path::Path) -> Result<CodeSearchNetDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    let mut dataset: CodeSearchNetDataset =
        serde_json::from_str(&content).map_err(|e| format!("parse json: {}", e))?;

    for doc in &mut dataset.documents {
        let raw_type = doc.doc_type.clone();
        let Some(normalized) = crate::normalize_eval_chunk_type(&raw_type) else {
            return Err(format!(
                "unsupported chunk type '{}' for document {}",
                raw_type, doc.id
            ));
        };
        doc.doc_type = normalized.to_string();
    }

    Ok(dataset)
}

/// Create a client, index documents, and optionally run queries
fn create_indexed_client(
    memd_path: &PathBuf,
    dataset: &CodeSearchNetDataset,
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
            "tenant_id": "eval_codesearchnet",
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
    dataset: &CodeSearchNetDataset,
    embedding_model: &str,
) -> (TestResult, Option<QualityMetrics>) {
    let start = Instant::now();
    let name = "CodeSearchNet_index_and_evaluate";

    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return (TestResult::fail_with_duration(name, &e, start), None);
        }
    };

    println!("  Indexed {} documents", dataset.documents.len());

    // Evaluate all queries
    let metrics = evaluate_queries(&mut client, &dataset.queries);

    println!("\n  Overall: {}", metrics);

    (TestResult::pass_with_duration(name, start), Some(metrics))
}

fn check_quality_threshold(metrics: &QualityMetrics) -> TestResult {
    let start = Instant::now();
    let name = "CodeSearchNet_quality_threshold";

    // Target: 0.6 recall for code search (challenging cross-domain task)
    if metrics.recall_at_10 >= 0.6 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("Recall@10 {:.3} below threshold 0.6", metrics.recall_at_10),
            start,
        )
    }
}

fn run_performance_baseline(
    memd_path: &PathBuf,
    dataset: &CodeSearchNetDataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "CodeSearchNet_performance_baseline";

    // Create fresh client for performance testing
    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(name, &e, start);
        }
    };

    let mut latencies: Vec<f64> = Vec::new();

    // Run queries multiple times to get good sample
    let iterations = 3;
    for _ in 0..iterations {
        for query in &dataset.queries {
            let query_start = Instant::now();

            let params = serde_json::json!({
                "tenant_id": "eval_codesearchnet",
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
        println!("  Performance targets missed: {}", failures.join(", "));
        TestResult::fail_with_duration(name, &failures.join("; "), start)
    }
}

fn evaluate_queries(client: &mut McpClient, queries: &[CodeSearchQuery]) -> QualityMetrics {
    let mut total_recall = 0.0;
    let mut total_rr = 0.0;
    let mut total_precision = 0.0;
    let mut evaluated = 0;

    for query in queries {
        let params = serde_json::json!({
            "tenant_id": "eval_codesearchnet",
            "query": query.query,
            "k": 10
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Query {} failed: {}", query.id, e);
                continue;
            }
        };

        let retrieved_ids = extract_retrieved_ids(&response);
        let relevant_set: HashSet<_> = query.relevant.iter().cloned().collect();

        total_recall += calculate_recall(&retrieved_ids, &relevant_set, 10);
        total_rr += calculate_reciprocal_rank(&retrieved_ids, &relevant_set);
        total_precision += calculate_precision(&retrieved_ids, &relevant_set, 10);
        evaluated += 1;
    }

    if evaluated > 0 {
        QualityMetrics {
            recall_at_10: total_recall / evaluated as f64,
            mrr: total_rr / evaluated as f64,
            precision_at_10: total_precision / evaluated as f64,
            query_count: evaluated,
        }
    } else {
        QualityMetrics::default()
    }
}

fn extract_retrieved_ids(response: &Value) -> Vec<String> {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let parsed: Value = serde_json::from_str(text).unwrap_or_default();

    parsed
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("tags")
                        .and_then(|t| t.as_array())
                        .and_then(|tags| tags.first())
                        .and_then(|tag| tag.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn calculate_recall(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }

    let retrieved_k: HashSet<_> = retrieved.iter().take(k).cloned().collect();
    let hits = relevant.intersection(&retrieved_k).count();

    hits as f64 / relevant.len() as f64
}

fn calculate_reciprocal_rank(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    for (i, doc_id) in retrieved.iter().enumerate() {
        if relevant.contains(doc_id) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

fn calculate_precision(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    let retrieved_k: HashSet<_> = retrieved.iter().take(k).cloned().collect();
    if retrieved_k.is_empty() {
        return 0.0;
    }

    let hits = relevant.intersection(&retrieved_k).count();
    hits as f64 / retrieved_k.len() as f64
}

fn calculate_percentiles(latencies: &[f64]) -> PerformanceMetrics {
    if latencies.is_empty() {
        return PerformanceMetrics::default();
    }

    let mut sorted = latencies.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let len = sorted.len();
    let p50_idx = (len as f64 * 0.50) as usize;
    let p90_idx = (len as f64 * 0.90) as usize;
    let p99_idx = (len as f64 * 0.99) as usize;

    let sum: f64 = sorted.iter().sum();

    PerformanceMetrics {
        p50_ms: sorted.get(p50_idx).copied().unwrap_or(0.0),
        p90_ms: sorted.get(p90_idx).copied().unwrap_or(0.0),
        p99_ms: sorted.get(p99_idx.min(len - 1)).copied().unwrap_or(0.0),
        mean_ms: sum / len as f64,
        query_count: len,
    }
}
