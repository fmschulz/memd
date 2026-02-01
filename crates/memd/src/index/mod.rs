//! Index module for vector and lexical search.
//!
//! Provides:
//! - HNSW index for approximate nearest neighbor search (dense)
//! - BM25 index for keyword-based retrieval (sparse)

pub mod bm25;
pub mod embedding_cache;
pub mod hnsw;
pub mod sparse;

pub use bm25::Bm25Index;
pub use embedding_cache::EmbeddingCache;
pub use hnsw::{HnswConfig, HnswIndex, SearchResult};
pub use sparse::{SparseIndex, SparseSearchResult};
