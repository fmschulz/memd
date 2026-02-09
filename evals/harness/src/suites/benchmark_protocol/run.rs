use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

use super::math::{
    calculate_precision, calculate_recall, calculate_reciprocal_rank, evaluate_quality_gate,
    summarize,
};
use super::types::{BenchmarkConfig, BenchmarkReport, Dataset, Query, QueryMetrics, Thresholds};

pub fn run_benchmark_protocol(
    memd_path: &PathBuf,
    embedding_model: &str,
    config: BenchmarkConfig,
) -> Vec<TestResult> {
    let mut results = Vec::new();
    let load_start = Instant::now();
    let mut dataset = match load_dataset(&config.dataset_path) {
        Ok(dataset) => dataset,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                "P6_load_dataset",
                &format!("Failed to load dataset: {err}"),
                load_start,
            ));
            return results;
        }
    };
    apply_limits(&mut dataset, config.max_documents, config.max_queries);
    results.push(TestResult::pass_with_duration(
        "P6_load_dataset",
        load_start,
    ));

    println!("\n=== Offline Retrieval Benchmark Protocol ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {note}");
    }
    println!(
        "Documents indexed: {}, queries evaluated: {}",
        dataset.documents.len(),
        dataset.queries.len()
    );
    println!(
        "Bootstrap iterations: {}, seed: {}\n",
        config.bootstrap_iterations, config.seed
    );

    let run_start = Instant::now();
    let data_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                "P6_run_benchmark",
                &format!("Failed to create temp dir: {err}"),
                run_start,
            ));
            return results;
        }
    };

    let data_dir_arg = data_dir.path().to_string_lossy().to_string();
    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir_arg.as_str(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(client) => client,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                "P6_run_benchmark",
                &format!("Failed to start memd: {err}"),
                run_start,
            ));
            return results;
        }
    };

    if let Err(err) = client.initialize() {
        results.push(TestResult::fail_with_duration(
            "P6_run_benchmark",
            &format!("Failed to initialize MCP client: {err}"),
            run_start,
        ));
        return results;
    }

    if let Err(err) = index_documents(&mut client, &dataset.documents) {
        results.push(TestResult::fail_with_duration(
            "P6_run_benchmark",
            &format!("Failed during document indexing: {err}"),
            run_start,
        ));
        return results;
    }

    let query_metrics = match evaluate_queries(&mut client, &dataset.queries) {
        Ok(metrics) => metrics,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                "P6_run_benchmark",
                &format!("Failed during query evaluation: {err}"),
                run_start,
            ));
            return results;
        }
    };
    results.push(TestResult::pass_with_duration(
        "P6_run_benchmark",
        run_start,
    ));

    let summary = summarize(&query_metrics, config.bootstrap_iterations, config.seed);
    println!(
        "Summary: Recall@10 {:.3} [{:.3}, {:.3}] | MRR {:.3} [{:.3}, {:.3}] | P@10 {:.3} [{:.3}, {:.3}]",
        summary.recall.mean,
        summary.recall.ci_lower,
        summary.recall.ci_upper,
        summary.mrr.mean,
        summary.mrr.ci_lower,
        summary.mrr.ci_upper,
        summary.precision.mean,
        summary.precision.ci_lower,
        summary.precision.ci_upper,
    );

    let quality_gate = evaluate_quality_gate(&summary, &config);
    let gate_start = Instant::now();
    if quality_gate.0 {
        results.push(TestResult::pass_with_duration(
            "P6_quality_gate",
            gate_start,
        ));
    } else {
        results.push(TestResult::fail_with_duration(
            "P6_quality_gate",
            &quality_gate.1,
            gate_start,
        ));
    }

    if let Some(report_path) = &config.report_json {
        let report = BenchmarkReport {
            generated_unix_seconds: now_unix_seconds(),
            dataset_path: config.dataset_path.display().to_string(),
            dataset_description: dataset.description,
            dataset_version: dataset.version,
            embedding_model: embedding_model.to_string(),
            bootstrap_iterations: config.bootstrap_iterations,
            seed: config.seed,
            queries_evaluated: query_metrics.len(),
            documents_indexed: dataset.documents.len(),
            thresholds: Thresholds {
                recall: config.threshold_recall,
                mrr: config.threshold_mrr,
                precision: config.threshold_precision,
            },
            summary,
            quality_gate_passed: quality_gate.0,
            quality_gate_message: quality_gate.1,
            query_metrics,
        };
        if let Err(err) = write_report(report_path, &report) {
            results.push(TestResult::fail(
                "P6_report_write",
                &format!("Failed to write report JSON: {err}"),
            ));
        } else {
            results.push(TestResult::pass("P6_report_write"));
        }
    }

    results
}

fn load_dataset(path: &PathBuf) -> Result<Dataset, String> {
    let content = fs::read_to_string(path).map_err(|err| format!("read file: {err}"))?;
    let mut dataset: Dataset =
        serde_json::from_str(&content).map_err(|err| format!("parse json: {err}"))?;
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

fn apply_limits(dataset: &mut Dataset, max_documents: Option<usize>, max_queries: Option<usize>) {
    if let Some(limit) = max_documents {
        dataset.documents.truncate(limit);
    }
    if let Some(limit) = max_queries {
        dataset.queries.truncate(limit);
    }
}

fn index_documents(
    client: &mut McpClient,
    documents: &[super::types::Document],
) -> Result<(), String> {
    for doc in documents {
        let params = serde_json::json!({
            "tenant_id": "eval_benchmark_protocol",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });
        client
            .call_tool("memory.add", params)
            .map_err(|err| format!("memory.add for doc {} failed: {err}", doc.id))?;
    }
    Ok(())
}

fn evaluate_queries(
    client: &mut McpClient,
    queries: &[Query],
) -> Result<Vec<QueryMetrics>, String> {
    let mut metrics = Vec::with_capacity(queries.len());
    for query in queries {
        let query_start = Instant::now();
        let response = client
            .call_tool(
                "memory.search",
                serde_json::json!({
                    "tenant_id": "eval_benchmark_protocol",
                    "query": query.query,
                    "k": 10
                }),
            )
            .map_err(|err| format!("memory.search for query {} failed: {err}", query.id))?;
        let retrieved_ids = extract_retrieved_ids(&response);
        let relevant_set: HashSet<_> = query.relevant.iter().cloned().collect();
        metrics.push(QueryMetrics {
            query_id: query.id.clone(),
            recall_at_10: calculate_recall(&retrieved_ids, &relevant_set),
            mrr: calculate_reciprocal_rank(&retrieved_ids, &relevant_set),
            precision_at_10: calculate_precision(&retrieved_ids, &relevant_set),
            latency_ms: query_start.elapsed().as_secs_f64() * 1000.0,
        });
    }
    Ok(metrics)
}

fn extract_retrieved_ids(response: &Value) -> Vec<String> {
    let Some(text) = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
    else {
        return Vec::new();
    };
    let parsed: Value = serde_json::from_str(text).unwrap_or_default();
    parsed
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    result
                        .get("tags")
                        .and_then(Value::as_array)
                        .and_then(|tags| tags.first())
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn write_report(path: &PathBuf, report: &BenchmarkReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create report dir: {err}"))?;
    }
    let content =
        serde_json::to_string_pretty(report).map_err(|err| format!("serialize report: {err}"))?;
    fs::write(path, content).map_err(|err| format!("write report file: {err}"))?;
    Ok(())
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
