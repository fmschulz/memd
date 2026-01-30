//! Embeddings module for vector generation
//!
//! Provides the Embedder trait and implementations (ONNX, mock).

pub mod mock;
pub mod traits;

pub use mock::MockEmbedder;
pub use traits::{Embedder, EmbeddingConfig, EmbeddingResult};
