//! CLI client for current memd evaluation suites.
//!
//! The released executable no longer starts an MCP stdio server. Evaluation
//! suites exercise the same operation surface through `memd call`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliClientError {
    #[error("failed to spawn process: {0}")]
    SpawnError(#[from] std::io::Error),

    #[error("failed to parse JSON: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("CLI operation failed: {0}")]
    CommandError(String),
}

pub struct CliClient {
    memd_path: PathBuf,
    global_args: Vec<String>,
    _temp_dir_guard: Option<TempDir>,
}

impl CliClient {
    pub fn start_with_args(memd_path: &Path, extra_args: &[&str]) -> Self {
        Self {
            memd_path: memd_path.to_path_buf(),
            global_args: extra_args.iter().map(|arg| arg.to_string()).collect(),
            _temp_dir_guard: None,
        }
    }

    pub fn start(memd_path: &str) -> Result<Self, CliClientError> {
        let data_dir = TempDir::new()?;
        let data_dir_arg = data_dir.path().to_string_lossy().to_string();
        Ok(Self {
            memd_path: PathBuf::from(memd_path),
            global_args: vec!["--data-dir".to_string(), data_dir_arg],
            _temp_dir_guard: Some(data_dir),
        })
    }

    pub fn call(&self, name: &str, arguments: Value) -> Result<Value, CliClientError> {
        let json_arg = serde_json::to_string(&arguments)?;
        let output = Command::new(&self.memd_path)
            .args(&self.global_args)
            .arg("call")
            .arg(name)
            .arg("--json")
            .arg(json_arg)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            return Err(CliClientError::CommandError(detail));
        }

        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub fn is_available(&self) -> bool {
        Command::new(&self.memd_path)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}
