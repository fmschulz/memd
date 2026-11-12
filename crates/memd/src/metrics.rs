//! Store index statistics.

use serde::{Deserialize, Serialize};

/// Index statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexStats {
    /// Number of chunks indexed
    pub chunks_indexed: u64,
    /// Number of embeddings stored
    pub embeddings_count: u64,
    /// Embedding dimension
    pub embedding_dimension: usize,
    /// Total index memory estimate (bytes)
    pub index_memory_bytes: u64,
}
