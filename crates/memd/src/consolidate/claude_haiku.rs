//! Tool-free Claude consolidator adapter.
//!
//! Spawns `claude -p --model <model> --output-format json`, feeds the
//! prompt on stdin, and extracts the `result` field from the single
//! JSON object Claude emits in `--output-format json` mode.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::{
    cli_version, run_cli_capture_command, Consolidator, ConsolidatorIdentity, DEFAULT_TIMEOUT,
};
use crate::error::{MemdError, Result};

/// Default Claude model tier used for consolidation.
pub const DEFAULT_MODEL: &str = "sonnet";

/// Consolidator backed by the `claude` CLI in non-interactive print
/// mode.
pub struct ClaudeHaikuConsolidator {
    model: String,
    timeout: Duration,
}

impl Default for ClaudeHaikuConsolidator {
    fn default() -> Self {
        Self {
            model: std::env::var("MEMD_CONSOLIDATION_MODEL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
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

impl ClaudeHaikuConsolidator {
    async fn consolidate_with_program(&self, program: &str, prompt: &str) -> Result<String> {
        let args = [
            "-p",
            "--model",
            self.model.as_str(),
            "--output-format",
            "json",
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources",
            "",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--disable-slash-commands",
        ];
        let directory = tempfile::tempdir()?;
        let mut command = tokio::process::Command::new(program);
        command.args(args).current_dir(directory.path());
        let stdout = run_cli_capture_command(command, prompt, self.timeout).await?;
        extract_claude_result(&stdout)
    }
}

#[async_trait]
impl Consolidator for ClaudeHaikuConsolidator {
    async fn consolidate(&self, prompt: &str) -> Result<String> {
        self.consolidate_with_program("claude", prompt).await
    }

    fn name(&self) -> &'static str {
        "claude-haiku"
    }

    async fn identity(&self) -> Result<ConsolidatorIdentity> {
        Ok(ConsolidatorIdentity {
            adapter: self.name().to_string(),
            command: Some(format!(
                "claude -p --model {} --output-format json --tools '' --strict-mcp-config --mcp-config '{{\"mcpServers\":{{}}}}' --setting-sources '' --settings '{{\"disableAllHooks\":true}}' --disable-slash-commands",
                self.model
            )),
            model: Some(self.model.clone()),
            version: Some(cli_version("claude", self.timeout).await?),
        })
    }
}

/// Pull the `result` string out of `claude --output-format json`
/// output. Public for unit tests.
pub(crate) fn extract_claude_result(stdout: &str) -> Result<String> {
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| MemdError::ValidationError(format!("claude returned non-JSON output: {e}")))?;
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
    fn audit_claude_default_uses_tier_alias() {
        assert_eq!(DEFAULT_MODEL, "sonnet");
    }

    #[test]
    fn audit_claude_model_environment_override() {
        if std::env::var_os("MEMD_AUDIT_MODEL_CHILD").is_some() {
            assert_eq!(ClaudeHaikuConsolidator::default().model, "custom-model");
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "consolidate::claude_haiku::tests::audit_claude_model_environment_override",
                "--nocapture",
            ])
            .env("MEMD_AUDIT_MODEL_CHILD", "1")
            .env("MEMD_CONSOLIDATION_MODEL", "custom-model")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn audit_claude_has_no_tools_and_uses_empty_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let stub = directory.path().join("claude-stub");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
tools=no
strict=no
settings=no
skills=no
while [ "$#" -gt 0 ]; do
  case "$1" in
    --tools) shift; [ "$1" = "" ] && tools=yes ;;
    --strict-mcp-config) strict=yes ;;
    --setting-sources) shift; [ "$1" = "" ] && settings=yes ;;
    --disable-slash-commands) skills=yes ;;
  esac
  shift
done
[ "$tools/$strict/$settings/$skills" = yes/yes/yes/yes ] || exit 41
[ -z "$(ls -A)" ] || exit 42
cat >/dev/null
printf '{"result":"[]"}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeHaikuConsolidator::default();
        assert_eq!(
            adapter
                .consolidate_with_program(stub.to_str().unwrap(), "memory data")
                .await
                .unwrap(),
            "[]"
        );
    }

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
