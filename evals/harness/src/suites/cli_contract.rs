//! CLI contract suite.
//!
//! Verifies the current public executable surface used by automation and
//! confirms that the retired MCP stdio entry point is not accepted.

use std::process::Command;
use std::time::Instant;

use crate::TestResult;

pub fn run(memd_path: &str) -> Vec<TestResult> {
    vec![
        expect_success("CLI_help", memd_path, &["--help"]),
        expect_success("CLI_add_help", memd_path, &["add", "--help"]),
        expect_success("CLI_search_help", memd_path, &["search", "--help"]),
        expect_success(
            "CLI_agent_context_help",
            memd_path,
            &["agent-context", "--help"],
        ),
        expect_success("CLI_call_help", memd_path, &["call", "--help"]),
        expect_success("CLI_batch_help", memd_path, &["batch", "--help"]),
        expect_success("CLI_warm_help", memd_path, &["warm", "--help"]),
        expect_failure("CLI_rejects_mcp_mode", memd_path, &["--mode", "mcp"]),
    ]
}

fn expect_success(name: &str, memd_path: &str, args: &[&str]) -> TestResult {
    let start = Instant::now();
    match Command::new(memd_path).args(args).output() {
        Ok(output) if output.status.success() => TestResult::pass_with_duration(name, start),
        Ok(output) => TestResult::fail_with_duration(
            name,
            &format!(
                "expected success, got status {:?}: {}",
                output.status.code(),
                command_output(&output)
            ),
            start,
        ),
        Err(error) => TestResult::fail_with_duration(name, &error.to_string(), start),
    }
}

fn expect_failure(name: &str, memd_path: &str, args: &[&str]) -> TestResult {
    let start = Instant::now();
    match Command::new(memd_path).args(args).output() {
        Ok(output) if !output.status.success() => TestResult::pass_with_duration(name, start),
        Ok(output) => TestResult::fail_with_duration(
            name,
            &format!(
                "expected failure, command succeeded: {}",
                command_output(&output)
            ),
            start,
        ),
        Err(error) => TestResult::fail_with_duration(name, &error.to_string(), start),
    }
}

fn command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => "<no output>".to_string(),
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}
