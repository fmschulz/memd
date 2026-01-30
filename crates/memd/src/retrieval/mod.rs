//! Retrieval pipeline: fusion, reranking, and context packing
//!
//! Combines dense (semantic) and sparse (keyword) search results into
//! unified rankings, then applies feature-based reranking for context-aware
//! relevance. Finally, packs results with deduplication, MMR diversity, and
//! token budgeting.

pub mod fusion;
pub mod packer;
pub mod reranker;

pub use fusion::{FusedResult, FusionCandidate, FusionSource, RrfConfig, RrfFusion};
pub use packer::{ContextPacker, PackedChunk, PackedContext, PackerConfig, PackerInput};
pub use reranker::{ChunkWithMeta, FeatureReranker, RankedResult, RerankerConfig, RerankerContext};
