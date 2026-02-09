//! Feature and cross-encoder reranking for context-aware retrieval.
//!
//! Applies recency, project/type preferences, and optional query-document
//! interaction scoring to produce final rankings.

use crate::types::{ChunkId, ChunkType};

/// Reranker strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankerMode {
    /// Metadata-only feature reranking.
    Feature,
    /// Query-document interaction reranking.
    CrossEncoder,
}

impl Default for RerankerMode {
    fn default() -> Self {
        Self::Feature
    }
}

/// Configuration for reranking.
#[derive(Debug, Clone)]
pub struct RerankerConfig {
    /// Selected reranker strategy.
    pub mode: RerankerMode,
    /// Weight for RRF score.
    pub rrf_weight: f32,
    /// Weight for recency bonus.
    pub recency_weight: f32,
    /// Decay half-life in days.
    pub recency_half_life_days: f32,
    /// Weight for project match bonus.
    pub project_weight: f32,
    /// Weight for type match bonus.
    pub type_weight: f32,
    /// Weight for cross-encoder interaction score.
    pub cross_encoder_weight: f32,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            mode: RerankerMode::Feature,
            rrf_weight: 1.0,
            recency_weight: 0.1,
            recency_half_life_days: 7.0,
            project_weight: 0.2,
            type_weight: 0.05,
            cross_encoder_weight: 0.7,
        }
    }
}

/// Input chunk with metadata for reranking.
#[derive(Debug, Clone)]
pub struct ChunkWithMeta {
    pub chunk_id: ChunkId,
    pub rrf_score: f32,
    pub timestamp_created: i64,
    pub project_id: Option<String>,
    pub chunk_type: ChunkType,
    /// Optional text payload, required for cross-encoder scoring.
    pub text: Option<String>,
}

/// Context for reranking.
#[derive(Debug, Clone)]
pub struct RerankerContext {
    pub current_project: Option<String>,
    pub preferred_types: Vec<ChunkType>,
    pub now_ms: i64,
    /// Optional query text for query-document interaction reranking.
    pub query_text: Option<String>,
}

impl RerankerContext {
    pub fn now() -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Self {
            current_project: None,
            preferred_types: Vec::new(),
            now_ms,
            query_text: None,
        }
    }

    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        let project = project.into();
        self.current_project = if project.is_empty() {
            None
        } else {
            Some(project)
        };
        self
    }

    pub fn with_preferred_types(mut self, types: Vec<ChunkType>) -> Self {
        self.preferred_types = types;
        self
    }

    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        let query = query.into();
        self.query_text = if query.trim().is_empty() {
            None
        } else {
            Some(query)
        };
        self
    }
}

/// Ranked result with scoring components.
#[derive(Debug, Clone)]
pub struct RankedResult {
    pub chunk_id: ChunkId,
    pub final_score: f32,
    pub rrf_score: f32,
    pub recency_bonus: f32,
    pub project_bonus: f32,
    pub type_bonus: f32,
    pub cross_encoder_score: f32,
}

/// Feature-only reranker.
pub struct FeatureReranker {
    config: RerankerConfig,
}

impl FeatureReranker {
    pub fn new(config: RerankerConfig) -> Self {
        Self { config }
    }

    pub fn default_config() -> Self {
        Self::new(RerankerConfig::default())
    }

    pub fn rerank(
        &self,
        chunks: Vec<ChunkWithMeta>,
        context: &RerankerContext,
    ) -> Vec<RankedResult> {
        let mut results: Vec<RankedResult> = chunks
            .into_iter()
            .map(|chunk| {
                let recency_bonus =
                    self.compute_recency_bonus(chunk.timestamp_created, context.now_ms);
                let project_bonus =
                    self.compute_project_bonus(&chunk.project_id, &context.current_project);
                let type_bonus =
                    self.compute_type_bonus(chunk.chunk_type, &context.preferred_types);
                let final_score = self.config.rrf_weight * chunk.rrf_score
                    + self.config.recency_weight * recency_bonus
                    + self.config.project_weight * project_bonus
                    + self.config.type_weight * type_bonus;

                RankedResult {
                    chunk_id: chunk.chunk_id,
                    final_score,
                    rrf_score: chunk.rrf_score,
                    recency_bonus,
                    project_bonus,
                    type_bonus,
                    cross_encoder_score: 0.0,
                }
            })
            .collect();

        sort_desc(&mut results);
        results
    }

