//! Feature-based reranking for context-aware results.
//!
//! Applies recency, project match, and type preference bonuses
//! to produce final rankings that account for contextual signals.

use crate::types::{ChunkId, ChunkType};

/// Configuration for the feature-based reranker.
#[derive(Debug, Clone)]
pub struct RerankerConfig {
    /// Weight for RRF score (default 1.0)
    pub rrf_weight: f32,
    /// Weight for recency bonus (default 0.1)
    pub recency_weight: f32,
    /// Decay half-life in days for recency (default 7.0)
    pub recency_half_life_days: f32,
    /// Weight for project match bonus (default 0.2)
    pub project_weight: f32,
    /// Weight for type match bonus (default 0.05)
    pub type_weight: f32,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            rrf_weight: 1.0,
            recency_weight: 0.1,
            recency_half_life_days: 7.0,
            project_weight: 0.2,
            type_weight: 0.05,
        }
    }
}

/// Input chunk with metadata for reranking.
#[derive(Debug, Clone)]
pub struct ChunkWithMeta {
    /// Chunk identifier
    pub chunk_id: ChunkId,
    /// RRF score from fusion
    pub rrf_score: f32,
    /// When the chunk was created (Unix milliseconds)
    pub timestamp_created: i64,
    /// Project the chunk belongs to (if any)
    pub project_id: Option<String>,
    /// Type of chunk content
    pub chunk_type: ChunkType,
}

/// Context for reranking (query-time information).
#[derive(Debug, Clone)]
pub struct RerankerContext {
    /// Current project context (for project match bonus)
    pub current_project: Option<String>,
    /// Preferred chunk types (for type match bonus)
    pub preferred_types: Vec<ChunkType>,
    /// Current timestamp in milliseconds (for recency calculation)
    pub now_ms: i64,
}

impl RerankerContext {
    /// Create context with current time.
    pub fn now() -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Self {
            current_project: None,
            preferred_types: Vec::new(),
            now_ms,
        }
    }

    /// Set current project.
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.current_project = Some(project.into());
        self
    }

    /// Set preferred types.
    pub fn with_preferred_types(mut self, types: Vec<ChunkType>) -> Self {
        self.preferred_types = types;
        self
    }
}

/// Result with final score and component breakdown.
#[derive(Debug, Clone)]
pub struct RankedResult {
    /// Chunk identifier
    pub chunk_id: ChunkId,
    /// Final combined score
    pub final_score: f32,
    /// Original RRF score
    pub rrf_score: f32,
    /// Recency bonus contribution
    pub recency_bonus: f32,
    /// Project match bonus contribution
    pub project_bonus: f32,
    /// Type match bonus contribution
    pub type_bonus: f32,
}

/// Feature-based reranker.
///
/// Applies contextual signals to RRF-fused results:
/// - Recency bonus: exponential decay based on chunk age
/// - Project match: boost for chunks in same project
/// - Type preference: boost for preferred chunk types
pub struct FeatureReranker {
    config: RerankerConfig,
}

impl FeatureReranker {
    /// Create a new reranker with the given configuration.
    pub fn new(config: RerankerConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(RerankerConfig::default())
    }

    /// Rerank chunks using feature-based scoring.
    ///
    /// # Formula
    /// ```text
    /// final = rrf_weight * rrf_score
    ///       + recency_weight * recency_bonus
    ///       + project_weight * project_bonus
    ///       + type_weight * type_bonus
    /// ```
    pub fn rerank(&self, chunks: Vec<ChunkWithMeta>, context: &RerankerContext) -> Vec<RankedResult> {
        let mut results: Vec<RankedResult> = chunks
            .into_iter()
            .map(|chunk| {
                let recency_bonus = self.compute_recency_bonus(chunk.timestamp_created, context.now_ms);
                let project_bonus = self.compute_project_bonus(&chunk.project_id, &context.current_project);
                let type_bonus = self.compute_type_bonus(chunk.chunk_type, &context.preferred_types);

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
                }
            })
            .collect();

