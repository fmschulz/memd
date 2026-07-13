//! Test suites for memd evaluation
//!
//! Each suite tests a specific aspect of CLI behavior or retrieval quality.
//!
//! ## Suites
//!
//! - **Suite A**: CLI contract (cli_contract)
//! - **Suite B**: Retrieval quality (retrieval)
//! - **Suite C**: Hybrid retrieval (hybrid)
//! - **Suite D**: Tiered search (tiered)
//! - **Suite E**: Structural queries (structural)
//! - **Suite F**: Compaction (compaction)
//! - **Suite P6**: Offline benchmark protocol (benchmark)
//! - **Suite P6R**: Benchmark regression significance gate (benchmark-regression)

pub mod benchmark_protocol;
pub mod cli_contract;
pub mod codesearchnet;
pub mod compaction;
pub mod hybrid;
pub mod longitudinal;
pub mod nfcorpus;
pub mod persistence;
pub mod retrieval;
pub mod sanity;
pub mod scifact;
pub mod structural;
pub mod tiered;
pub mod true_semantic;
