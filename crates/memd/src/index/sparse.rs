//! Sparse index trait for keyword-based retrieval.
//!
//! Provides a trait for lexical search using inverted indexes (BM25).
//! Sparse indexes excel at exact matches: function names, file paths, error messages.

use crate::error::Result;
use crate::types::{ChunkId, TenantId};

/// Result from a sparse/lexical search.
#[derive(Debug, Clone)]
pub struct SparseSearchResult {
    /// The chunk ID of the matching document
    pub chunk_id: ChunkId,
    /// BM25 relevance score (higher = more relevant)
    pub score: f32,
    /// Index of the sentence that matched within the chunk
    pub sentence_idx: usize,
}

/// Trait for sparse/lexical indexes.
///
/// Sparse indexes use inverted indexes for keyword-based retrieval.
/// They complement dense (embedding-based) indexes by capturing exact matches
/// that semantic search might miss.
pub trait SparseIndex: Send + Sync {
    /// Index a chunk's sentences for keyword search.
    ///
    /// Each sentence is indexed separately to enable fine-grained matching.
    fn insert(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        sentences: &[String],
    ) -> Result<()>;

    /// Search for chunks matching the query.
    ///
    /// Returns up to `k` results sorted by BM25 score (descending).
    fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<SparseSearchResult>>;

    /// Remove a chunk from the index.
    ///
    /// Returns true if the chunk was found and deleted.
    fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool>;

    /// Get the number of indexed documents for a tenant.
    fn doc_count(&self, tenant_id: &TenantId) -> Result<u64>;
}
