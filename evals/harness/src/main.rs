//! memd evaluation harness
//!
//! Runs MCP conformance tests against memd.

use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use memd_evals::suites;
use memd_evals::TestResult;

#[derive(Debug, Deserialize)]
struct DatasetManifest {
    #[serde(default)]
    dataset: Vec<DatasetManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct DatasetManifestEntry {
    path: String,
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    qrels_format: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    approx_bytes: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    license: Option<String>,
}

/// Read a TOML dataset manifest and return the resolved dataset paths.
///
/// Relative `path` entries resolve against the manifest file's own parent
/// directory so `evals/bench/beir_manifest.toml` can ship with stable
/// `datasets/retrieval/...` paths regardless of the operator's CWD.
fn load_dataset_manifest(manifest_path: &Path) -> Result<Vec<PathBuf>, String> {
    let content = fs::read_to_string(manifest_path)
        .map_err(|err| format!("read manifest {}: {err}", manifest_path.display()))?;
    let parsed: DatasetManifest = toml::from_str(&content)
        .map_err(|err| format!("parse manifest {}: {err}", manifest_path.display()))?;
    if parsed.dataset.is_empty() {
        return Err(format!(
            "manifest {} contains no [[dataset]] entries",
            manifest_path.display()
        ));
    }
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(parsed
        .dataset
        .into_iter()
        .map(|entry| {
            let candidate = PathBuf::from(&entry.path);
            if candidate.is_absolute() {
                candidate
            } else {
                manifest_dir.join(candidate)
            }
        })
        .collect())
}

/// memd evaluation harness
#[derive(Parser, Debug)]
#[command(name = "memd-evals", version, about)]
struct Args {
    /// Path to memd binary
    #[arg(long, default_value = "target/debug/memd")]
    memd_path: String,

    /// Suite to run (all, sanity, mcp, persistence, retrieval, hybrid, scifact, true-semantic, nfcorpus, codesearchnet, tiered, structural, compaction, benchmark, benchmark-regression)
    #[arg(long, default_value = "all")]
    suite: String,

    /// Output format (pretty, json)
    #[arg(long, default_value = "pretty")]
    output: String,

    /// Skip build step
    #[arg(long)]
    skip_build: bool,

    /// Embedding model to use (all-minilm, qwen3)
    #[arg(long, default_value = "all-minilm")]
    embedding_model: String,

    /// Include structural tests in 'all' suite
    #[arg(long, default_value = "true")]
    include_structural: bool,

    /// Include compaction tests in 'all' suite (slower, tests compaction correctness)
    #[arg(long, default_value = "false")]
    include_compaction: bool,

    /// Override dataset file path for dataset-backed suites
    ///
    /// Use the flag once for single-suite runs (e.g. `--suite retrieval`) and
    /// repeat it for multi-dataset benchmark runs (`--suite benchmark`).
    #[arg(long = "dataset-path")]
    dataset_path: Vec<String>,

    /// Path to a TOML dataset manifest (see `evals/bench/beir_manifest.toml`).
    ///
    /// Each `[[dataset]]` entry contributes one dataset path. Relative paths
    /// resolve against the manifest file's own directory. Manifest-provided
    /// paths concatenate with any explicit `--dataset-path` values.
    #[arg(long = "dataset-manifest")]
    dataset_manifest: Option<String>,

    /// Bootstrap iterations used by the benchmark protocol suite.
    #[arg(long, default_value_t = 1000)]
    bootstrap_iterations: usize,

    /// Random seed used by the benchmark protocol suite.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Write benchmark protocol report as JSON.
    #[arg(long)]
    report_json: Option<String>,

    /// Optional minimum Recall@10 quality gate (benchmark suite only).
    #[arg(long)]
    threshold_recall: Option<f64>,

    /// Optional minimum MRR quality gate (benchmark suite only).
    #[arg(long)]
    threshold_mrr: Option<f64>,

    /// Optional minimum Precision@10 quality gate (benchmark suite only).
    #[arg(long)]
    threshold_precision: Option<f64>,

