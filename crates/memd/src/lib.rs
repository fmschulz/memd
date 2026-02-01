pub mod chunking;
pub mod cli;
pub mod compaction;
pub mod config;
pub mod embeddings;
pub mod error;
pub mod index;
pub mod logging;
pub mod mcp;
pub mod metrics;
pub mod retrieval;
pub mod store;
pub mod structural;
pub mod text;
pub mod tiered;
pub mod types;

pub use chunking::{Chunk, ChunkingConfig, chunk_text};
pub use compaction::{
    AuditResult, CompactionConfig, CompactionManager, CompactionMetrics, CompactionThresholds,
    TombstoneAudit,
};
pub use config::{load_config, Config, ServerConfig};
pub use embeddings::{CandleEmbedder, Embedder, EmbeddingConfig, EmbeddingResult, MockEmbedder};
pub use error::{MemdError, Result};
pub use index::{HnswConfig, HnswIndex, SearchResult};
pub use logging::init_logging;
pub use mcp::{McpServer, run_server};
pub use metrics::{IndexStats, LatencyStats, MetricsCollector, MetricsSnapshot, QueryMetrics, Timer};
pub use store::{MemoryStore, PersistentStore, PersistentStoreConfig, Store, StoreStats, TenantManager};
pub use retrieval::{
    ChunkWithMeta, FeatureReranker, FusedResult, FusionCandidate, FusionSource, RankedResult,
    RerankerConfig, RerankerContext, RrfConfig, RrfFusion,
};
pub use text::{CodeTokenizer, ProcessedSentence, Sentence, SentenceSplitter, TextProcessor};
pub use tiered::{
    AccessEvent, AccessTracker, AccessTrackerConfig, CacheEntry, CacheHit, CacheStats,
    CachedResult, HotTier, HotTierConfig, HotTierStats, PromotionScore, SemanticCache,
    SemanticCacheConfig,
};
pub use structural::{
    detect_language, parse_file, ExtractedSymbol, LanguageSupport, ParseResult, QueryIntent,
    QueryRouter, RouteResult, StructuralStore, SupportedLanguage, SymbolExtractor, SymbolIndexer,
    SymbolKind, SymbolRecord,
};
pub use types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk, ProjectId, Source, TenantId};
