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

    /// Suite to run (all, mcp)
    #[arg(long, default_value = "all")]
    suite: String,

    /// Output format (pretty, json)
    #[arg(long, default_value = "pretty")]
    output: String,

    /// Skip build step
    #[arg(long)]
    skip_build: bool,
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
    let results: Vec<TestResult> = match args.suite.as_str() {
        "all" => {
            let mut all = vec![];
            all.extend(suites::mcp_conformance::run(&args.memd_path));
            all
        }
        "mcp" => suites::mcp_conformance::run(&args.memd_path),
        _ => {
            eprintln!("Unknown suite: {}", args.suite);
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
