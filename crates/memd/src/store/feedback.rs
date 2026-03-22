//! Feedback capture and score adjustment utilities.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{ChunkId, MemoryChunk, TenantId};

/// User feedback label for retrieved chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelevanceLabel {
    Relevant,
    Irrelevant,
}

impl RelevanceLabel {
    fn polarity(self) -> f32 {
        match self {
            Self::Relevant => 1.0,
            Self::Irrelevant => -1.0,
        }
    }
}

/// Persisted feedback event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub tenant_id: TenantId,
    pub query: String,
    pub chunk_id: ChunkId,
    pub relevance: RelevanceLabel,
    pub timestamp_ms: i64,
}

impl FeedbackEntry {
    pub fn new(
        tenant_id: TenantId,
        query: impl Into<String>,
        chunk_id: ChunkId,
        relevance: RelevanceLabel,
        timestamp_ms: i64,
    ) -> Self {
        Self {
            tenant_id,
            query: normalize_query(&query.into()),
            chunk_id,
            relevance,
            timestamp_ms,
        }
    }
}

/// Adjustment behavior for feedback-aware reranking.
#[derive(Debug, Clone)]
pub struct FeedbackConfig {
    /// Exponential decay half-life in hours for old feedback.
    pub decay_half_life_hours: u32,
    /// Minimum events needed before a chunk receives any score adjustment.
    pub min_samples: usize,
    /// Maximum absolute score delta contributed by feedback.
    pub max_adjustment: f32,
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        Self {
            decay_half_life_hours: 72,
            min_samples: 2,
            max_adjustment: 0.25,
        }
    }
}

/// Normalize query text for stable feedback matching.
pub fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Apply feedback-derived score adjustments with decay and safety caps.
pub fn apply_feedback_scores(
    mut scored: Vec<(MemoryChunk, f32)>,
    query: &str,
    feedback_entries: &[FeedbackEntry],
    now_ms: i64,
    config: &FeedbackConfig,
) -> Vec<(MemoryChunk, f32)> {
    if scored.is_empty() {
        return scored;
    }

    let normalized_query = normalize_query(query);
    if normalized_query.is_empty() {
        return scored;
    }

    let half_life_ms = (config.decay_half_life_hours as i64)
        .saturating_mul(60)
        .saturating_mul(60)
        .saturating_mul(1000)
        .max(1) as f64;

    #[derive(Default)]
    struct Aggregate {
        weighted_sum: f64,
        total_weight: f64,
        samples: usize,
    }

    let mut by_chunk: HashMap<String, Aggregate> = HashMap::new();
    for entry in feedback_entries {
        if normalize_query(&entry.query) != normalized_query {
            continue;
        }

        let age_ms = (now_ms - entry.timestamp_ms).max(0) as f64;
        let decay = (-age_ms * std::f64::consts::LN_2 / half_life_ms).exp();

        let aggregate = by_chunk.entry(entry.chunk_id.to_string()).or_default();
        aggregate.weighted_sum += entry.relevance.polarity() as f64 * decay;
        aggregate.total_weight += decay;
        aggregate.samples += 1;
    }

    for (chunk, score) in &mut scored {
        let Some(aggregate) = by_chunk.get(&chunk.chunk_id.to_string()) else {
            continue;
        };
        if aggregate.samples < config.min_samples || aggregate.total_weight <= 0.0 {
            continue;
        }

        let signal = (aggregate.weighted_sum / aggregate.total_weight).clamp(-1.0, 1.0) as f32;
        *score += config.max_adjustment * signal;
    }

    scored.sort_by(|(a_chunk, a_score), (b_chunk, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b_chunk.timestamp_created.cmp(&a_chunk.timestamp_created))
    });

    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;

    fn make_tenant() -> TenantId {
        TenantId::new("feedback_test").expect("valid tenant")
    }

    fn make_chunk(text: &str) -> MemoryChunk {
        MemoryChunk::new(make_tenant(), text, ChunkType::Doc)
    }

    fn now_ms() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn normalize_query_collapses_whitespace_and_case() {
        assert_eq!(normalize_query("  Hello   WORLD "), "hello world");
    }

    #[test]
    fn relevant_feedback_boosts_after_min_samples() {
        let chunk_a = make_chunk("alpha");
        let chunk_b = make_chunk("beta");
        let query = "test query";
        let t = now_ms();

        let entries = vec![
            FeedbackEntry::new(
                chunk_a.tenant_id.clone(),
                query,
                chunk_a.chunk_id.clone(),
                RelevanceLabel::Relevant,
                t,
            ),
            FeedbackEntry::new(
                chunk_a.tenant_id.clone(),
                query,
                chunk_a.chunk_id.clone(),
                RelevanceLabel::Relevant,
                t,
            ),
        ];

        let ranked = apply_feedback_scores(
            vec![(chunk_a.clone(), 0.5), (chunk_b.clone(), 0.5)],
            query,
            &entries,
            t,
            &FeedbackConfig::default(),
        );

        assert_eq!(ranked[0].0.chunk_id, chunk_a.chunk_id);
        assert!(ranked[0].1 > ranked[1].1);
    }

    #[test]
    fn min_samples_prevents_single_vote_instability() {
        let chunk = make_chunk("alpha");
        let query = "test";
        let t = now_ms();

        let entries = vec![FeedbackEntry::new(
            chunk.tenant_id.clone(),
            query,
            chunk.chunk_id.clone(),
            RelevanceLabel::Irrelevant,
            t,
        )];

        let ranked = apply_feedback_scores(
            vec![(chunk.clone(), 0.9)],
            query,
            &entries,
            t,
            &FeedbackConfig::default(),
        );

        assert!((ranked[0].1 - 0.9).abs() < 1e-6);
    }

    #[test]
    fn decay_downweights_old_feedback() {
        let chunk = make_chunk("alpha");
        let query = "test";
        let t = now_ms();

        let entries = vec![
            FeedbackEntry::new(
                chunk.tenant_id.clone(),
                query,
                chunk.chunk_id.clone(),
                RelevanceLabel::Irrelevant,
                t - 30 * 24 * 60 * 60 * 1000,
            ),
            FeedbackEntry::new(
                chunk.tenant_id.clone(),
                query,
                chunk.chunk_id.clone(),
                RelevanceLabel::Relevant,
                t,
            ),
            FeedbackEntry::new(
                chunk.tenant_id.clone(),
                query,
                chunk.chunk_id.clone(),
                RelevanceLabel::Relevant,
                t,
            ),
        ];

        let ranked = apply_feedback_scores(
            vec![(chunk.clone(), 0.4)],
            query,
            &entries,
            t,
            &FeedbackConfig::default(),
        );

        assert!(ranked[0].1 > 0.4);
    }
}
