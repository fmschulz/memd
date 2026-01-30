//! Embeddings module for vector generation
//!
//! Provides the Embedder trait and implementations (ONNX, mock).

pub mod download;
pub mod mock;
pub mod onnx;
pub mod traits;

pub use mock::MockEmbedder;
pub use onnx::OnnxEmbedder;
pub use traits::{Embedder, EmbeddingConfig, EmbeddingResult};
