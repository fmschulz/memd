//! Claude (Haiku) consolidator adapter.
//!
//! Spawns `claude -p --model <model> --output-format json`, feeds the
//! prompt on stdin, and extracts the `result` field from the single
//! JSON object Claude emits in `--output-format json` mode.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::{run_cli_capture, Consolidator, DEFAULT_TIMEOUT};
use crate::error::{MemdError, Result};

/// Default Haiku model id used for consolidation.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Consolidator backed by the `claude` CLI in non-interactive print
/// mode.
pub struct ClaudeHaikuConsolidator {
    model: String,
    timeout: Duration,
}

impl Default for ClaudeHaikuConsolidator {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl ClaudeHaikuConsolidator {
    pub fn new(model: impl Into<String>, timeout: Duration) -> Self {
        Self {
            model: model.into(),
            timeout,
        }
    }
}

#[async_trait]
impl Consolidator for ClaudeHaikuConsolidator {
    async fn consolidate(&self, prompt: &str) -> Result<String> {
        let args = [
            "-p",
            "--model",
            self.model.as_str(),
            "--output-format",
            "json",
        ];
        let stdout = run_cli_capture("claude", &args, prompt, self.timeout).await?;
        extract_claude_result(&stdout)
    }

    fn name(&self) -> &'static str {
        "claude-haiku"
    }
}

/// Pull the `result` string out of `claude --output-format json`
/// output. Public for unit tests.
pub(crate) fn extract_claude_result(stdout: &str) -> Result<String> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        MemdError::ValidationError(format!("claude returned non-JSON output: {e}"))
    })?;
    // `--output-format json` wraps the answer in a `result` field;
    // some versions also surface an `is_error` flag.
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(MemdError::ProtocolError(
            "claude reported is_error=true".to_string(),
        ));
    }
    value
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            MemdError::ValidationError(
                "claude JSON output missing string `result` field".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_result_field() {
        let stdout = r#"{"type":"result","result":"[{\"text\":\"x\"}]","is_error":false}"#;
        assert_eq!(extract_claude_result(stdout).unwrap(), "[{\"text\":\"x\"}]");
    }

    #[test]
    fn rejects_is_error() {
        let stdout = r#"{"result":"oops","is_error":true}"#;
        assert!(extract_claude_result(stdout).is_err());
    }

    #[test]
    fn rejects_non_json() {
        assert!(extract_claude_result("not json").is_err());
    }

    #[test]
    fn rejects_missing_result() {
        assert!(extract_claude_result(r#"{"type":"result"}"#).is_err());
    }
}
