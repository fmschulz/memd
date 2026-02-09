//! RRF (Reciprocal Rank Fusion) for combining search results.
//!
//! Merges ranked lists from multiple retrieval sources (dense, sparse)
//! into a unified ranking using the RRF algorithm.

use std::collections::HashMap;

use crate::types::ChunkId;

/// Source of a fusion candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionSource {
    /// Dense (embedding-based) search
    Dense,
    /// Sparse (keyword/BM25) search
    Sparse,
}

/// Candidate for fusion from a single source.
#[derive(Debug, Clone)]
pub struct FusionCandidate {
    /// Chunk identifier
    pub chunk_id: ChunkId,
    /// Which retrieval source produced this candidate
    pub source: FusionSource,
    /// 1-indexed rank in the source result list
    pub rank: usize,
    /// Original score from the source system
    pub source_score: f32,
}

/// Configuration for RRF fusion.
#[derive(Debug, Clone)]
pub struct RrfConfig {
    /// RRF constant k (default 60, standard value from literature)
    pub k: f32,
    /// Weight for dense source results (default 1.0)
    pub dense_weight: f32,
    /// Weight for sparse source results (default 1.0)
    pub sparse_weight: f32,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            dense_weight: 1.0,
            sparse_weight: 1.0,
        }
    }
}

/// Result after RRF fusion.
#[derive(Debug, Clone)]
pub struct FusedResult {
    /// Chunk identifier
    pub chunk_id: ChunkId,
    /// Combined RRF score
    pub rrf_score: f32,
    /// Rank in dense results (if present)
    pub dense_rank: Option<usize>,
    /// Rank in sparse results (if present)
    pub sparse_rank: Option<usize>,
}

/// Internal tracking for fusion computation.
struct FusionAccumulator {
    rrf_score: f32,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
}

/// Reciprocal Rank Fusion combiner.
///
/// Merges results from dense and sparse search using the RRF formula:
/// `score = sum over sources: weight / (k + rank)`
pub struct RrfFusion {
    config: RrfConfig,
}