        // Sort by final score descending
        results.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap_or(std::cmp::Ordering::Equal));

        results
    }

    /// Compute recency bonus using exponential decay.
    ///
    /// recency = exp(-age_days * ln(2) / half_life)
    /// Range: 1.0 (just created) to 0.0 (very old)
    fn compute_recency_bonus(&self, timestamp_created: i64, now_ms: i64) -> f32 {
        let age_ms = (now_ms - timestamp_created).max(0) as f64;
        let age_days = age_ms / (1000.0 * 60.0 * 60.0 * 24.0);

        let decay_rate = std::f64::consts::LN_2 / self.config.recency_half_life_days as f64;
        (-age_days * decay_rate).exp() as f32
    }

    /// Compute project match bonus.
    ///
    /// 1.0 if chunk project matches current project, 0.0 otherwise.
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

    /// Compute type match bonus.
    ///
    /// 1.0 if chunk type is in preferred types, 0.0 otherwise.
    fn compute_type_bonus(&self, chunk_type: ChunkType, preferred_types: &[ChunkType]) -> f32 {
        if preferred_types.contains(&chunk_type) {
            1.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_chunk_id(seed: u8) -> ChunkId {
        let bytes = [seed; 16];
        ChunkId::from_uuid(Uuid::from_bytes(bytes))
    }

    const MS_PER_DAY: i64 = 1000 * 60 * 60 * 24;

    #[test]
    fn test_recency_bonus() {
        let reranker = FeatureReranker::default_config();
        let now_ms = 1_000_000_000_000i64; // Fixed "now"

        // Chunk A: 1 day old
        // Chunk B: 30 days old
        // Same RRF score
        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: now_ms - MS_PER_DAY, // 1 day old
                project_id: None,
                chunk_type: ChunkType::Doc,
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: now_ms - (30 * MS_PER_DAY), // 30 days old
                project_id: None,
                chunk_type: ChunkType::Doc,
            },
        ];

        let context = RerankerContext {
            current_project: None,
            preferred_types: vec![],
            now_ms,
        };

        let results = reranker.rerank(chunks, &context);

        // Chunk A (1 day old) should rank higher
        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert!(results[0].recency_bonus > results[1].recency_bonus);
    }

    #[test]
    fn test_project_match() {
        let reranker = FeatureReranker::default_config();
        let now_ms = 1_000_000_000_000i64;

        // Chunk A: matches current project
        // Chunk B: different project
        // Same RRF score and age
        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: now_ms,
                project_id: Some("my-project".to_string()),
                chunk_type: ChunkType::Doc,
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: now_ms,
                project_id: Some("other-project".to_string()),
                chunk_type: ChunkType::Doc,
            },
        ];

        let context = RerankerContext {
            current_project: Some("my-project".to_string()),
            preferred_types: vec![],
            now_ms,
        };

        let results = reranker.rerank(chunks, &context);

        // Chunk A (project match) should rank higher
        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert_eq!(results[0].project_bonus, 1.0);
        assert_eq!(results[1].project_bonus, 0.0);
    }

    #[test]
    fn test_combined_features() {
        let config = RerankerConfig {
            rrf_weight: 1.0,
            recency_weight: 0.1,
            recency_half_life_days: 7.0,
            project_weight: 0.2,
            type_weight: 0.05,
        };
        let reranker = FeatureReranker::new(config.clone());
        let now_ms = 1_000_000_000_000i64;

        let chunk = ChunkWithMeta {
            chunk_id: make_chunk_id(1),
            rrf_score: 0.5,
            timestamp_created: now_ms, // Just created
            project_id: Some("my-project".to_string()),
            chunk_type: ChunkType::Code,
        };

        let context = RerankerContext {
            current_project: Some("my-project".to_string()),
            preferred_types: vec![ChunkType::Code],
            now_ms,
        };

        let results = reranker.rerank(vec![chunk], &context);
        let result = &results[0];

        // Verify all bonuses are applied
        assert!((result.recency_bonus - 1.0).abs() < 0.001, "Just created should have recency ~1.0");
        assert_eq!(result.project_bonus, 1.0);
        assert_eq!(result.type_bonus, 1.0);

        // Verify final score formula
        let expected = config.rrf_weight * 0.5
            + config.recency_weight * result.recency_bonus
            + config.project_weight * 1.0
            + config.type_weight * 1.0;
        assert!(
            (result.final_score - expected).abs() < 0.001,
            "Final score should match formula"
        );
    }

    #[test]
    fn test_type_preference() {
        let reranker = FeatureReranker::default_config();
        let now_ms = 1_000_000_000_000i64;

        // Code chunk vs Doc chunk, same RRF and age
        let chunks = vec![
            ChunkWithMeta {
                chunk_id: make_chunk_id(1),
                rrf_score: 0.5,
                timestamp_created: now_ms,
                project_id: None,
                chunk_type: ChunkType::Code,
            },
            ChunkWithMeta {
                chunk_id: make_chunk_id(2),
                rrf_score: 0.5,
                timestamp_created: now_ms,
                project_id: None,
                chunk_type: ChunkType::Doc,
            },
        ];

        let context = RerankerContext {
            current_project: None,
            preferred_types: vec![ChunkType::Code],
            now_ms,
        };

        let results = reranker.rerank(chunks, &context);

        // Code chunk should rank higher
        assert_eq!(results[0].chunk_id, make_chunk_id(1));
        assert_eq!(results[0].type_bonus, 1.0);
        assert_eq!(results[1].type_bonus, 0.0);
    }
}
