//! Feature and cross-encoder reranking for context-aware retrieval.
//!
//! Applies recency, project/type preferences, and optional query-document
//! interaction scoring to produce final rankings.

#[cfg(feature = "cross-encoder-reranker")]
use super::onnx_cross_encoder;
use crate::types::{ChunkId, ChunkType};
use std::collections::HashSet;

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
    /// Weight for lightweight query-text lexical match bonus.
    pub query_text_weight: f32,
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
            query_text_weight: 0.12,
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
                let query_text_bonus = self.compute_query_text_bonus(
                    context.query_text.as_deref().unwrap_or_default(),
                    chunk.text.as_deref(),
                );
                let final_score = self.config.rrf_weight * chunk.rrf_score
                    + self.config.recency_weight * recency_bonus
                    + self.config.project_weight * project_bonus
                    + self.config.type_weight * type_bonus
                    + self.config.query_text_weight * query_text_bonus;

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
        // Floor the half-life to a tiny positive value so a configured
        // half-life of 0 cannot produce inf/NaN (LN_2 / 0 -> inf, and
        // 0 * inf -> NaN at age 0), which would make ranking nondeterministic.
        let half_life_days = (self.config.recency_half_life_days as f64).max(f64::EPSILON);
        let decay_rate = std::f64::consts::LN_2 / half_life_days;
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

    fn compute_query_text_bonus(&self, query: &str, text: Option<&str>) -> f32 {
        let Some(text) = text else {
            return 0.0;
        };
        if query.trim().is_empty() || text.trim().is_empty() {
            return 0.0;
        }

        let query_tokens = signal_query_tokens(query);
        if query_tokens.is_empty() {
            return 0.0;
        }

        let text_tokens = ascii_tokens(text);
        if text_tokens.is_empty() {
            return 0.0;
        }

        let query_unique: HashSet<&str> = query_tokens.iter().map(String::as_str).collect();
        let text_unique: HashSet<&str> = text_tokens.iter().map(String::as_str).collect();
        let overlap = query_unique.intersection(&text_unique).count() as f32;
        let keyword_score = overlap / query_unique.len() as f32;

        let query_bigrams = bigram_set(&query_tokens);
        let text_bigrams = bigram_set(&text_tokens);
        let bigram_score = if query_bigrams.is_empty() {
            0.0
        } else {
            query_bigrams.intersection(&text_bigrams).count() as f32 / query_bigrams.len() as f32
        };

        let text_norm = text_tokens.join(" ");
        let phrase_score = if query_phrases(query, &query_tokens)
            .iter()
            .any(|phrase| text_norm.contains(phrase))
        {
            1.0
        } else {
            0.0
        };

        let query_numbers: HashSet<&str> = query_tokens
            .iter()
            .filter(|token| has_ascii_digit(token))
            .map(String::as_str)
            .collect();
        let numeric_score = if query_numbers.is_empty() {
            0.0
        } else {
            query_numbers.intersection(&text_unique).count() as f32 / query_numbers.len() as f32
        };

        (0.60 * keyword_score + 0.20 * bigram_score + 0.10 * phrase_score + 0.10 * numeric_score)
            .clamp(0.0, 1.0)
    }
}

