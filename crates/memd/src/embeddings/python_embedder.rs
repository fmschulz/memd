use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::traits::{Embedder, EmbeddingConfig, EmbeddingResult};
use crate::error::{MemdError, Result};

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
    id: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
}

/// Python-based embedder using sentence-transformers
///
/// Runs a Python subprocess for embeddings, avoiding C++ linking issues.
/// Requires uv or pixi for Python environment management.
pub struct PythonEmbedder {
    process: Arc<Mutex<EmbeddingProcess>>,
    config: EmbeddingConfig,
    request_id: Arc<Mutex<u64>>,
}

struct EmbeddingProcess {
    child: Child,
}

impl PythonEmbedder {
    /// Create new Python embedder with default model (all-MiniLM-L6-v2)
    pub fn new() -> Result<Self> {
        Self::with_model("sentence-transformers/all-MiniLM-L6-v2")
    }

    /// Create embedder with specific model
    pub fn with_model(model_name: &str) -> Result<Self> {
        // Find Python executable
        let python_path = Self::find_python()?;

        tracing::info!("Starting embedding service with Python: {}", python_path);

        // Start Python process
        let script_path = Self::get_script_path()?;

        let mut child = Command::new(&python_path)
            .arg(&script_path)
            .arg(model_name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                MemdError::ConfigError(format!(
                    "Failed to start embedding service: {}. \
                     Make sure to run: uv venv .venv && uv pip install sentence-transformers torch",
                    e
                ))
            })?;

        let process = EmbeddingProcess { child };

        let embedder = Self {
            process: Arc::new(Mutex::new(process)),
            config: EmbeddingConfig::default(),
            request_id: Arc::new(Mutex::new(0)),
        };

        // Get dimension from Python service
        let dimension = embedder.get_dimension()?;

        tracing::info!("Embedding service ready: {}D", dimension);

        Ok(Self {
            process: embedder.process,
            config: EmbeddingConfig {
                dimension,
                ..Default::default()
            },
            request_id: embedder.request_id,
        })
    }

    fn find_python() -> Result<String> {
        // Priority:
        // 1. .venv/bin/python (uv)
        // 2. Python in PATH
        // 3. python3 in PATH

        if std::path::Path::new(".venv/bin/python").exists() {
            return Ok(".venv/bin/python".to_string());
        }

        if let Ok(path) = std::env::var("PYTHON") {
            return Ok(path);
        }

        if std::path::Path::new("/usr/bin/python3").exists() {
            return Ok("/usr/bin/python3".to_string());
        }

        if std::path::Path::new("/usr/bin/python").exists() {
            return Ok("/usr/bin/python".to_string());
        }

        Err(MemdError::ConfigError(
            "No Python found. Run: uv venv .venv".to_string(),
        ))
    }

    fn get_script_path() -> Result<String> {
        // Try multiple locations for the script
        let candidates = [
            "python/embedding_service.py",
            "./python/embedding_service.py",
            "../python/embedding_service.py",
        ];

        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Ok(path.to_string());
            }
        }

        Err(MemdError::ConfigError(
            "embedding_service.py not found. Make sure you're in the memd root directory.".to_string(),
        ))
    }

    fn call_method(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let mut process = self.process.lock();
        let mut request_id = self.request_id.lock();
        *request_id += 1;

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: *request_id,
        };

        // Send request
        let stdin = process
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| MemdError::EmbeddingError("No stdin available".into()))?;

        let request_json = serde_json::to_string(&request)
            .map_err(|e| MemdError::EmbeddingError(format!("JSON serialize error: {}", e)))?;

        writeln!(stdin, "{}", request_json)
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to write to stdin: {}", e)))?;

        stdin
            .flush()
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to flush stdin: {}", e)))?;

        // Read response
        let stdout = process
            .child
            .stdout
            .as_mut()
            .ok_or_else(|| MemdError::EmbeddingError("No stdout available".into()))?;

        let mut reader = BufReader::new(stdout);
        let mut response_line = String::new();

        reader
            .read_line(&mut response_line)
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to read response: {}", e)))?;

        let response: JsonRpcResponse = serde_json::from_str(&response_line).map_err(|e| {
            MemdError::EmbeddingError(format!("Failed to parse response: {}. Got: {}", e, response_line))
        })?;

        if let Some(error) = response.error {
            return Err(MemdError::EmbeddingError(format!(
                "Python error (code {}): {}",
                error.code, error.message
            )));
        }

        response
            .result
            .ok_or_else(|| MemdError::EmbeddingError("No result in response".into()))
    }

    fn get_dimension(&self) -> Result<usize> {
        let result = self.call_method("dimension", json!({}))?;
        Ok(result.as_u64().ok_or_else(|| {
            MemdError::EmbeddingError("Invalid dimension response".into())
        })? as usize)
    }

    /// Ping the embedding service to check if it's alive
    pub fn ping(&self) -> Result<bool> {
        let result = self.call_method("ping", json!({}))?;
        Ok(result.as_str() == Some("pong"))
    }
}

#[async_trait::async_trait]
impl Embedder for PythonEmbedder {
    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let texts_owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let result = self.call_method("embed", json!({ "texts": texts_owned }))?;

        let embeddings: Vec<Vec<f32>> = serde_json::from_value(result)
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to parse embeddings: {}", e)))?;

        // Validate dimensions
        for (i, emb) in embeddings.iter().enumerate() {
            if emb.len() != self.config.dimension {
                return Err(MemdError::ValidationError(format!(
                    "Embedding {} has wrong dimension: expected {}, got {}",
                    i,
                    self.config.dimension,
                    emb.len()
                )));
            }
        }

        Ok(embeddings)
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        let results = self.embed_texts(&[query]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| MemdError::EmbeddingError("No embedding returned for query".into()))
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }

    fn config(&self) -> &EmbeddingConfig {
        &self.config
    }
}

impl Drop for EmbeddingProcess {
    fn drop(&mut self) {
        tracing::debug!("Shutting down embedding service");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires Python setup
    fn test_python_embedder_basic() {
        let embedder = PythonEmbedder::new().unwrap();

        // Test ping
        assert!(embedder.ping().unwrap());

        // Test dimension
        assert_eq!(embedder.dimension(), 384);

        // Test embedding
        let texts = vec!["hello world", "test embedding"];
        let embeddings = embedder.embed(&texts).unwrap();

        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 384);
        assert_eq!(embeddings[1].len(), 384);
    }
}
