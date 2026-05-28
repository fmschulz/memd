#![recursion_limit = "256"]

pub mod auto_priority;
pub mod chunking;
pub mod cli;
pub mod compaction;
pub mod config;
pub mod consolidate;
pub mod embeddings;
pub mod error;
pub mod hit_stats;
pub mod index;
pub mod logging;
pub mod maintenance;
pub mod mcp;
pub mod metrics;
pub mod omf;
pub mod ops;
pub mod retrieval;
pub mod store;
pub mod structural;
pub mod task_memory;
pub mod text;
pub mod tiered;
pub mod types;
pub mod write_admission;

pub use chunking::{chunk_text, Chunk, ChunkingConfig};
pub use compaction::{
    AuditResult, CompactionConfig, CompactionManager, CompactionMetrics, CompactionThresholds,
    Throttle, ThrottleConfig, TombstoneAudit,
};
pub use config::{load_config, Config, ServerConfig};
pub use embeddings::{CandleEmbedder, Embedder, EmbeddingConfig, EmbeddingResult, MockEmbedder};
pub use error::{MemdError, Result};
pub use index::{HnswConfig, HnswIndex, SearchResult};
pub use logging::init_logging;
pub use metrics::{
    IndexStats, LatencyStats, MetricsCollector, MetricsSnapshot, QueryMetrics, Timer,
    TokenUsageStats, ToolTokenAggregate, ToolTokenUsage,
};
pub use ops::configure_operation_routing;
pub use retrieval::{
    ChunkWithMeta, CrossEncoderReranker, FeatureReranker, FusedResult, FusionCandidate,
    FusionSource, RankedResult, RerankerConfig, RerankerContext, RerankerEngine, RerankerMode,
    RrfConfig, RrfFusion,
};
pub use store::{
    MemoryStore, PersistentStore, PersistentStoreConfig, Store, StoreStats, TenantManager,
};
pub use structural::{
    detect_language, parse_file, ExtractedSymbol, LanguageSupport, ParseResult, QueryIntent,
    QueryRouter, RouteResult, StructuralStore, SupportedLanguage, SymbolExtractor, SymbolIndexer,
    SymbolKind, SymbolRecord,
};
pub use task_memory::{
    build_project_brief_digest_artifact, build_project_brief_view, build_task_projections,
    build_task_resume_digest_artifact, build_task_resume_view, derive_artifact_promotion_state,
    derive_artifact_trust_tier, derive_chunk_promotion_state, derive_chunk_trust_tier,
    infer_decision_items, infer_evidence_items, infer_failure_items, infer_highlight_items,
    ArtifactKind, ContributorRef, DatasetRef, DecisionViewItem, EntityRef, EvidenceViewItem,
    FailureViewItem, HighlightViewItem, ProjectBriefView, ProjectionKind, RunDigestItem,
    TaskArtifact, TaskArtifactWriteResult, TaskProjection, TaskProvenance, TaskRecord,
    TaskResumeView, TaskSearchFilters, TrustTier, DIGEST_ROLE_DECISION_LIBRARY,
    DIGEST_ROLE_EVIDENCE_LIBRARY, DIGEST_ROLE_FAILURE_LIBRARY, DIGEST_ROLE_HIGHLIGHT_LIBRARY,
    DIGEST_ROLE_PROJECT_BRIEF, DIGEST_ROLE_TASK_RESUME,
};
pub use text::{CodeTokenizer, ProcessedSentence, Sentence, SentenceSplitter, TextProcessor};
pub use tiered::{
    AccessEvent, AccessTracker, AccessTrackerConfig, CacheEntry, CacheHit, CacheStats,
    CachedResult, HotTier, HotTierConfig, HotTierStats, PromotionScore, SemanticCache,
    SemanticCacheConfig,
};
pub use types::{
    ChunkId, ChunkStatus, ChunkType, MemoryChunk, ProjectId, PromotionState, Source, TenantId,
};
