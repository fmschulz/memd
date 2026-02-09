//! Test suites for memd evaluation
//!
//! Each suite tests a specific aspect of MCP conformance.
//!
//! ## Suites
//!
//! - **Suite A**: MCP conformance (mcp_conformance)
//! - **Suite B**: Retrieval quality (retrieval)
//! - **Suite C**: Hybrid retrieval (hybrid)
//! - **Suite D**: Tiered search (tiered)
//! - **Suite E**: Structural queries (structural)
//! - **Suite F**: Compaction (compaction)
//! - **Suite P6**: Offline benchmark protocol (benchmark)

pub mod benchmark_protocol;
pub mod codesearchnet;
pub mod compaction;
pub mod hybrid;
pub mod mcp_conformance;
pub mod nfcorpus;
pub mod persistence;
pub mod retrieval;
pub mod sanity;
pub mod scifact;
pub mod structural;
pub mod tiered;
pub mod true_semantic;