    /// Optional max queries to evaluate (benchmark suite only).
    #[arg(long)]
    max_queries: Option<usize>,

    /// Optional max documents to index (benchmark suite only).
    #[arg(long)]
    max_documents: Option<usize>,

    /// Retrieval system variant to run (`hybrid-feature`, `hybrid-cross-encoder`,
    /// `dense-only`, `bm25-only`).
    #[arg(long, default_value = "hybrid-feature")]
    system_variant: String,

    /// Include abstention questions for LongMemEval-formatted datasets.
    ///
    /// Default is false to match LongMemEval retrieval evaluation conventions.
    #[arg(long, default_value_t = false)]
    include_abstention: bool,

    /// Optional cap of haystack sessions retained per query when loading
    /// LongMemEval-formatted datasets.
    #[arg(long)]
    max_sessions_per_query: Option<usize>,

    /// Optional cap of characters retained from each LongMemEval session
    /// document during conversion.
    #[arg(long)]
    max_session_chars: Option<usize>,

    /// Baseline benchmark report JSON for regression gating.
    #[arg(long)]
    baseline_report: Option<String>,

    /// Candidate benchmark report JSON for regression gating.
    #[arg(long)]
    candidate_report: Option<String>,

    /// Significance alpha for regression gating.
    #[arg(long, default_value_t = 0.05)]
    significance_alpha: f64,

    /// Minimum absolute Cohen's d for practical-significance gating.
    #[arg(long, default_value_t = 0.1)]
    min_effect_size: f64,

    /// Optional regression gate report path.
    #[arg(long)]
    regression_report_json: Option<String>,

