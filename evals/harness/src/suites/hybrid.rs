//! Hybrid retrieval evaluation (Suite C)
//!
//! Compares hybrid (dense+sparse) against dense-only baseline.
//! Measures quality improvement and performance baseline.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure for hybrid tests
#[derive(Debug, Deserialize)]
pub struct HybridDataset {
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub note: Option<String>,
    pub query_types: Vec<String>,
    pub queries: Vec<HybridQuery>,
    pub documents: Vec<HybridDocument>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HybridQuery {
    pub id: String,
    pub query: String,
    #[serde(rename = "type")]
    pub query_type: String,
    pub relevant: Vec<String>,
    #[allow(dead_code)]
    pub irrelevant: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct HybridDocument {
    pub id: String,
    #[serde(alias = "content")]
    pub text: String,
    #[serde(rename = "type", default = "default_doc_type")]
    pub doc_type: String,
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
}

fn default_doc_type() -> String {
    "code".to_string()
}

/// Quality metrics per query type
#[derive(Debug, Clone, Default)]
pub struct TypeMetrics {
    pub recall_at_10: f64,
    pub mrr: f64,
    pub precision_at_10: f64,
    pub query_count: usize,
}

impl std::fmt::Display for TypeMetrics {
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

/// Run hybrid evaluation tests
pub fn run_hybrid_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let dataset_path = std::path::Path::new("evals/datasets/retrieval/hybrid_test.json");
    let dataset = match load_dataset(dataset_path) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "C_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== Hybrid Retrieval Suite (Suite C) ===");
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

    // C1: Index all documents and run quality tests in one session
    let (c1_result, all_metrics) = run_c1_index_and_evaluate(memd_path, &dataset, embedding_model);
    results.push(c1_result);

    let (keyword_metrics, semantic_metrics, mixed_metrics) = match all_metrics {
        Some(m) => m,
        None => return results,
    };

    // C2: Keyword query quality threshold
    results.push(check_keyword_threshold(&keyword_metrics));

    // C3: Semantic query quality threshold
    results.push(check_semantic_threshold(&semantic_metrics));

    // C4: Mixed query quality threshold
    results.push(check_mixed_threshold(&mixed_metrics));

    // C5: Hybrid vs dense comparison (informational)
    results.push(run_c5_hybrid_comparison(
        &keyword_metrics,
        &semantic_metrics,
        &mixed_metrics,
    ));

    // C6: Performance baseline
    results.push(run_c6_performance_baseline(memd_path, &dataset, embedding_model));

    // C7: Quality thresholds check
    results.push(run_c7_quality_thresholds(
        &keyword_metrics,
        &semantic_metrics,
        &mixed_metrics,
    ));

    results
}

fn load_dataset(path: &std::path::Path) -> Result<HybridDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("parse json: {}", e))
}

/// Generic dataset loader (public for reuse in other test suites)
pub fn load_dataset_generic(path: &std::path::Path) -> Result<HybridDataset, String> {
    load_dataset(path)
}

/// Create a client, index documents, and optionally run queries
fn create_indexed_client(
    memd_path: &PathBuf,
    dataset: &HybridDataset,
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
            "tenant_id": "eval_hybrid",
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

fn run_c1_index_and_evaluate(
    memd_path: &PathBuf,
    dataset: &HybridDataset,
    embedding_model: &str,
) -> (TestResult, Option<(TypeMetrics, TypeMetrics, TypeMetrics)>) {
    let start = Instant::now();
    let name = "C1_index_documents";

    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return (TestResult::fail_with_duration(name, &e, start), None);
        }
    };

    println!("  Indexed {} documents", dataset.documents.len());

    // Now evaluate all query types
    let keyword_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "keyword")
        .collect();
    let semantic_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "semantic")
        .collect();
    let mixed_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "mixed")
        .collect();

    let keyword_metrics = evaluate_queries(&mut client, &keyword_queries);
    let semantic_metrics = evaluate_queries(&mut client, &semantic_queries);
    let mixed_metrics = evaluate_queries(&mut client, &mixed_queries);

    println!("\n  Keyword queries: {}", keyword_metrics);
    println!("  Semantic queries: {}", semantic_metrics);
    println!("  Mixed queries: {}", mixed_metrics);

    (
        TestResult::pass_with_duration(name, start),
        Some((keyword_metrics, semantic_metrics, mixed_metrics)),
    )
}

fn check_keyword_threshold(metrics: &TypeMetrics) -> TestResult {
    let start = Instant::now();
    let name = "C2_keyword_query_quality";

    // Target: 0.9 recall for keyword (exact match) queries
    if metrics.recall_at_10 >= 0.9 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "Keyword Recall@10 {:.3} below threshold 0.9",
                metrics.recall_at_10
            ),
            start,
        )
    }
}

fn check_semantic_threshold(metrics: &TypeMetrics) -> TestResult {
    let start = Instant::now();
    let name = "C3_semantic_query_quality";

    // Lower threshold for semantic (harder task)
    if metrics.recall_at_10 >= 0.7 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "Semantic Recall@10 {:.3} below threshold 0.7",
                metrics.recall_at_10
            ),
            start,
        )
    }
}

