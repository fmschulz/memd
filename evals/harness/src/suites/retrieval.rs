//! Retrieval quality evaluation (Suite B)
//!
//! Measures Recall@k, MRR, and other retrieval metrics.
//! Phase 3: Uses handcrafted code samples with known similar pairs.
//! Phase 4+: Expand with benchmark datasets (RepoBench-R, LongMemEval).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure
#[derive(Debug, Deserialize)]
struct Dataset {
    description: String,
    version: String,
    #[serde(default)]
    note: Option<String>,
    queries: Vec<Query>,
    documents: Vec<Document>,
}

#[derive(Debug, Deserialize)]
struct Query {
    id: String,
    query: String,
    relevant: Vec<String>,
    #[allow(dead_code)]
    irrelevant: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Document {
    id: String,
    text: String,
    #[serde(rename = "type")]
    doc_type: String,
}

/// Retrieval metrics
#[derive(Debug, Clone, Default)]
pub struct RetrievalMetrics {
    pub recall_at_10: f64,
    pub mrr: f64,
    pub precision_at_10: f64,
    pub queries_evaluated: usize,
}

impl std::fmt::Display for RetrievalMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Recall@10: {:.3}, MRR: {:.3}, Precision@10: {:.3} (n={})",
            self.recall_at_10, self.mrr, self.precision_at_10, self.queries_evaluated
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

/// Run retrieval quality tests
pub fn run_retrieval_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let dataset_path = crate::resolve_dataset_path("evals/datasets/retrieval/code_pairs.json");
    let dataset = match load_dataset(dataset_path.as_path()) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "B_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== Retrieval Quality Suite (Suite B) ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {}", note);
    }
    println!(
        "Queries: {}, Documents: {}\n",
        dataset.queries.len(),
        dataset.documents.len()
    );

    // B1: Index all documents
    results.push(run_b1_index_documents(memd_path, &dataset, embedding_model));

    // B2: Evaluate retrieval quality
    let (b2_result, metrics) = run_b2_retrieval_quality(memd_path, &dataset, embedding_model);
    results.push(b2_result);

    // B3: Check quality thresholds
    results.push(check_quality_thresholds(&metrics));

    results
}

fn load_dataset(path: &std::path::Path) -> Result<Dataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    let mut dataset: Dataset =
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

fn run_b1_index_documents(
    memd_path: &PathBuf,
    dataset: &Dataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "B1_index_documents";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start)
        }
    };

    // Initialize
    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Regression guard: ensure tool-call RPC errors are surfaced as Err.
    if let Err(e) = verify_tool_error_propagation(&mut client, "eval_retrieval") {
        return TestResult::fail_with_duration(
            name,
            &format!("error propagation guard: {}", e),
            start,
        );
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_retrieval",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]  // Use tags to track document ID
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return TestResult::fail_with_duration(
                name,
                &format!("add doc {}: {}", doc.id, e),
                start,
            );
        }
    }

    println!("  Indexed {} documents", dataset.documents.len());
    TestResult::pass_with_duration(name, start)
}

fn run_b2_retrieval_quality(
    memd_path: &PathBuf,
    dataset: &Dataset,
    embedding_model: &str,
) -> (TestResult, RetrievalMetrics) {
    let start = Instant::now();
    let name = "B2_retrieval_quality";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
                RetrievalMetrics::default(),
            )
        }
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
                RetrievalMetrics::default(),
            )
        }
    };

    if let Err(e) = client.initialize() {
        return (
            TestResult::fail_with_duration(name, &format!("initialize: {}", e), start),
            RetrievalMetrics::default(),
        );
    }

    if let Err(e) = verify_tool_error_propagation(&mut client, "eval_retrieval") {
        return (
            TestResult::fail_with_duration(name, &format!("error propagation guard: {}", e), start),
            RetrievalMetrics::default(),
        );
    }

    // Index all documents first
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_retrieval",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return (
                TestResult::fail_with_duration(name, &format!("add doc {}: {}", doc.id, e), start),
                RetrievalMetrics::default(),
            );
        }
    }

    let mut total_recall = 0.0;
    let mut total_rr = 0.0; // Reciprocal rank
    let mut total_precision = 0.0;
    let mut queries_evaluated = 0;

    for query in &dataset.queries {
        let params = serde_json::json!({
            "tenant_id": "eval_retrieval",
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

        // Extract results from MCP response
        let retrieved_ids = extract_retrieved_ids(&response);
        let relevant_set: HashSet<_> = query.relevant.iter().cloned().collect();

        // Calculate metrics
        let recall = calculate_recall(&retrieved_ids, &relevant_set, 10);
        let rr = calculate_reciprocal_rank(&retrieved_ids, &relevant_set);
        let precision = calculate_precision(&retrieved_ids, &relevant_set, 10);

        total_recall += recall;
        total_rr += rr;
        total_precision += precision;
        queries_evaluated += 1;
    }

    let metrics = if queries_evaluated > 0 {
        RetrievalMetrics {
            recall_at_10: total_recall / queries_evaluated as f64,
            mrr: total_rr / queries_evaluated as f64,
            precision_at_10: total_precision / queries_evaluated as f64,
            queries_evaluated,
        }
    } else {
        RetrievalMetrics::default()
    };

    // Print metrics
    println!("\n=== Retrieval Quality Metrics ===");
    println!("{}", metrics);
    println!("================================\n");

    (TestResult::pass_with_duration(name, start), metrics)
}

/// Regression helper: invalid tool calls must return Err, not a successful response.
fn verify_tool_error_propagation(client: &mut McpClient, tenant_id: &str) -> Result<(), String> {
    let params = serde_json::json!({
        "tenant_id": tenant_id,
        "text": "error propagation regression probe",
        "type": "invalid_type_xyz"
    });

    match client.call_tool("memory.add", params) {
        Ok(_) => Err("memory.add unexpectedly succeeded for invalid type".to_string()),
        Err(_) => Ok(()),
    }
}

fn extract_retrieved_ids(response: &Value) -> Vec<String> {
    // Parse MCP response structure
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
                    // Get document ID from tags
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

fn check_quality_thresholds(metrics: &RetrievalMetrics) -> TestResult {
    let name = "B3_quality_thresholds";
    let start = Instant::now();

    let mut failures = Vec::new();

    // Target: Recall@10 > 0.8
    if metrics.recall_at_10 < 0.8 {
        failures.push(format!(
            "Recall@10 {:.3} below threshold 0.8",
            metrics.recall_at_10
        ));
    }

    // Target: MRR > 0.6
    if metrics.mrr < 0.6 {
        failures.push(format!("MRR {:.3} below threshold 0.6", metrics.mrr));
    }

    println!("Quality thresholds: Recall@10 > 0.8, MRR > 0.6");
    if failures.is_empty() {
        println!("  All thresholds met!");
    } else {
        println!("  Failures: {}", failures.join("; "));
    }

    if failures.is_empty() {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(name, &failures.join("; "), start)
    }
}