    /// Metric(s) to gate on for `--suite benchmark-regression`. Accepts
    /// `all` (recall_at_10, mrr, precision_at_10, ndcg_at_10), a named
    /// field (`recall_at_10`, `mrr`, `precision_at_10`), or `ndcg_at_<k>`
    /// (e.g. `ndcg_at_10`, `ndcg_at_100`). Default: `ndcg_at_10`.
    #[arg(long, default_value = "ndcg_at_10")]
    metric: String,
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Merge explicit --dataset-path values with entries parsed from
    // --dataset-manifest. Manifest entries append after explicit paths so
    // CLI-provided overrides take precedence for the first-path env var used
    // by single-dataset suites.
    let mut dataset_paths: Vec<String> = args.dataset_path.clone();
    if let Some(manifest_path) = args.dataset_manifest.as_deref() {
        match load_dataset_manifest(Path::new(manifest_path)) {
            Ok(paths) => {
                dataset_paths.extend(paths.into_iter().map(|p| p.display().to_string()));
            }
            Err(err) => {
                eprintln!("--dataset-manifest error: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    if !dataset_paths.is_empty() && args.suite == "all" {
        eprintln!(
            "--dataset-path / --dataset-manifest is only supported with a specific --suite, not --suite all"
        );
        return ExitCode::FAILURE;
    }

    if dataset_paths.len() > 1
        && !matches!(args.suite.as_str(), "benchmark" | "benchmark-protocol")
    {
        eprintln!(
            "Multiple dataset paths (from --dataset-path / --dataset-manifest) are only supported with --suite benchmark"
        );
        return ExitCode::FAILURE;
    }

    if let Some(path) = dataset_paths.first() {
        std::env::set_var("MEMD_EVAL_DATASET_PATH", path);
    }

    // Build memd first (unless skipped)
    if !args.skip_build {
        println!("Building memd...");
        let status = Command::new("cargo")
            .args(["build", "-p", "memd"])
            .status()
            .expect("Failed to run cargo build");

        if !status.success() {
            eprintln!("Build failed");
            return ExitCode::FAILURE;
        }
        println!("Build complete.\n");
    }

    // Run the specified suite
    let memd_binary = std::path::PathBuf::from(&args.memd_path);
    let embedding_model = &args.embedding_model;
    let results: Vec<TestResult> = match args.suite.as_str() {
        "all" => {
            let mut all = vec![];
            // Run sanity check first - halt if it fails
            let sanity_results = suites::sanity::run_sanity_tests(&memd_binary, embedding_model);
            if sanity_results.iter().any(|r| !r.passed) {
                eprintln!("\n[X] Sanity checks FAILED - halting benchmarks");
                eprintln!("Fix evaluation harness bugs before running full benchmarks.");
                return ExitCode::FAILURE;
            }
            println!("\n[OK] Sanity checks PASSED - proceeding with benchmarks\n");

            all.extend(sanity_results);
            all.extend(suites::mcp_conformance::run(&args.memd_path));
            all.extend(suites::persistence::run_all(&memd_binary));
            all.extend(suites::retrieval::run_retrieval_tests(
                &memd_binary,
                embedding_model,
            ));
            all.extend(suites::hybrid::run_hybrid_tests(
                &memd_binary,
                embedding_model,
            ));
            all.extend(suites::true_semantic::run_true_semantic_tests(
                &memd_binary,
                embedding_model,
            ));
            all.extend(suites::scifact::run_scifact_tests(
                &memd_binary,
                embedding_model,
            ));
            all.extend(suites::codesearchnet::run_codesearchnet_tests(
                &memd_binary,
                embedding_model,
            ));

            // Structural tests (Suite E)
            if args.include_structural {
                all.extend(suites::structural::run_structural_tests(
                    &memd_binary,
                    embedding_model,
                ));
            }

            // Compaction tests (Suite F) - excluded by default (slower)
            if args.include_compaction {
                all.extend(suites::compaction::run_compaction_tests(
                    &memd_binary,
                    embedding_model,
                ));
            }
            all
        }
        "sanity" => suites::sanity::run_sanity_tests(&memd_binary, embedding_model),
        "mcp" => suites::mcp_conformance::run(&args.memd_path),
        "persistence" => suites::persistence::run_all(&memd_binary),
        "retrieval" => suites::retrieval::run_retrieval_tests(&memd_binary, embedding_model),
        "hybrid" => suites::hybrid::run_hybrid_tests(&memd_binary, embedding_model),
        "true-semantic" => {
            suites::true_semantic::run_true_semantic_tests(&memd_binary, embedding_model)
        }
        "scifact" => suites::scifact::run_scifact_tests(&memd_binary, embedding_model),
        "nfcorpus" => suites::nfcorpus::run_nfcorpus_tests(&memd_binary, embedding_model),
        "codesearchnet" => {
            suites::codesearchnet::run_codesearchnet_tests(&memd_binary, embedding_model)
        }
        "tiered" => suites::tiered::run_tiered_tests(&memd_binary, embedding_model),
        "structural" => suites::structural::run_structural_tests(&memd_binary, embedding_model),
        "compaction" | "f" => {
            suites::compaction::run_compaction_tests(&memd_binary, embedding_model)
        }
        "benchmark" | "benchmark-protocol" => {
            if dataset_paths.is_empty() {
                eprintln!(
                    "--dataset-path or --dataset-manifest is required for --suite benchmark"
                );
                return ExitCode::FAILURE;
            }
            let config = suites::benchmark_protocol::BenchmarkConfig {
                dataset_paths: dataset_paths
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect(),
                system_variant: args.system_variant,
                bootstrap_iterations: args.bootstrap_iterations,
                seed: args.seed,
                report_json: args.report_json.as_deref().map(std::path::PathBuf::from),
                threshold_recall: args.threshold_recall,
                threshold_mrr: args.threshold_mrr,
                threshold_precision: args.threshold_precision,
                max_queries: args.max_queries,
                max_documents: args.max_documents,
                include_abstention: args.include_abstention,
                max_sessions_per_query: args.max_sessions_per_query,
                max_session_chars: args.max_session_chars,
            };
            suites::benchmark_protocol::run_benchmark_protocol(
                &memd_binary,
                embedding_model,
                config,
            )
        }
        "benchmark-regression" | "regression-gate" => {
            let Some(baseline_report) = args.baseline_report.as_deref() else {
                eprintln!("--baseline-report is required for --suite benchmark-regression");
                return ExitCode::FAILURE;
            };
            let Some(candidate_report) = args.candidate_report.as_deref() else {
                eprintln!("--candidate-report is required for --suite benchmark-regression");
                return ExitCode::FAILURE;
            };
            let config = suites::benchmark_protocol::RegressionConfig {
                baseline_report: std::path::PathBuf::from(baseline_report),
                candidate_report: std::path::PathBuf::from(candidate_report),
                alpha: args.significance_alpha,
                min_effect_size: args.min_effect_size,
                report_json: args
                    .regression_report_json
                    .as_deref()
                    .map(std::path::PathBuf::from),
                metric: args.metric,
            };
            suites::benchmark_protocol::run_regression_gate(config)
        }
        _ => {
            eprintln!(
                "Unknown suite: {}. Available: all, sanity, mcp, persistence, retrieval, hybrid, true-semantic, scifact, nfcorpus, codesearchnet, tiered, structural, compaction, benchmark, benchmark-regression",
                args.suite
            );
            return ExitCode::FAILURE;
        }
    };

    // Print results
    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();

    println!("\n{}/{} tests passed", passed, total);
    println!("{}", "=".repeat(50));

    for result in &results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        let duration = if result.duration_ms > 0 {
            format!(" ({}ms)", result.duration_ms)
        } else {
            String::new()
        };

        println!("  [{}] {}{}", status, result.name, duration);

        if !result.passed && !result.message.is_empty() {
            println!("       -> {}", result.message);
        }
    }

    if args.output == "json" {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "passed": r.passed,
                    "message": r.message,
                    "duration_ms": r.duration_ms,
                })
            })
            .collect();
        println!("\n{}", serde_json::to_string_pretty(&json_results).unwrap());
    }

    if passed == total {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_dataset_manifest_resolves_relative_paths_against_manifest_parent() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("beir_manifest.toml");
        fs::write(
            &manifest,
            r#"
[[dataset]]
name = "fiqa"
path = "datasets/retrieval/beir_fiqa.json"

[[dataset]]
name = "scidocs"
path = "datasets/retrieval/beir_scidocs.json"
"#,
        )
        .unwrap();

        let paths = load_dataset_manifest(&manifest).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            dir.path().join("datasets/retrieval/beir_fiqa.json")
        );
        assert_eq!(
            paths[1],
            dir.path().join("datasets/retrieval/beir_scidocs.json")
        );
    }

    #[test]
    fn load_dataset_manifest_keeps_absolute_paths_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("m.toml");
        let absolute = "/tmp/definitely/not/relative.json";
        fs::write(
            &manifest,
            format!(
                r#"
[[dataset]]
path = "{absolute}"
"#
            ),
        )
        .unwrap();
        let paths = load_dataset_manifest(&manifest).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(absolute));
    }

    #[test]
    fn load_dataset_manifest_rejects_empty_dataset_list() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("empty.toml");
        fs::write(&manifest, "# nothing here\n").unwrap();
        let err = load_dataset_manifest(&manifest).unwrap_err();
        assert!(err.contains("no [[dataset]] entries"), "{err}");
    }

    #[test]
    fn load_dataset_manifest_surfaces_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("bad.toml");
        fs::write(&manifest, "this is not = valid toml = x\n").unwrap();
        let err = load_dataset_manifest(&manifest).unwrap_err();
        assert!(err.contains("parse manifest"));
    }

    #[test]
    fn load_dataset_manifest_ignores_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("m.toml");
        fs::write(
            &manifest,
            r#"
[[dataset]]
path = "datasets/retrieval/beir_fiqa.json"
name = "fiqa"
qrels_format = "binary"
approx_bytes = 45_000_000
license = "Apache-2.0"
some_future_hint = "whatever"
"#,
        )
        .unwrap();
        let paths = load_dataset_manifest(&manifest).unwrap();
        assert_eq!(paths.len(), 1);
    }
}
