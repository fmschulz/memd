//! True Semantic Retrieval Evaluation Suite
//!
//! Tests pure semantic understanding with minimal keyword overlap (6.1%).
//! This suite isolates embedding model quality by using queries where BM25 cannot help.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::suites::hybrid::{load_dataset_generic, HybridDataset, HybridQuery};
use crate::TestResult;

fn extract_content_text(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
}

pub fn run_true_semantic_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    let dataset_path =
        crate::resolve_dataset_path("evals/datasets/retrieval/true_semantic_test.json");
    let dataset = match load_dataset_generic(dataset_path.as_path()) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "TrueSemantic_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== True Semantic Retrieval Suite ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {}", note);
    }
    println!(
        "Queries: {}, Documents: {}",
        dataset.queries.len(),
        dataset.documents.len()
    );
    println!("Average keyword overlap: 6.1% (tests PURE semantic understanding)\n");

    let (test_result, recall) = run_index_and_evaluate(memd_path, &dataset, embedding_model);
    results.push(test_result);

    if let Some(r) = recall {
        results.push(check_quality_threshold(r));
    }

    results
}

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

    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_true_semantic",
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
    dataset: &HybridDataset,
    embedding_model: &str,
) -> (TestResult, Option<f64>) {
    let start = Instant::now();
    let name = "TrueSemantic_index_and_evaluate";

    let (mut client, _data_dir) = match create_indexed_client(memd_path, dataset, embedding_model) {
        Ok(c) => c,
        Err(e) => {
            return (TestResult::fail_with_duration(name, &e, start), None);
        }
    };

    println!("  Indexed {} documents", dataset.documents.len());

    let (recall, mrr, precision, count) = evaluate_queries(&mut client, &dataset.queries);

    println!(
        "\n  Overall: Recall@10: {:.3}, MRR: {:.3}, P@10: {:.3} (n={})",
        recall, mrr, precision, count
    );

    (TestResult::pass_with_duration(name, start), Some(recall))
}

fn check_quality_threshold(recall: f64) -> TestResult {
    let start = Instant::now();
    let name = "TrueSemantic_quality_threshold";

    // Target: 0.6 recall (harder than hybrid due to low keyword overlap)
    if recall >= 0.6 {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("Recall@10 {:.3} below threshold 0.6", recall),
            start,
        )
    }
}

fn evaluate_queries(client: &mut McpClient, queries: &[HybridQuery]) -> (f64, f64, f64, usize) {
    let mut total_recall = 0.0;
    let mut total_rr = 0.0;
    let mut total_precision = 0.0;
    let mut evaluated = 0;

    for query in queries {
        let params = serde_json::json!({
            "tenant_id": "eval_true_semantic",
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
        (
            total_recall / evaluated as f64,
            total_rr / evaluated as f64,
            total_precision / evaluated as f64,
            evaluated,
        )
    } else {
        (0.0, 0.0, 0.0, 0)
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
