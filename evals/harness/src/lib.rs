//! memd evaluation harness
//!
//! Provides test infrastructure for MCP conformance testing.
//! The harness starts memd as a subprocess and communicates via MCP protocol.
//!
//! ## Suites
//!
//! - **Suite A**: MCP conformance (mcp_conformance)
//! - **Suite B**: Retrieval quality (retrieval)
//! - **Suite C**: Hybrid retrieval (hybrid)
//! - **Suite D**: Tiered search (tiered)
//! - **Suite E**: Structural queries (structural)

pub mod mcp_client;
pub mod metrics;
pub mod statistics;
pub mod suites;

// Re-export structural suite results for external use
pub use suites::structural::{StructuralSuiteResults, StructuralTestResult};

use std::path::PathBuf;
use std::time::Instant;

/// Result of a single test
#[derive(Debug, Clone)]
pub struct TestResult {
    /// Test name (e.g., "A1_initialize")
    pub name: String,
    /// Whether the test passed
    pub passed: bool,
    /// Error message if failed
    pub message: String,
    /// Test duration in milliseconds
    pub duration_ms: u64,
}

impl TestResult {
    /// Create a passing test result
    pub fn pass(name: &str) -> Self {
        Self {
            name: name.to_string(),
            passed: true,
            message: String::new(),
            duration_ms: 0,
        }
    }

    /// Create a failing test result
    pub fn fail(name: &str, message: &str) -> Self {
        Self {
            name: name.to_string(),
            passed: false,
            message: message.to_string(),
            duration_ms: 0,
        }
    }

    /// Create a passing test result with duration
    pub fn pass_with_duration(name: &str, start: Instant) -> Self {
        Self {
            name: name.to_string(),
            passed: true,
            message: String::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Create a failing test result with duration
    pub fn fail_with_duration(name: &str, message: &str, start: Instant) -> Self {
        Self {
            name: name.to_string(),
            passed: false,
            message: message.to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

/// Resolve dataset path override from environment.
///
/// If `MEMD_EVAL_DATASET_PATH` is set, that path is used. Otherwise, the
/// provided default path is returned.
pub fn resolve_dataset_path(default_path: &str) -> PathBuf {
    std::env::var("MEMD_EVAL_DATASET_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default_path))
}

/// Normalize dataset chunk types used by evaluation fixtures.
///
/// Returns `None` for unknown/unsupported types.
pub fn normalize_eval_chunk_type(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "code" => Some("code"),
        "doc" => Some("doc"),
        "scientific" => Some("scientific"),
        "trace" => Some("trace"),
        "decision" => Some("decision"),
        "plan" => Some("plan"),
        "research" => Some("research"),
        "message" => Some("message"),
        "summary" => Some("summary"),
        "general" => Some("general"),
        "other" => Some("other"),
        "documentation" => Some("doc"),
        "log" => Some("trace"),
        "research_paper" => Some("scientific"),
        "nfcorpus" => Some("scientific"),
        "biomedical" => Some("scientific"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn normalize_eval_chunk_type_maps_legacy_aliases() {
        assert_eq!(normalize_eval_chunk_type("log"), Some("trace"));
        assert_eq!(normalize_eval_chunk_type("documentation"), Some("doc"));
        assert_eq!(
            normalize_eval_chunk_type("research_paper"),
            Some("scientific")
        );
        assert_eq!(normalize_eval_chunk_type("nfcorpus"), Some("scientific"));
        assert_eq!(normalize_eval_chunk_type("biomedical"), Some("scientific"));
        assert_eq!(normalize_eval_chunk_type("doc"), Some("doc"));
        assert_eq!(normalize_eval_chunk_type("unknown_kind"), None);
    }

    #[test]
    fn retrieval_dataset_types_are_supported_or_mapped() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../datasets/retrieval");
        let entries = std::fs::read_dir(&base).expect("read retrieval dataset dir");

        for entry in entries {
            let path = entry.expect("read dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = std::fs::read_to_string(&path).expect("read dataset file");
            let value: Value = serde_json::from_str(&content).expect("parse dataset json");
            let Some(documents) = value.get("documents").and_then(|v| v.as_array()) else {
                continue;
            };

            for doc in documents {
                let Some(raw_type) = doc.get("type").and_then(|v| v.as_str()) else {
                    continue;
                };
                assert!(
                    normalize_eval_chunk_type(raw_type).is_some(),
                    "unsupported chunk type '{}' in {}",
                    raw_type,
                    path.display()
                );
            }
        }
    }
}
