//! Compatibility re-exports for operation handlers.
//!
//! Operation logic lives in `crate::ops`; this module preserves the historical
//! `memd::mcp::handlers::*` Rust path for downstream callers.

pub use crate::ops::*;