    fn compute_recency_bonus(&self, timestamp_created: i64, now_ms: i64) -> f32 {
        let age_ms = (now_ms - timestamp_created).max(0) as f64;
        let age_days = age_ms / (1000.0 * 60.0 * 60.0 * 24.0);
        let decay_rate = std::f64::consts::LN_2 / self.config.recency_half_life_days as f64;
        (-age_days * decay_rate).exp() as f32
    }

    fn compute_project_bonus(
        &self,
        chunk_project: &Option<String>,
        current_project: &Option<String>,
    ) -> f32 {
        match (chunk_project, current_project) {
            (Some(chunk_proj), Some(current_proj)) if chunk_proj == current_proj => 1.0,
            _ => 0.0,
        }
    }

    fn compute_type_bonus(&self, chunk_type: ChunkType, preferred_types: &[ChunkType]) -> f32 {
        if preferred_types.contains(&chunk_type) {
            1.0
        } else {
            0.0
        }
    }
}

/// Lightweight query-document interaction reranker.
///
/// Uses lexical interaction features to approximate cross-encoder style
/// pair scoring while remaining deterministic and offline-friendly.
pub struct CrossEncoderReranker {
    config: RerankerConfig,
}

impl CrossEncoderReranker {
    pub fn new(config: RerankerConfig) -> Self {
        Self { config }
    }

    pub fn rerank(
        &self,
        chunks: Vec<ChunkWithMeta>,
        context: &RerankerContext,
    ) -> Vec<RankedResult> {
        let feature = FeatureReranker::new(self.config.clone());
        let query = context.query_text.as_deref().unwrap_or("");

        let mut results: Vec<RankedResult> = chunks
            .into_iter()
            .map(|chunk| {
                let recency_bonus =
                    feature.compute_recency_bonus(chunk.timestamp_created, context.now_ms);
                let project_bonus =
                    feature.compute_project_bonus(&chunk.project_id, &context.current_project);
                let type_bonus =
                    feature.compute_type_bonus(chunk.chunk_type, &context.preferred_types);
                let cross_encoder_score =
                    interaction_score(query, chunk.text.as_deref().unwrap_or_default());

                let final_score = self.config.rrf_weight * chunk.rrf_score
                    + self.config.cross_encoder_weight * cross_encoder_score
                    + self.config.recency_weight * recency_bonus
                    + self.config.project_weight * project_bonus
                    + self.config.type_weight * type_bonus;

                RankedResult {
                    chunk_id: chunk.chunk_id,
                    final_score,
                    rrf_score: chunk.rrf_score,
                    recency_bonus,
                    project_bonus,
                    type_bonus,
                    cross_encoder_score,
                }
            })
            .collect();

        sort_desc(&mut results);
        results
    }
}

/// Configured reranker with strategy fallback.
pub struct RerankerEngine {
    feature: FeatureReranker,
    #[cfg(feature = "cross-encoder-reranker")]
    cross: CrossEncoderReranker,
    mode: RerankerMode,
}

impl RerankerEngine {
    pub fn new(config: RerankerConfig) -> Self {
        #[cfg(feature = "cross-encoder-reranker")]
        {
            return Self {
                feature: FeatureReranker::new(config.clone()),
                cross: CrossEncoderReranker::new(config.clone()),
                mode: config.mode,
            };
        }

        #[cfg(not(feature = "cross-encoder-reranker"))]
        {
            if config.mode == RerankerMode::CrossEncoder {
                tracing::warn!(
                    "cross-encoder reranker requested but feature 'cross-encoder-reranker' is disabled; falling back to feature reranker"
                );
            }

            Self {
                feature: FeatureReranker::new(config),
                mode: RerankerMode::Feature,
            }
        }
    }

    pub fn mode(&self) -> RerankerMode {
        self.mode
    }

