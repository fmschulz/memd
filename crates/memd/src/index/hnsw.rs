//! HNSW warm index for approximate nearest neighbor search
//!
//! Placeholder implementation - full implementation in 03-03-PLAN.md

use crate::error::Result;
use crate::types::ChunkId;

/// Configuration for HNSW index
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// Maximum number of connections per layer
    pub max_nb_connection: usize,
    /// Size of the dynamic candidate list during construction
    pub ef_construction: usize,
    /// Number of results to expand during search
    pub ef_search: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            max_nb_connection: 24,
            ef_construction: 200,
            ef_search: 50,
        }
    }
}

/// Search result from HNSW index
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Chunk ID of the result
    pub chunk_id: ChunkId,
    /// Similarity score (higher is more similar)
    pub score: f32,
}

/// HNSW index for approximate nearest neighbor search
///
/// Stub implementation - full implementation in 03-03-PLAN.md
pub struct HnswIndex {
    config: HnswConfig,
}

impl HnswIndex {
    /// Create a new HNSW index
    pub fn new(config: HnswConfig, _dimension: usize) -> Result<Self> {
        Ok(Self { config })
    }

    /// Get the configuration
    pub fn config(&self) -> &HnswConfig {
        &self.config
    }
}
