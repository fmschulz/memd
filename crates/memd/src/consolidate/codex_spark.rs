//! Codex (Spark) consolidator adapter.
//!
//! Spawns `codex exec --model <model> --json`, feeds the prompt on
//! stdin, and extracts the final agent message from the JSONL event
//! stream Codex emits in `--json` mode.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::{run_cli_capture, Consolidator, DEFAULT_TIMEOUT};
use crate::error::{MemdError, Result};

/// Default Codex model id used for consolidation.
pub const DEFAULT_MODEL: &str = "codex-5.3-spark";

/// Consolidator backed by the `codex exec` CLI.
pub struct CodexSparkConsolidator {
    model: String,
    timeout: Duration,
}

impl Default for CodexSparkConsolidator {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl CodexSparkConsolidator {
    pub fn new(model: impl Into<String>, timeout: Duration) -> Self {
        Self {
            model: model.into(),
            timeout,
        }
    }
}

#[async_trait]
impl Consolidator for CodexSparkConsolidator {
    async fn consolidate(&self, prompt: &str) -> Result<String> {
        let args = [
            "exec",
            "--model",
            self.model.as_str(),
            "--json",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "-",
        ];
        let stdout = run_cli_capture("codex", &args, prompt, self.timeout).await?;
        extract_codex_message(&stdout)
    }

    fn name(&self) -> &'static str {
        "codex-spark"
    }
}

/// Extract the final agent message from `codex exec --json` output.
/// Codex emits one JSON event per line; the consolidation answer is
/// the last completed agent/assistant message. Public for unit tests.
pub(crate) fn extract_codex_message(stdout: &str) -> Result<String> {
    let mut last_message: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = codex_message_text(&event) {
            last_message = Some(text);
        }
    }
    last_message.ok_or_else(|| {
        MemdError::ValidationError(
            "codex JSON stream contained no agent message".to_string(),
        )
    })
}

/// Pull message text out of a single Codex JSON event, tolerating the
/// schema variations across Codex CLI versions.
fn codex_message_text(event: &Value) -> Option<String> {
    // Newer schema: {"type":"item.completed","item":{"type":"agent_message","text":"..."}}
    let item = event.get("item").unwrap_or(event);
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let is_message = item_type.contains("agent_message")
        || item_type.contains("assistant_message")
        || item_type == "message";
    if !is_message {
        return None;
    }
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    // Some schemas nest the text under `content` blocks.
    if let Some(blocks) = item.get("content").and_then(Value::as_array) {
        let joined = blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_last_agent_message() {
        let stdout = concat!(
            r#"{"type":"item.started","item":{"type":"reasoning"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"[{\"text\":\"a\"}]"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"[{\"text\":\"final\"}]"}}"#,
        );
        assert_eq!(
            extract_codex_message(stdout).unwrap(),
            "[{\"text\":\"final\"}]"
        );
    }

    #[test]
    fn tolerates_non_json_lines() {
        let stdout = concat!(
            "warming up...\n",
            r#"{"type":"item.completed","item":{"type":"assistant_message","text":"ok"}}"#,
        );
        assert_eq!(extract_codex_message(stdout).unwrap(), "ok");
    }

    #[test]
    fn extracts_content_blocks() {
        let stdout =
            r#"{"item":{"type":"agent_message","content":[{"text":"hello "},{"text":"world"}]}}"#;
        assert_eq!(extract_codex_message(stdout).unwrap(), "hello world");
    }

    #[test]
    fn errors_when_no_message() {
        let stdout = r#"{"type":"item.started","item":{"type":"reasoning"}}"#;
        assert!(extract_codex_message(stdout).is_err());
    }
}
