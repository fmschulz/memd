//! Sanity check evaluation suite
//!
//! Tests evaluation harness with trivial exact-match queries.
//! All queries should achieve 1.000 Recall@10. If not, harness has bugs.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

#[derive(Debug, Deserialize)]
struct SanityDataset {
    description: String,
    version: String,
    #[serde(default)]
    note: Option<String>,
    query_types: Vec<String>,
    queries: Vec<SanityQuery>,
    documents: Vec<SanityDocument>,
}

#[derive(Debug, Deserialize, Clone)]
struct SanityQuery {
    id: String,
    query: String,
    #[serde(rename = "type")]
    query_type: String,
    relevant: Vec<String>,
    irrelevant: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SanityDocument {
    id: String,
    text: String,
    #[serde(rename = "type")]
    doc_type: String,
}

fn extract_content_text(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
}

pub fn run_sanity_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    let dataset_path = crate::resolve_dataset_path("evals/datasets/retrieval/sanity_check.json");
    let dataset = match load_dataset(dataset_path.as_path()) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "Sanity_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== Sanity Check Suite ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {}", note);
    }
    println!(
        "Queries: {}, Documents: {}\n",
        dataset.queries.len(),
        dataset.documents.len()
    );

    // Run the sanity check
    let test_result = run_exact_match_test(memd_path, &dataset, embedding_model);
    results.push(test_result);

    results
}

fn load_dataset(path: &std::path::Path) -> Result<SanityDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    let mut dataset: SanityDataset =
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

fn run_exact_match_test(
    memd_path: &PathBuf,
    dataset: &SanityDataset,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "Sanity_exact_match";

    // Create client and index documents
    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start);
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
            return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start);
        }
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_sanity",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
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

    // Evaluate all queries
    let mut total_recall = 0.0;
    let mut failed_queries = Vec::new();

    for query in &dataset.queries {
        let params = serde_json::json!({
            "tenant_id": "eval_sanity",
            "query": query.query,
            "k": 10
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(e) => {
                return TestResult::fail_with_duration(
                    name,
                    &format!("query {} failed: {}", query.id, e),
                    start,
                );
            }
        };

        let retrieved_ids = extract_retrieved_ids(&response);
        let relevant_set: HashSet<_> = query.relevant.iter().cloned().collect();

        let recall = calculate_recall(&retrieved_ids, &relevant_set);
        total_recall += recall;

        if recall < 1.0 {
            failed_queries.push(format!(
                "{} (recall: {:.3}, expected: {}, got: {:?})",
                query.id, recall, query.relevant[0], retrieved_ids
            ));
        }
    }

    let avg_recall = total_recall / dataset.queries.len() as f64;

    println!("  Recall@10: {:.3} (expected: 1.000)", avg_recall);

    // Sanity check MUST achieve perfect recall
    if avg_recall >= 1.0 {
        println!("  ✓ Sanity check PASSED");
        TestResult::pass_with_duration(name, start)
    } else {
        let message = format!(
            "Sanity check FAILED: Recall {:.3} < 1.000. Failed queries: {}",
            avg_recall,
            failed_queries.join(", ")
        );
        println!("  ✗ {}", message);
        eprintln!("\nERROR: Sanity check failure indicates evaluation harness bug!");
        eprintln!("Exact text queries should always find their source documents.");
        TestResult::fail_with_duration(name, &message, start)
    }
}

fn extract_retrieved_ids(response: &Value) -> Vec<String> {
    // Debug: print the actual response structure
    eprintln!(
        "[DEBUG] Full response: {}",
        serde_json::to_string_pretty(response).unwrap_or_default()
    );

    let text = match extract_content_text(response) {
        Some(t) => t,
        None => {
            eprintln!("[DEBUG] Failed to extract content text");
            return Vec::new();
        }
    };

    eprintln!("[DEBUG] Extracted text: {}", text);
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

fn calculate_recall(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }

    let retrieved_set: HashSet<_> = retrieved.iter().take(10).cloned().collect();
    let hits = relevant.intersection(&retrieved_set).count();

    hits as f64 / relevant.len() as f64
}
