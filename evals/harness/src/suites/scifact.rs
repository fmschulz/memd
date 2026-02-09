//! BEIR SciFact evaluation suite
//!
//! Tests scientific claim retrieval using the SciFact dataset from BEIR benchmark.
//! This suite evaluates semantic retrieval quality on domain-specific scientific text.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure for SciFact tests
#[derive(Debug, Deserialize)]
struct SciFactDataset {
    description: String,
    version: String,
    #[serde(default)]
    note: Option<String>,
    query_types: Vec<String>,
    queries: Vec<SciFactQuery>,
    documents: Vec<SciFactDocument>,
}

#[derive(Debug, Deserialize, Clone)]
struct SciFactQuery {
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
struct SciFactDocument {
    id: String,
    #[serde(alias = "content")]
    text: String,
    #[serde(rename = "type", default = "default_doc_type")]
    doc_type: String,
}

fn default_doc_type() -> String {
    "scientific".to_string()
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

/// Run SciFact evaluation tests
pub fn run_scifact_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let dataset_path =
        crate::resolve_dataset_path("evals/datasets/retrieval/beir_scifact_fixed.json");
    let dataset = match load_dataset(dataset_path.as_path()) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "SciFact_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== SciFact Retrieval Suite ===");
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

fn load_dataset(path: &std::path::Path) -> Result<SciFactDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    let mut dataset: SciFactDataset =
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
    dataset: &SciFactDataset,
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
            "tenant_id": "eval_scifact",
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
    dataset: &SciFactDataset,
    embedding_model: &str,
) -> (TestResult, Option<QualityMetrics>) {
    let start = Instant::now();
    let name = "SciFact_index_and_evaluate";

    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return (TestResult::fail_with_duration(name, &e, start), None);
        }
    };

    println!("  Indexed {} documents", dataset.documents.len());

    // Run a sanity check with exact text query
    eprintln!("\n[SciFact Diagnostic] Running sanity check with exact document text...");
    let first_doc = &dataset.documents[0];
    let sanity_params = serde_json::json!({
        "tenant_id": "eval_scifact",
        "query": &first_doc.text[..first_doc.text.len().min(100)],
        "k": 5
    });

    match client.call_tool("memory.search", sanity_params) {
        Ok(response) => {
            let ids = extract_retrieved_ids(&response);
            eprintln!(
                "[SciFact Diagnostic] Sanity check returned {} results",
                ids.len()
            );
            if ids.is_empty() {
                eprintln!("[SciFact Diagnostic] WARNING: Exact text query returned 0 results!");
                eprintln!(
                    "[SciFact Diagnostic] This likely indicates embeddings failed to initialize."
                );
                eprintln!(
                    "[SciFact Diagnostic] Check that --embedding-model flag is passed correctly."
                );
            } else if ids.contains(&first_doc.id) {
                eprintln!("[SciFact Diagnostic] Sanity check PASSED: Found exact document");
            } else {
                eprintln!("[SciFact Diagnostic] WARNING: Exact text query didn't find the source document");
                eprintln!("[SciFact Diagnostic] Retrieved IDs: {:?}", ids);
            }
        }
        Err(e) => {
            eprintln!("[SciFact Diagnostic] Sanity check query failed: {}", e);
        }
    }

    // Sample queries to keep evaluation time reasonable (50 out of 300)
    let step = std::cmp::max(dataset.queries.len() / 50, 1);
    let sampled_queries: Vec<_> = dataset.queries.iter().step_by(step).take(50).collect();

    println!(
        "  Evaluating {} sampled queries (of {})",
        sampled_queries.len(),
        dataset.queries.len()
    );

    // Evaluate sampled queries
    let metrics = evaluate_queries(&mut client, &sampled_queries);

    println!("\n  Overall: {}", metrics);

    (TestResult::pass_with_duration(name, start), Some(metrics))
}

fn check_quality_threshold(metrics: &QualityMetrics) -> TestResult {
    let start = Instant::now();
    let name = "SciFact_quality_threshold";

    // Target: 0.4 recall for semantic queries on scientific text (harder than general domain)
    if metrics.recall_at_10 >= 0.4 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("Recall@10 {:.3} below threshold 0.4", metrics.recall_at_10),
            start,
        )
    }
}

fn run_performance_baseline(
    memd_path: &PathBuf,
    dataset: &SciFactDataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "SciFact_performance_baseline";

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
                "tenant_id": "eval_scifact",
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

fn evaluate_queries(client: &mut McpClient, queries: &[&SciFactQuery]) -> QualityMetrics {
    let mut total_recall = 0.0;
    let mut total_rr = 0.0;
    let mut total_precision = 0.0;
    let mut evaluated = 0;
    let mut zero_results_count = 0;

    eprintln!(
        "\n[SciFact Diagnostic] Starting evaluation of {} queries",
        queries.len()
    );

    for (idx, query) in queries.iter().enumerate() {
        let params = serde_json::json!({
            "tenant_id": "eval_scifact",
            "query": query.query,
            "k": 10
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[SciFact Diagnostic] Query {} failed: {}", query.id, e);
                continue;
            }
        };

        let retrieved_ids = extract_retrieved_ids(&response);
        let relevant_set: HashSet<_> = query.relevant.iter().cloned().collect();

        // Diagnostic logging for first 3 queries
        if idx < 3 {
            eprintln!(
                "[SciFact Diagnostic] Query {}: \"{}\"",
                query.id,
                &query.query[..query.query.len().min(60)]
            );
            eprintln!(
                "[SciFact Diagnostic]   Expected {} relevant docs: {:?}",
                relevant_set.len(),
                relevant_set
            );
            eprintln!(
                "[SciFact Diagnostic]   Retrieved {} docs: {:?}",
                retrieved_ids.len(),
                if retrieved_ids.is_empty() {
                    "NONE (0 results)".to_string()
                } else {
                    format!("{:?}", &retrieved_ids[..retrieved_ids.len().min(3)])
                }
            );
        }

        if retrieved_ids.is_empty() {
            zero_results_count += 1;
        }

        let recall = calculate_recall(&retrieved_ids, &relevant_set, 10);
        total_recall += recall;
        total_rr += calculate_reciprocal_rank(&retrieved_ids, &relevant_set);
        total_precision += calculate_precision(&retrieved_ids, &relevant_set, 10);
        evaluated += 1;
    }

    eprintln!("[SciFact Diagnostic] Evaluation complete:");
    eprintln!("[SciFact Diagnostic]   {} queries evaluated", evaluated);
    eprintln!(
        "[SciFact Diagnostic]   {} queries returned 0 results",
        zero_results_count
    );
    if evaluated > 0 {
        eprintln!(
            "[SciFact Diagnostic]   Average Recall@10: {:.3}",
            total_recall / evaluated as f64
        );
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