impl RrfFusion {
    /// Create a new RRF fusion instance with the given configuration.
    pub fn new(config: RrfConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration (k=60, equal weights).
    pub fn default_config() -> Self {
        Self::new(RrfConfig::default())
    }

    /// Fuse candidates from multiple sources using RRF.
    ///
    /// # Algorithm
    /// 1. Group candidates by chunk_id
    /// 2. For each chunk, compute RRF score:
    ///    - If present in dense at rank r: add dense_weight / (k + r)
    ///    - If present in sparse at rank r: add sparse_weight / (k + r)
    /// 3. Sort by RRF score descending
    pub fn fuse(&self, candidates: Vec<FusionCandidate>) -> Vec<FusedResult> {
        let mut accumulators: HashMap<ChunkId, FusionAccumulator> = HashMap::new();

        for candidate in candidates {
            let weight = match candidate.source {
                FusionSource::Dense => self.config.dense_weight,
                FusionSource::Sparse => self.config.sparse_weight,
            };

            let contribution = weight / (self.config.k + candidate.rank as f32);

            let acc = accumulators
                .entry(candidate.chunk_id.clone())
                .or_insert_with(|| FusionAccumulator {
                    rrf_score: 0.0,
                    dense_rank: None,
                    sparse_rank: None,
                });

            acc.rrf_score += contribution;

            match candidate.source {
                FusionSource::Dense => acc.dense_rank = Some(candidate.rank),
                FusionSource::Sparse => acc.sparse_rank = Some(candidate.rank),
            }
        }

        let mut results: Vec<FusedResult> = accumulators
            .into_iter()
            .map(|(chunk_id, acc)| FusedResult {
                chunk_id,
                rrf_score: acc.rrf_score,
                dense_rank: acc.dense_rank,
                sparse_rank: acc.sparse_rank,
            })
            .collect();

        // Sort by RRF score descending
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
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

    #[test]
    fn test_rrf_single_source() {
        let fusion = RrfFusion::default_config();

        let candidates = vec![
            FusionCandidate {
                chunk_id: make_chunk_id(1),
                source: FusionSource::Dense,
                rank: 1,
                source_score: 0.9,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(2),
                source: FusionSource::Dense,
                rank: 2,
                source_score: 0.8,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(3),
                source: FusionSource::Dense,
                rank: 3,
                source_score: 0.7,
            },
        ];

        let results = fusion.fuse(candidates);

        assert_eq!(results.len(), 3);
        // RRF score = 1/(60+rank), so rank 1 > rank 2 > rank 3
        assert!(results[0].rrf_score > results[1].rrf_score);
        assert!(results[1].rrf_score > results[2].rrf_score);
        // All from dense only
        assert!(results[0].dense_rank.is_some());
        assert!(results[0].sparse_rank.is_none());
    }

    #[test]
    fn test_rrf_both_sources() {
        let fusion = RrfFusion::default_config();

        // Chunk A: dense rank 1, sparse rank 3
        // Chunk B: dense rank 2, sparse rank 1
        // Chunk C: dense only rank 3
        let candidates = vec![
            FusionCandidate {
                chunk_id: make_chunk_id(1), // A
                source: FusionSource::Dense,
                rank: 1,
                source_score: 0.9,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(2), // B
                source: FusionSource::Dense,
                rank: 2,
                source_score: 0.8,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(3), // C
                source: FusionSource::Dense,
                rank: 3,
                source_score: 0.7,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(1), // A
                source: FusionSource::Sparse,
                rank: 3,
                source_score: 5.0,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(2), // B
                source: FusionSource::Sparse,
                rank: 1,
                source_score: 8.0,
            },
        ];

        let results = fusion.fuse(candidates);

        assert_eq!(results.len(), 3);

        // B should be first (dense rank 2 + sparse rank 1)
        // A should be second (dense rank 1 + sparse rank 3)
        // C should be last (dense rank 3 only)
        let b_result = results
            .iter()
            .find(|r| r.chunk_id == make_chunk_id(2))
            .unwrap();
        let a_result = results
            .iter()
            .find(|r| r.chunk_id == make_chunk_id(1))
            .unwrap();
        let c_result = results
            .iter()
            .find(|r| r.chunk_id == make_chunk_id(3))
            .unwrap();

        assert!(
            b_result.rrf_score > a_result.rrf_score,
            "B should rank higher than A"
        );
        assert!(
            a_result.rrf_score > c_result.rrf_score,
            "A should rank higher than C"
        );
    }

    #[test]
    fn test_rrf_deduplication() {
        let fusion = RrfFusion::default_config();

        // Same chunk from both sources
        let candidates = vec![
            FusionCandidate {
                chunk_id: make_chunk_id(1),
                source: FusionSource::Dense,
                rank: 1,
                source_score: 0.9,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(1),
                source: FusionSource::Sparse,
                rank: 2,
                source_score: 5.0,
            },
        ];

        let results = fusion.fuse(candidates);

        // Should be deduplicated to single entry
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].dense_rank, Some(1));
        assert_eq!(results[0].sparse_rank, Some(2));

        // Score should be sum of both contributions
        let expected = 1.0 / (60.0 + 1.0) + 1.0 / (60.0 + 2.0);
        assert!((results[0].rrf_score - expected).abs() < 0.0001);
    }

    #[test]
    fn test_rrf_weights() {
        let config = RrfConfig {
            k: 60.0,
            dense_weight: 2.0,
            sparse_weight: 1.0,
        };
        let fusion = RrfFusion::new(config);

        // Two chunks, each appearing in only one source at same rank
        let candidates = vec![
            FusionCandidate {
                chunk_id: make_chunk_id(1), // Dense only
                source: FusionSource::Dense,
                rank: 1,
                source_score: 0.9,
            },
            FusionCandidate {
                chunk_id: make_chunk_id(2), // Sparse only
                source: FusionSource::Sparse,
                rank: 1,
                source_score: 5.0,
            },
        ];

        let results = fusion.fuse(candidates);

        let dense_result = results
            .iter()
            .find(|r| r.chunk_id == make_chunk_id(1))
            .unwrap();
        let sparse_result = results
            .iter()
            .find(|r| r.chunk_id == make_chunk_id(2))
            .unwrap();

        // Dense should have double the score due to weight
        assert!(
            dense_result.rrf_score > sparse_result.rrf_score,
            "Dense should rank higher with 2x weight"
        );
        assert!(
            (dense_result.rrf_score / sparse_result.rrf_score - 2.0).abs() < 0.0001,
            "Dense score should be 2x sparse score"
        );
    }
}
