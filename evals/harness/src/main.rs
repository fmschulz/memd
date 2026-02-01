//! memd evaluation harness
//!
//! Runs MCP conformance tests against memd.

use clap::Parser;
use std::process::{Command, ExitCode};

use memd_evals::suites;
use memd_evals::TestResult;

/// memd evaluation harness
#[derive(Parser, Debug)]
#[command(name = "memd-evals", version, about)]
struct Args {
    /// Path to memd binary
    #[arg(long, default_value = "target/debug/memd")]
    memd_path: String,

    /// Suite to run (all, sanity, mcp, persistence, retrieval, hybrid, scifact, true-semantic, nfcorpus, codesearchnet, tiered, structural, compaction)
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
}

fn main() -> ExitCode {
    let args = Args::parse();

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
            all.extend(suites::retrieval::run_retrieval_tests(&memd_binary, embedding_model));
            all.extend(suites::hybrid::run_hybrid_tests(&memd_binary, embedding_model));
            all.extend(suites::true_semantic::run_true_semantic_tests(&memd_binary, embedding_model));
            all.extend(suites::scifact::run_scifact_tests(&memd_binary, embedding_model));
            all.extend(suites::codesearchnet::run_codesearchnet_tests(&memd_binary, embedding_model));

            // Structural tests (Suite E)
            if args.include_structural {
                all.extend(suites::structural::run_structural_tests(&memd_binary, embedding_model));
            }

            // Compaction tests (Suite F) - excluded by default (slower)
            if args.include_compaction {
                all.extend(suites::compaction::run_compaction_tests(&memd_binary, embedding_model));
            }
            all
        }
        "sanity" => suites::sanity::run_sanity_tests(&memd_binary, embedding_model),
        "mcp" => suites::mcp_conformance::run(&args.memd_path),
        "persistence" => suites::persistence::run_all(&memd_binary),
        "retrieval" => suites::retrieval::run_retrieval_tests(&memd_binary, embedding_model),
        "hybrid" => suites::hybrid::run_hybrid_tests(&memd_binary, embedding_model),
        "true-semantic" => suites::true_semantic::run_true_semantic_tests(&memd_binary, embedding_model),
        "scifact" => suites::scifact::run_scifact_tests(&memd_binary, embedding_model),
        "nfcorpus" => suites::nfcorpus::run_nfcorpus_tests(&memd_binary, embedding_model),
        "codesearchnet" => suites::codesearchnet::run_codesearchnet_tests(&memd_binary, embedding_model),
        "tiered" => suites::tiered::run_tiered_tests(&memd_binary, embedding_model),
        "structural" => suites::structural::run_structural_tests(&memd_binary, embedding_model),
        "compaction" | "f" => suites::compaction::run_compaction_tests(&memd_binary, embedding_model),
        _ => {
            eprintln!(
                "Unknown suite: {}. Available: all, sanity, mcp, persistence, retrieval, hybrid, true-semantic, scifact, nfcorpus, codesearchnet, tiered, structural, compaction",
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
        println!(
            "\n{}",
            serde_json::to_string_pretty(&json_results).unwrap()
        );
    }

    if passed == total {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
