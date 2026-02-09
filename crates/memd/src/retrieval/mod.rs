//! Retrieval pipeline: fusion, reranking, and context packing
//!
//! Combines dense (semantic) and sparse (keyword) search results into
//! unified rankings, then applies feature-based reranking for context-aware
//! relevance, and packs results within token budgets.

pub mod fusion;
pub mod packer;
pub mod reranker;

pub use fusion::{FusedResult, FusionCandidate, FusionSource, RrfConfig, RrfFusion};
pub use packer::{ContextPacker, PackedChunk, PackedContext, PackerConfig, PackerInput};
pub use reranker::{
    ChunkWithMeta, CrossEncoderReranker, FeatureReranker, RankedResult, RerankerConfig,
    RerankerContext, RerankerEngine, RerankerMode,
};
