use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

use super::math::{
    calculate_precision, calculate_recall, calculate_reciprocal_rank, evaluate_quality_gate,
    summarize, summarize_cross_corpus,
};
use super::types::{
    BenchmarkConfig, BenchmarkReport, CrossCorpusReport, Dataset, DatasetBenchmarkResult, Query,
    QueryMetrics, Thresholds,
};

pub fn run_benchmark_protocol(
    memd_path: &PathBuf,
    embedding_model: &str,
    config: BenchmarkConfig,
) -> Vec<TestResult> {
    let mut results = Vec::new();
    if config.dataset_paths.is_empty() {
        results.push(TestResult::fail(
            "P6_config",
            "at least one --dataset-path is required for --suite benchmark",
        ));
        return results;
    }

    println!("\n=== Offline Retrieval Benchmark Protocol ===");
    println!("Datasets requested: {}", config.dataset_paths.len());
    println!(
        "Bootstrap iterations: {}, seed: {}",
        config.bootstrap_iterations, config.seed
    );
    if let Some(limit) = config.max_queries {
        println!("Max queries per dataset: {limit}");
    }
    if let Some(limit) = config.max_documents {
        println!("Max documents per dataset: {limit}");
    }

    let multi_dataset = config.dataset_paths.len() > 1;
    let mut dataset_reports = Vec::with_capacity(config.dataset_paths.len());
    for dataset_path in &config.dataset_paths {
        let label = dataset_label(dataset_path);
        let (dataset_results, report) = run_single_dataset(
            memd_path,
            embedding_model,
            dataset_path,
            &label,
            multi_dataset,
            &config,
        );
        results.extend(dataset_results);
        if let Some(report) = report {
            dataset_reports.push(report);
        }
    }

    let all_datasets_reported = dataset_reports.len() == config.dataset_paths.len();
    let mut cross_report = None;
    if config.dataset_paths.len() > 1 {
        if all_datasets_reported {
            let report = build_cross_corpus_report(&dataset_reports, embedding_model, &config);
            println!(
                "Normalized cross-corpus: Recall@10 {:.3} [{:.3}, {:.3}] | MRR {:.3} [{:.3}, {:.3}] | P@10 {:.3} [{:.3}, {:.3}]",
                report.normalized_summary.recall.mean,
                report.normalized_summary.recall.ci_lower,
                report.normalized_summary.recall.ci_upper,
                report.normalized_summary.mrr.mean,
                report.normalized_summary.mrr.ci_lower,
                report.normalized_summary.mrr.ci_upper,
                report.normalized_summary.precision.mean,
                report.normalized_summary.precision.ci_lower,
                report.normalized_summary.precision.ci_upper,
            );
            let gate_name = "P6_quality_gate[normalized_cross_corpus]";
            if report.quality_gate_passed {
                results.push(TestResult::pass(gate_name));
            } else {
                results.push(TestResult::fail(gate_name, &report.quality_gate_message));
            }
            cross_report = Some(report);
        } else {
            results.push(TestResult::fail(
                "P6_cross_corpus_report",
                &format!(
                    "cross-corpus summary skipped: only {}/{} datasets completed",
                    dataset_reports.len(),
                    config.dataset_paths.len()
                ),
            ));
        }
    }

    if let Some(report_path) = &config.report_json {
        let write_result = if config.dataset_paths.len() == 1 {
            dataset_reports
                .first()
                .ok_or_else(|| "no successful dataset run to report".to_string())
                .and_then(|report| write_report(report_path, report))
        } else if let Some(report) = &cross_report {
            write_report(report_path, report)
        } else {
            Err("cross-corpus report unavailable due to earlier failures".to_string())
        };

        if let Err(err) = write_result {
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

fn run_single_dataset(
    memd_path: &PathBuf,
    embedding_model: &str,
    dataset_path: &PathBuf,
    dataset_label: &str,
    multi_dataset: bool,
    config: &BenchmarkConfig,
) -> (Vec<TestResult>, Option<BenchmarkReport>) {
    let mut results = Vec::new();

    let load_name = stage_name("P6_load_dataset", dataset_label, multi_dataset);
    let run_name = stage_name("P6_run_benchmark", dataset_label, multi_dataset);
    let gate_name = stage_name("P6_quality_gate", dataset_label, multi_dataset);

    let load_start = Instant::now();
    let mut dataset = match load_dataset(dataset_path) {
        Ok(dataset) => dataset,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                &load_name,
                &format!("Failed to load dataset: {err}"),
                load_start,
            ));
            return (results, None);
        }
    };
    apply_limits(&mut dataset, config.max_documents, config.max_queries);
    results.push(TestResult::pass_with_duration(&load_name, load_start));

    println!("\nDataset: {} (v{})", dataset.description, dataset.version);
    println!("Path: {}", dataset_path.display());
    if let Some(note) = &dataset.note {
        println!("Note: {note}");
    }
    println!(
        "Documents indexed: {}, queries evaluated: {}",
        dataset.documents.len(),
        dataset.queries.len()
    );

    let run_start = Instant::now();
    let data_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                &run_name,
                &format!("Failed to create temp dir: {err}"),
                run_start,
            ));
            return (results, None);
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
                &run_name,
                &format!("Failed to start memd: {err}"),
                run_start,
            ));
            return (results, None);
        }
    };

    if let Err(err) = client.initialize() {
        results.push(TestResult::fail_with_duration(
            &run_name,
            &format!("Failed to initialize MCP client: {err}"),
            run_start,
        ));
        return (results, None);
    }

    if let Err(err) = index_documents(&mut client, &dataset.documents) {
        results.push(TestResult::fail_with_duration(
            &run_name,
            &format!("Failed during document indexing: {err}"),
            run_start,
        ));
        return (results, None);
    }

    let query_metrics = match evaluate_queries(&mut client, &dataset.queries) {
        Ok(metrics) => metrics,
        Err(err) => {
            results.push(TestResult::fail_with_duration(
                &run_name,
                &format!("Failed during query evaluation: {err}"),
                run_start,
            ));
            return (results, None);
        }
    };
    results.push(TestResult::pass_with_duration(&run_name, run_start));

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

    let quality_gate = evaluate_quality_gate(&summary, config);
    if quality_gate.0 {
        results.push(TestResult::pass(&gate_name));
    } else {
        results.push(TestResult::fail(&gate_name, &quality_gate.1));
    }

    let report = BenchmarkReport {
        generated_unix_seconds: now_unix_seconds(),
        dataset_path: dataset_path.display().to_string(),
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

    (results, Some(report))
}