    pub fn rerank(
        &self,
        chunks: Vec<ChunkWithMeta>,
        context: &RerankerContext,
    ) -> Vec<RankedResult> {
        match self.mode {
            RerankerMode::Feature => self.feature.rerank(chunks, context),
            RerankerMode::CrossEncoder => {
                #[cfg(feature = "cross-encoder-reranker")]
                {
                    return self.cross.rerank(chunks, context);
                }

                #[cfg(not(feature = "cross-encoder-reranker"))]
                {
                    self.feature.rerank(chunks, context)
                }
            }
        }
    }
}

fn sort_desc(results: &mut [RankedResult]) {
    results.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn interaction_score(query: &str, text: &str) -> f32 {
    let q_tokens = tokenize(query);
    let d_tokens = tokenize(text);

    if q_tokens.is_empty() || d_tokens.is_empty() {
        return 0.0;
    }

    let q_unique: std::collections::HashSet<_> = q_tokens.iter().collect();
    let d_unique: std::collections::HashSet<_> = d_tokens.iter().collect();

    let overlap = q_unique.intersection(&d_unique).count() as f32;
    let coverage = overlap / q_unique.len() as f32;

    let phrase_score = if q_tokens.len() < 2 {
        0.0
    } else {
        let joined = d_tokens.join(" ");
        let mut matched = 0usize;
        for pair in q_tokens.windows(2) {
            let phrase = format!("{} {}", pair[0], pair[1]);
            if joined.contains(&phrase) {
                matched += 1;
            }
        }
        matched as f32 / (q_tokens.len() - 1) as f32
    };

    let mut freq = 0usize;
    for q in &q_tokens {
        freq += d_tokens.iter().filter(|token| *token == q).count();
    }
    let freq_score = ((freq as f32) / q_tokens.len() as f32).min(3.0) / 3.0;

    (0.6 * coverage + 0.25 * phrase_score + 0.15 * freq_score).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_chunk_id(seed: u8) -> ChunkId {
        ChunkId::from_uuid(Uuid::from_bytes([seed; 16]))
    }

    const MS_PER_DAY: i64 = 1000 * 60 * 60 * 24;

    #[test]
    fn feature_reranker_prefers_recent_when_rrf_is_tied() {
        let reranker = FeatureReranker::default_config();
        let now_ms = 1_000_000_000_000i64;

        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: now_ms - MS_PER_DAY,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: None,
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: now_ms - (30 * MS_PER_DAY),
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: None,
            },
        ];

        let context = RerankerContext {
            current_project: None,
            preferred_types: vec![],
            now_ms,
            query_text: None,
        };

        let results = reranker.rerank(chunks, &context);
        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert!(results[0].recency_bonus > results[1].recency_bonus);
    }

    #[test]
    fn cross_encoder_interaction_prefers_token_and_phrase_matches() {
        let config = RerankerConfig {
            mode: RerankerMode::CrossEncoder,
            rrf_weight: 0.1,
            recency_weight: 0.0,
            recency_half_life_days: 7.0,
            project_weight: 0.0,
            type_weight: 0.0,
            cross_encoder_weight: 1.0,
        };
        let reranker = CrossEncoderReranker::new(config);

        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("hybrid retrieval with cross encoder reranking".to_string()),
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("garbage unrelated sentence".to_string()),
            },
        ];

        let context = RerankerContext::now().with_query("cross encoder retrieval");
        let results = reranker.rerank(chunks, &context);

        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert!(results[0].cross_encoder_score > results[1].cross_encoder_score);
    }

    #[test]
    fn reranker_engine_uses_feature_by_default() {
        let engine = RerankerEngine::new(RerankerConfig::default());
        assert_eq!(engine.mode(), RerankerMode::Feature);
    }

    #[test]
    fn reranker_context_with_project_ignores_empty_values() {
        let context = RerankerContext::now().with_project("");
        assert!(context.current_project.is_none());
    }

    #[test]
    fn interaction_score_is_zero_with_missing_query_or_text() {
        assert_eq!(interaction_score("", "some text"), 0.0);
        assert_eq!(interaction_score("query", ""), 0.0);
    }
}
