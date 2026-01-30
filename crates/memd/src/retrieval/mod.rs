//! Retrieval pipeline: fusion and reranking
//!
//! Combines dense (semantic) and sparse (keyword) search results into
//! unified rankings, then applies feature-based reranking for context-aware
//! relevance.

pub mod fusion;
pub mod reranker;

pub use fusion::{FusedResult, FusionCandidate, FusionSource, RrfConfig, RrfFusion};
pub use reranker::{ChunkWithMeta, FeatureReranker, RankedResult, RerankerConfig, RerankerContext};