fn build_cross_corpus_report(
    reports: &[BenchmarkReport],
    embedding_model: &str,
    config: &BenchmarkConfig,
) -> CrossCorpusReport {
    let datasets: Vec<DatasetBenchmarkResult> = reports
        .iter()
        .map(|report| DatasetBenchmarkResult {
            dataset_path: report.dataset_path.clone(),
            dataset_description: report.dataset_description.clone(),
            dataset_version: report.dataset_version.clone(),
            queries_evaluated: report.queries_evaluated,
            documents_indexed: report.documents_indexed,
            summary: report.summary.clone(),
            quality_gate_passed: report.quality_gate_passed,
            quality_gate_message: report.quality_gate_message.clone(),
        })
        .collect();

    let normalized_summary =
        summarize_cross_corpus(&datasets, config.bootstrap_iterations, config.seed);
    let normalized_gate = evaluate_quality_gate(&normalized_summary, config);
    let (quality_gate_passed, quality_gate_message) =
        evaluate_cross_corpus_quality_gate(&datasets, normalized_gate);

    CrossCorpusReport {
        generated_unix_seconds: now_unix_seconds(),
        embedding_model: embedding_model.to_string(),
        bootstrap_iterations: config.bootstrap_iterations,
        seed: config.seed,
        max_queries: config.max_queries,
        max_documents: config.max_documents,
        normalization: "macro_average_by_dataset".to_string(),
        thresholds: Thresholds {
            recall: config.threshold_recall,
            mrr: config.threshold_mrr,
            precision: config.threshold_precision,
        },
        datasets,
        normalized_summary,
        quality_gate_passed,
        quality_gate_message,
    }
}

fn evaluate_cross_corpus_quality_gate(
    datasets: &[DatasetBenchmarkResult],
    normalized_gate: (bool, String),
) -> (bool, String) {
    let mut failures = Vec::new();
    if !normalized_gate.0 {
        failures.push(format!("normalized summary: {}", normalized_gate.1));
    }

    for dataset in datasets {
        if !dataset.quality_gate_passed {
            failures.push(format!(
                "{}: {}",
                dataset.dataset_description, dataset.quality_gate_message
            ));
        }
    }

    if failures.is_empty() {
        (
            true,
            "All dataset-level and normalized thresholds satisfied".to_string(),
        )
    } else {
        (false, failures.join("; "))
    }
}

fn load_dataset(path: &Path) -> Result<Dataset, String> {
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

fn write_report<T: Serialize>(path: &PathBuf, report: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create report dir: {err}"))?;
    }
    let content =
        serde_json::to_string_pretty(report).map_err(|err| format!("serialize report: {err}"))?;
    fs::write(path, content).map_err(|err| format!("write report file: {err}"))?;
    Ok(())
}

fn dataset_label(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn stage_name(base: &str, dataset_label: &str, multi_dataset: bool) -> String {
    if multi_dataset {
        format!("{base}[{dataset_label}]")
    } else {
        base.to_string()
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
