//! LLM-backed memory consolidation (Phase 2).
//!
//! The consolidator rewrites a working region of recent chunks into a
//! smaller, deduplicated set of `kind:consolidated` lessons. The LLM
//! call itself is pluggable: a [`Consolidator`] is any adapter that
//! turns a prompt string into a response string. Two adapters ship —
//! [`claude_haiku::ClaudeHaikuConsolidator`] and
//! [`codex_spark::CodexSparkConsolidator`] — selected at runtime by
//! [`select::select_consolidator`].
//!
//! The prompt-building and response-parsing logic lives in
//! [`prompt`]; the `memd consolidate` subcommand in
//! `cli::consolidate` orchestrates region selection, the LLM call,
//! and persistence.

pub mod claude_haiku;
pub mod codex_spark;
pub mod prompt;
pub mod select;

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::error::{MemdError, Result};

/// Default wall-clock budget for a single consolidation LLM call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A pluggable LLM backend that rewrites a consolidation prompt into a
/// JSON response. Implementors must not panic on adversarial input —
/// chunk text fed into the prompt is untrusted.
#[async_trait]
pub trait Consolidator: Send + Sync {
    /// Run the consolidation prompt and return the raw model response
    /// (expected to be a JSON array; parsing is the caller's job).
    async fn consolidate(&self, prompt: &str) -> Result<String>;

    /// Stable adapter name, recorded on consolidated chunks as
    /// `consolidator:<name>`.
    fn name(&self) -> &'static str;
}

/// Spawn `program` with `args`, write `stdin_data` to its stdin, and
/// capture stdout. The prompt is passed via stdin (never as an argv
/// entry) so untrusted chunk text cannot reach the shell.
///
/// The whole spawn → write → drain → wait sequence runs under a
/// single `timeout`. stdin writing and stdout/stderr draining each
/// run on their own task so a child that never reads stdin (or fills
/// the output pipe) cannot deadlock. On timeout the child is killed
/// **and reaped** so no zombie is left behind.
pub(crate) async fn run_cli_capture(
    program: &str,
    args: &[&str],
    stdin_data: &str,
    timeout: Duration,
) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| MemdError::ProtocolError(format!("failed to spawn `{program}`: {e}")))?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Feed stdin on its own task: if the child never reads, killing it
    // on timeout closes the pipe and lets this task finish.
    let data = stdin_data.to_string();
    let write_task = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(data.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
    });
    // Drain stdout/stderr concurrently so a full pipe cannot wedge the
    // child before it exits.
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_end(&mut buf).await;
        }
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut buf).await;
        }
        buf
    });

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            write_task.abort();
            out_task.abort();
            err_task.abort();
            return Err(MemdError::ProtocolError(format!(
                "`{program}` did not complete: {e}"
            )));
        }
        Err(_) => {
            // Timeout: kill, then wait to reap the zombie.
            let _ = child.start_kill();
            let _ = child.wait().await;
            write_task.abort();
            out_task.abort();
            err_task.abort();
            return Err(MemdError::ProtocolError(format!(
                "`{program}` timed out after {}s",
                timeout.as_secs()
            )));
        }
    };

    let _ = write_task.await;
    let stdout_bytes = out_task.await.unwrap_or_default();
    let stderr_bytes = err_task.await.unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let truncated: String = stderr.chars().take(500).collect();
        return Err(MemdError::ProtocolError(format!(
            "`{program}` exited with {status}: {truncated}"
        )));
    }

    String::from_utf8(stdout_bytes)
        .map_err(|e| MemdError::ProtocolError(format!("`{program}` produced non-UTF8 output: {e}")))
}

/// Environment variable read by [`MockEnvConsolidator`].
pub const MOCK_RESPONSE_ENV: &str = "MEMD_CONSOLIDATOR_MOCK_RESPONSE";

/// Hermetic consolidator that returns the contents of
/// `$MEMD_CONSOLIDATOR_MOCK_RESPONSE` verbatim.
///
/// Selected only by an explicit `MEMD_CONSOLIDATOR=mock`. It lets
/// integration tests exercise the full `memd consolidate` path —
/// including `run_cli` dispatch — without spawning a real LLM CLI.
pub struct MockEnvConsolidator;

#[async_trait]
impl Consolidator for MockEnvConsolidator {
    async fn consolidate(&self, _prompt: &str) -> Result<String> {
        std::env::var(MOCK_RESPONSE_ENV).map_err(|_| {
            MemdError::ConfigError(format!(
                "MEMD_CONSOLIDATOR=mock requires {MOCK_RESPONSE_ENV} to be set"
            ))
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}