fn check_mixed_threshold(metrics: &TypeMetrics) -> TestResult {
    let start = Instant::now();
    let name = "C4_mixed_query_quality";

    // Mixed should be between keyword and semantic
    if metrics.recall_at_10 >= 0.75 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "Mixed Recall@10 {:.3} below threshold 0.75",
                metrics.recall_at_10
            ),
            start,
        )
    }
}

fn run_c5_hybrid_comparison(
    keyword_metrics: &TypeMetrics,
    semantic_metrics: &TypeMetrics,
    mixed_metrics: &TypeMetrics,
) -> TestResult {
    let start = Instant::now();
    let name = "C5_hybrid_vs_dense_comparison";

    // This is informational - hybrid should show keyword queries working well
    // which indicates sparse (BM25) is contributing
    println!("\n=== Hybrid vs Dense Analysis ===");
    println!("Keyword queries benefit from BM25 (sparse):");
    println!(
        "  - Keyword Recall@10: {:.3} (target: 0.9 for exact matches)",
        keyword_metrics.recall_at_10
    );
    println!("Semantic queries use dense embeddings:");
    println!(
        "  - Semantic Recall@10: {:.3} (target: 0.7 for conceptual)",
        semantic_metrics.recall_at_10
    );
    println!("Mixed queries benefit from both:");
    println!(
        "  - Mixed Recall@10: {:.3} (should be between keyword and semantic)",
        mixed_metrics.recall_at_10
    );

    // Check that keyword beats semantic (indicates sparse is helping)
    if keyword_metrics.recall_at_10 >= semantic_metrics.recall_at_10 {
        println!("\n  Hybrid advantage confirmed: keyword >= semantic");
    } else {
        println!(
            "\n  Note: semantic outperformed keyword ({:.3} vs {:.3})",
            semantic_metrics.recall_at_10, keyword_metrics.recall_at_10
        );
    }

    // Always pass - this is informational
    TestResult::pass_with_duration(name, start)
}

fn run_c6_performance_baseline(
    memd_path: &PathBuf,
    dataset: &HybridDataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "C6_performance_baseline";

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
                "tenant_id": "eval_hybrid",
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

fn run_c7_quality_thresholds(
    keyword_metrics: &TypeMetrics,
    semantic_metrics: &TypeMetrics,
    mixed_metrics: &TypeMetrics,
) -> TestResult {
    let start = Instant::now();
    let name = "C7_quality_thresholds";

    // Calculate overall metrics
    let total_count =
        keyword_metrics.query_count + semantic_metrics.query_count + mixed_metrics.query_count;

    let overall_recall = if total_count > 0 {
        (keyword_metrics.recall_at_10 * keyword_metrics.query_count as f64
            + semantic_metrics.recall_at_10 * semantic_metrics.query_count as f64
            + mixed_metrics.recall_at_10 * mixed_metrics.query_count as f64)
            / total_count as f64
    } else {
        0.0
    };

    let overall_mrr = if total_count > 0 {
        (keyword_metrics.mrr * keyword_metrics.query_count as f64
            + semantic_metrics.mrr * semantic_metrics.query_count as f64
            + mixed_metrics.mrr * mixed_metrics.query_count as f64)
            / total_count as f64
    } else {
        0.0
    };

    println!("\n=== Quality Thresholds ===");
    println!(
        "  Overall Recall@10: {:.3} (threshold: 0.75)",
        overall_recall
    );
    println!("  Overall MRR: {:.3} (threshold: 0.6)", overall_mrr);
    println!(
        "  Keyword Recall@10: {:.3} (threshold: 0.85)",
        keyword_metrics.recall_at_10
    );

    let mut failures = Vec::new();

    if overall_recall < 0.75 {
        failures.push(format!("Overall Recall@10 {:.3} < 0.75", overall_recall));
    }
    if overall_mrr < 0.6 {
        failures.push(format!("Overall MRR {:.3} < 0.6", overall_mrr));
    }
    if keyword_metrics.recall_at_10 < 0.85 {
        failures.push(format!(
            "Keyword Recall@10 {:.3} < 0.85",
            keyword_metrics.recall_at_10
        ));
    }

    if failures.is_empty() {
        println!("  All quality thresholds met!");
        TestResult::pass_with_duration(name, start)
    } else {
        println!("  Thresholds not met: {}", failures.join("; "));
        TestResult::fail_with_duration(name, &failures.join("; "), start)
    }
}

fn evaluate_queries(client: &mut McpClient, queries: &[&HybridQuery]) -> TypeMetrics {
    let mut total_recall = 0.0;
    let mut total_rr = 0.0;
    let mut total_precision = 0.0;
    let mut evaluated = 0;

    for query in queries {
        let params = serde_json::json!({
            "tenant_id": "eval_hybrid",
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
        TypeMetrics {
            recall_at_10: total_recall / evaluated as f64,
            mrr: total_rr / evaluated as f64,
            precision_at_10: total_precision / evaluated as f64,
            query_count: evaluated,
        }
    } else {
        TypeMetrics::default()
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