/// Query-document interaction reranker.
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
        let cross_scores = cross_encoder_scores(query, &chunks);

        let mut results: Vec<RankedResult> = chunks
            .into_iter()
            .zip(cross_scores)
            .map(|chunk| {
                let (chunk, cross_encoder_score) = chunk;
                let recency_bonus =
                    feature.compute_recency_bonus(chunk.timestamp_created, context.now_ms);
                let project_bonus =
                    feature.compute_project_bonus(&chunk.project_id, &context.current_project);
                let type_bonus =
                    feature.compute_type_bonus(chunk.chunk_type, &context.preferred_types);

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
            let mut mode = config.mode;
            if mode == RerankerMode::CrossEncoder
                && !cfg!(test)
                && !onnx_cross_encoder::is_available()
            {
                tracing::warn!(
                    "cross-encoder reranker requested but ONNX scorer is unavailable; falling back to feature reranker"
                );
                mode = RerankerMode::Feature;
            }
            return Self {
                feature: FeatureReranker::new(config.clone()),
                cross: CrossEncoderReranker::new(config.clone()),
                mode,
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

fn ascii_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn signal_query_tokens(query: &str) -> Vec<String> {
    ascii_tokens(query)
        .into_iter()
        .filter(|token| has_ascii_digit(token) || (token.len() >= 3 && !is_query_stopword(token)))
        .collect()
}

const QUERY_TEXT_STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "for",
    "from",
    "had",
    "has",
    "have",
    "he",
    "her",
    "hers",
    "him",
    "his",
    "how",
    "i",
    "in",
    "into",
    "is",
    "it",
    "its",
    "list",
    "me",
    "of",
    "on",
    "or",
    "our",
    "she",
    "summarize",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "they",
    "this",
    "to",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
];

fn is_query_stopword(token: &str) -> bool {
    QUERY_TEXT_STOPWORDS.contains(&token)
}

fn has_ascii_digit(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_digit())
}

fn bigram_set(tokens: &[String]) -> HashSet<(String, String)> {
    tokens
        .windows(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect()
}

fn query_phrases(query: &str, signal_tokens: &[String]) -> Vec<String> {
    let mut phrases = Vec::new();
    let parts: Vec<&str> = query.split('"').collect();
    for quoted in parts.iter().skip(1).step_by(2) {
        let quoted_tokens = signal_query_tokens(quoted);
        if quoted_tokens.len() >= 2 {
            phrases.push(quoted_tokens.join(" "));
        }
    }
    if signal_tokens.len() >= 3 {
        phrases.push(signal_tokens.join(" "));
    }
    phrases
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

fn cross_encoder_scores(query: &str, chunks: &[ChunkWithMeta]) -> Vec<f32> {
    if chunks.is_empty() {
        return Vec::new();
    }
    if cfg!(test) {
        return chunks
            .iter()
            .map(|chunk| interaction_score(query, chunk.text.as_deref().unwrap_or_default()))
            .collect();
    }
    #[cfg(feature = "cross-encoder-reranker")]
    {
        let docs: Vec<String> = chunks
            .iter()
            .map(|chunk| chunk.text.clone().unwrap_or_default())
            .collect();
        match onnx_cross_encoder::score_pairs(query, &docs) {
            Ok(scores) if scores.len() == chunks.len() => return scores,
            Ok(_) => {
                tracing::warn!(
                    "cross-encoder scorer returned mismatched score count; using lexical fallback"
                );
            }
            Err(err) => {
                tracing::warn!(error = %err, "cross-encoder scorer failed; using lexical fallback");
            }
        }
    }

    chunks
        .iter()
        .map(|chunk| interaction_score(query, chunk.text.as_deref().unwrap_or_default()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_chunk_id(seed: u8) -> ChunkId {
        ChunkId::from_uuid(Uuid::from_bytes([seed; 16]))
    }

    const MS_PER_DAY: i64 = 1000 * 60 * 60 * 24;

    fn query_text_only_config() -> RerankerConfig {
        RerankerConfig {
            rrf_weight: 0.0,
            recency_weight: 0.0,
            project_weight: 0.0,
            type_weight: 0.0,
            query_text_weight: 1.0,
            cross_encoder_weight: 0.0,
            ..Default::default()
        }
    }

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
    fn feature_reranker_prefers_query_text_overlap_when_other_features_tie() {
        let reranker = FeatureReranker::new(query_text_only_config());
        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("Maria moved to Boston in 2021 after the semester ended".to_string()),
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("The group discussed unrelated travel plans".to_string()),
            },
        ];

        let context = RerankerContext::now().with_query("Where did Maria move in 2021?");
        let results = reranker.rerank(chunks, &context);

        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert!(results[0].final_score > results[1].final_score);
    }

    #[test]
    fn query_text_bonus_ignores_stopword_only_overlap() {
        let reranker = FeatureReranker::new(RerankerConfig {
            rrf_weight: 1.0,
            recency_weight: 0.0,
            project_weight: 0.0,
            type_weight: 0.0,
            query_text_weight: 1.0,
            cross_encoder_weight: 0.0,
            ..Default::default()
        });
        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.6,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("unrelated content".to_string()),
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("the and was for did".to_string()),
            },
        ];

        let context = RerankerContext::now().with_query("the and was for did");
        let results = reranker.rerank(chunks, &context);

        assert_eq!(results[0].chunk_id, make_chunk_id(1));
    }

    #[test]
    fn query_text_bonus_counts_numeric_matches() {
        let reranker = FeatureReranker::default_config();

        let with_number = reranker.compute_query_text_bonus(
            "What happened in 2021?",
            Some("They moved apartments in 2021"),
        );
        let without_number =
            reranker.compute_query_text_bonus("What happened in 2021?", Some("They moved later"));

        assert!(with_number > without_number);
        assert!(with_number > 0.0);
    }

    #[test]
    fn query_text_bonus_is_zero_without_chunk_text() {
        let reranker = FeatureReranker::default_config();

        assert_eq!(
            reranker.compute_query_text_bonus("Where did Maria move?", None),
            0.0
        );
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
            query_text_weight: 0.0,
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
