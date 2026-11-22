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

/// Revision of the vectors the embedder produces for a given text and model.
///
/// Bump it whenever a change makes new vectors differ from stored ones. A
/// writable store whose dense indexes were built under another revision
/// discards them and re-embeds its chunks once (`store::embedding_revision`).
///
/// - 1: the attention mask was passed as `token_type_ids` and padding was
///   attended, so a vector depended on its batch neighbours.
/// - 2: token type ids are zero and the attention mask is applied.
pub const EMBEDDING_REVISION: u32 = 2;

pub use candle_embedder::CandleEmbedder;
pub use download::CandleModel;
pub use mock::MockEmbedder;
// pub use onnx::OnnxEmbedder;
// pub use python_embedder::PythonEmbedder;
pub use traits::{Embedder, EmbeddingConfig, EmbeddingResult, PoolingStrategy};
