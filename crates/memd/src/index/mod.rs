//! Index module for vector search
//!
//! Provides HNSW index for approximate nearest neighbor search.

pub mod hnsw;

pub use hnsw::{HnswConfig, HnswIndex, SearchResult};
