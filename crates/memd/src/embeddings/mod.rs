//! Embeddings module for vector generation
//!
//! Provides the Embedder trait and implementations (Candle, mock).

pub mod candle_embedder;
pub mod download;
pub mod mock;
pub mod traits;
// Temporarily commented out during Candle migration
// pub mod onnx;
// pub mod python_embedder;

pub use candle_embedder::CandleEmbedder;
pub use download::EmbeddingModel;
pub use mock::MockEmbedder;
// pub use onnx::OnnxEmbedder;
// pub use python_embedder::PythonEmbedder;
pub use traits::{Embedder, EmbeddingConfig, EmbeddingResult, PoolingStrategy};
