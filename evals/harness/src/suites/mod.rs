//! Test suites for memd evaluation
//!
//! Each suite tests a specific aspect of CLI behavior or retrieval quality.
//!
//! ## Suites
//!
//! - **Suite A**: CLI contract (cli_contract)
//! - **Suite B**: Retrieval quality (retrieval)
//! - **Suite P6**: Offline benchmark protocol (benchmark)
//! - **Suite P6R**: Benchmark regression significance gate (benchmark-regression)

pub mod benchmark_protocol;
pub mod cli_contract;
pub mod longitudinal;
pub mod persistence;
pub mod retrieval;
