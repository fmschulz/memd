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
