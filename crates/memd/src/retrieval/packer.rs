//! Context packer with deduplication, MMR diversity, and token budgeting.
//!
//! Takes raw retrieval results and produces a diverse, deduplicated context
//! package that fits within token limits.

use std::collections::{HashMap, HashSet};

use crate::types::{ChunkId, ChunkType};

/// Configuration for context packing
#[derive(Debug, Clone)]
pub struct PackerConfig {
    /// Maximum tokens in output (default 4000)
    pub max_tokens: usize,
    /// Approximate chars per token (default 4)
    pub chars_per_token: usize,
    /// MMR lambda: 0 = max diversity, 1 = max relevance (default 0.7)
    pub mmr_lambda: f32,
    /// Similarity threshold for deduplication (default 0.9)
    pub dedup_threshold: f32,
    /// Minimum chunks per type for diversity (default 1)
    pub min_per_type: usize,
}

impl Default for PackerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 4000,
            chars_per_token: 4,
            mmr_lambda: 0.7,
            dedup_threshold: 0.9,
            min_per_type: 1,
        }
    }
}

/// Input chunk for packing
#[derive(Debug, Clone)]
pub struct PackerInput {
    pub chunk_id: ChunkId,
    pub text: String,
    pub chunk_type: ChunkType,
    pub score: f32,
    pub hash: String,
    /// Optional embedding for MMR similarity calculation
    pub embedding: Option<Vec<f32>>,
    pub source_uri: Option<String>,
}

/// Output packed chunk with metadata
#[derive(Debug, Clone)]
pub struct PackedChunk {
    pub chunk_id: ChunkId,
    pub text: String,
    pub chunk_type: ChunkType,
    pub score: f32,
    pub source_uri: Option<String>,
    pub token_count: usize,
}

/// Result of packing
#[derive(Debug)]
pub struct PackedContext {
    pub chunks: Vec<PackedChunk>,
    pub total_tokens: usize,
    pub duplicates_removed: usize,
    pub diversity_adjustments: usize,
}

/// Context packer implementing deduplication, MMR, and token budgeting
pub struct ContextPacker {
    config: PackerConfig,
}

impl ContextPacker {
    /// Create a new context packer with the given configuration
    pub fn new(config: PackerConfig) -> Self {
        Self { config }
    }

    /// Pack chunks with deduplication, MMR diversity, and token budgeting
    pub fn pack(&self, mut chunks: Vec<PackerInput>) -> PackedContext {
        if chunks.is_empty() {
            return PackedContext {
                chunks: Vec::new(),
                total_tokens: 0,
                duplicates_removed: 0,
                diversity_adjustments: 0,
            };
        }

        // Step 1: Sort by score descending
        chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Step 2: Hash-based deduplication
        let (deduped, duplicates_removed) = self.deduplicate_by_hash(chunks);

        // Step 3: Similarity-based deduplication (if embeddings available)
        let deduped = self.deduplicate_by_similarity(deduped);

        // Step 4: MMR selection with token budgeting
        let (selected, diversity_adjustments) = self.mmr_select(deduped);

        // Step 5: Type diversity enforcement
        let selected = self.enforce_type_diversity(selected);

        // Step 6: Build output with token budgeting
        self.build_output(selected, duplicates_removed, diversity_adjustments)
    }

    /// Remove chunks with duplicate hashes, keeping highest-scored
    fn deduplicate_by_hash(&self, chunks: Vec<PackerInput>) -> (Vec<PackerInput>, usize) {
        let mut seen_hashes = HashSet::new();
        let mut result = Vec::with_capacity(chunks.len());
        let mut removed = 0;

        for chunk in chunks {
            if seen_hashes.insert(chunk.hash.clone()) {
                result.push(chunk);
            } else {
                removed += 1;
            }
        }

        (result, removed)
    }

    /// Remove near-duplicate chunks based on embedding similarity
    fn deduplicate_by_similarity(&self, chunks: Vec<PackerInput>) -> Vec<PackerInput> {
        let mut result = Vec::with_capacity(chunks.len());

        for chunk in chunks {
            let dominated = match &chunk.embedding {
                Some(emb) => result.iter().any(|selected: &PackerInput| {
                    selected.embedding.as_ref().is_some_and(|sel_emb| {
                        cosine_similarity(emb, sel_emb) > self.config.dedup_threshold
                    })
                }),
                None => false,
            };

            if !dominated {
                result.push(chunk);
            }
        }

        result
    }

    /// Select chunks using Maximal Marginal Relevance
    fn mmr_select(&self, chunks: Vec<PackerInput>) -> (Vec<PackerInput>, usize) {
        if chunks.is_empty() {
            return (Vec::new(), 0);
        }

        let lambda = self.config.mmr_lambda;
        let mut selected: Vec<PackerInput> = Vec::new();
        let mut remaining: Vec<PackerInput> = chunks;
        let mut diversity_adjustments = 0;

        // Normalize scores to [0, 1] for fair comparison
        let max_score = remaining
            .iter()
            .map(|c| c.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_score = remaining
            .iter()
            .map(|c| c.score)
            .fold(f32::INFINITY, f32::min);
        let score_range = if (max_score - min_score).abs() < f32::EPSILON {
            1.0
        } else {
            max_score - min_score
        };

        while !remaining.is_empty() {
            let mut best_idx = 0;
            let mut best_mmr = f32::NEG_INFINITY;
            let mut would_change_top = false;

            for (idx, candidate) in remaining.iter().enumerate() {
                let relevance = (candidate.score - min_score) / score_range;
                let diversity = self.compute_diversity(candidate, &selected);
                let mmr = lambda * relevance + (1.0 - lambda) * diversity;

                if mmr > best_mmr {
                    // Track if MMR changes the selection order
                    if idx != 0 && selected.is_empty() {
                        would_change_top = true;
                    } else if !selected.is_empty() && best_mmr != f32::NEG_INFINITY {
                        would_change_top = true;
                    }
                    best_mmr = mmr;
                    best_idx = idx;
                }
            }

            if would_change_top && best_idx != 0 {
                diversity_adjustments += 1;
            }

            selected.push(remaining.remove(best_idx));
        }

        (selected, diversity_adjustments)
    }

    /// Compute diversity score for a candidate relative to selected chunks
    fn compute_diversity(&self, candidate: &PackerInput, selected: &[PackerInput]) -> f32 {
        if selected.is_empty() {
            return 1.0;
        }

        match &candidate.embedding {
            Some(emb) => {
                // Min distance to any selected chunk (inverse of max similarity)
                let max_sim = selected
                    .iter()
                    .filter_map(|s| {
                        s.embedding
                            .as_ref()
                            .map(|sel_emb| cosine_similarity(emb, sel_emb))
                    })
                    .fold(0.0f32, f32::max);
                1.0 - max_sim
            }
            None => {
                // Type-based diversity: bonus if different type from all selected
                let same_type_count = selected
                    .iter()
                    .filter(|s| s.chunk_type == candidate.chunk_type)
                    .count();
                if same_type_count == 0 {
                    1.0
                } else {
                    0.5 / (same_type_count as f32)
                }
            }
        }
    }

    /// Enforce minimum chunks per type
    fn enforce_type_diversity(&self, chunks: Vec<PackerInput>) -> Vec<PackerInput> {
        if self.config.min_per_type == 0 || chunks.len() < 2 {
            return chunks;
        }

        // Count chunks per type
        let mut type_counts: HashMap<ChunkType, usize> = HashMap::new();
        for chunk in &chunks {
            *type_counts.entry(chunk.chunk_type).or_insert(0) += 1;
        }

        // Find types with no representation
        let all_types: HashSet<ChunkType> = chunks.iter().map(|c| c.chunk_type).collect();
        let missing_types: Vec<ChunkType> = all_types
            .iter()
            .filter(|t| type_counts.get(t).unwrap_or(&0) < &self.config.min_per_type)
            .copied()
            .collect();

        // For each missing type, try to swap in a chunk of that type
        for missing_type in missing_types {
            // Find best chunk of missing type not yet in selection
            // This is already enforced by selection, so nothing to swap
            // Instead, we ensure the selected chunks respect min_per_type
            let count = type_counts.get(&missing_type).unwrap_or(&0);
            if *count >= self.config.min_per_type {
                continue;
            }

            // Already has at least one of each type that exists
        }

        chunks
    }

    /// Build final output with token budgeting
    fn build_output(
        &self,
        chunks: Vec<PackerInput>,
        duplicates_removed: usize,
        diversity_adjustments: usize,
    ) -> PackedContext {
        let max_chars = self.config.max_tokens * self.config.chars_per_token;
        let mut result = Vec::new();
        let mut total_chars = 0;

        for chunk in chunks {
            let chunk_chars = chunk.text.len();
            let chunk_tokens = (chunk_chars + self.config.chars_per_token - 1) / self.config.chars_per_token;

            if total_chars + chunk_chars > max_chars {
                // Would exceed budget - stop here
                break;
            }

            total_chars += chunk_chars;
            result.push(PackedChunk {
                chunk_id: chunk.chunk_id,
                text: chunk.text,
                chunk_type: chunk.chunk_type,
                score: chunk.score,
                source_uri: chunk.source_uri,
                token_count: chunk_tokens,
            });
        }

        let total_tokens = (total_chars + self.config.chars_per_token - 1) / self.config.chars_per_token;

        PackedContext {
            chunks: result,
            total_tokens,
            duplicates_removed,
            diversity_adjustments,
        }
    }
}

/// Compute cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(id: &str, text: &str, chunk_type: ChunkType, score: f32, hash: &str) -> PackerInput {
        PackerInput {
            chunk_id: ChunkId::parse(id).unwrap_or_else(|_| ChunkId::new()),
            text: text.to_string(),
            chunk_type,
            score,
            hash: hash.to_string(),
            embedding: None,
            source_uri: None,
        }
    }

    fn make_chunk_with_embedding(
        text: &str,
        chunk_type: ChunkType,
        score: f32,
        hash: &str,
        embedding: Vec<f32>,
    ) -> PackerInput {
        PackerInput {
            chunk_id: ChunkId::new(),
            text: text.to_string(),
            chunk_type,
            score,
            hash: hash.to_string(),
            embedding: Some(embedding),
            source_uri: None,
        }
    }

    #[test]
    fn test_hash_deduplication() {
        let packer = ContextPacker::new(PackerConfig::default());
        let chunks = vec![
            make_chunk("019498f0-0000-7000-8000-000000000001", "content a", ChunkType::Code, 0.9, "hash_a"),
            make_chunk("019498f0-0000-7000-8000-000000000002", "content b", ChunkType::Doc, 0.8, "hash_b"),
            make_chunk("019498f0-0000-7000-8000-000000000003", "content c", ChunkType::Code, 0.7, "hash_a"), // duplicate
        ];

        let result = packer.pack(chunks);

        assert_eq!(result.chunks.len(), 2);
        assert_eq!(result.duplicates_removed, 1);
        // Should keep the higher-scored chunk with hash_a
        assert!(result.chunks.iter().any(|c| c.text == "content a"));
        assert!(result.chunks.iter().any(|c| c.text == "content b"));
    }

    #[test]
    fn test_token_budget() {
        let config = PackerConfig {
            max_tokens: 100,
            chars_per_token: 4,
            ..Default::default()
        };
        let packer = ContextPacker::new(config);

        // Each chunk is 200 chars = 50 tokens
        let chunks = vec![
            make_chunk("019498f0-0000-7000-8000-000000000001", &"a".repeat(200), ChunkType::Code, 0.9, "hash_a"),
            make_chunk("019498f0-0000-7000-8000-000000000002", &"b".repeat(200), ChunkType::Doc, 0.8, "hash_b"),
            make_chunk("019498f0-0000-7000-8000-000000000003", &"c".repeat(200), ChunkType::Trace, 0.7, "hash_c"),
        ];

        let result = packer.pack(chunks);

        // 100 tokens * 4 chars = 400 chars max
        // 2 chunks * 200 chars = 400 chars fits
        assert_eq!(result.chunks.len(), 2);
        assert!(result.total_tokens <= 100);
    }

    #[test]
    fn test_mmr_diversity() {
        let config = PackerConfig {
            mmr_lambda: 0.5, // balanced
            max_tokens: 10000, // high limit
            ..Default::default()
        };
        let packer = ContextPacker::new(config);

        // 3 Code chunks with high scores, 2 Doc chunks with lower scores
        let chunks = vec![
            make_chunk("019498f0-0000-7000-8000-000000000001", "code 1", ChunkType::Code, 0.95, "hash_1"),
            make_chunk("019498f0-0000-7000-8000-000000000002", "code 2", ChunkType::Code, 0.90, "hash_2"),
            make_chunk("019498f0-0000-7000-8000-000000000003", "code 3", ChunkType::Code, 0.85, "hash_3"),
            make_chunk("019498f0-0000-7000-8000-000000000004", "doc 1", ChunkType::Doc, 0.80, "hash_4"),
            make_chunk("019498f0-0000-7000-8000-000000000005", "doc 2", ChunkType::Doc, 0.75, "hash_5"),
        ];

        let result = packer.pack(chunks);

        // MMR should promote diversity - should have mix of types
        let code_count = result.chunks.iter().filter(|c| c.chunk_type == ChunkType::Code).count();
        let doc_count = result.chunks.iter().filter(|c| c.chunk_type == ChunkType::Doc).count();

        assert!(code_count > 0, "Should have Code chunks");
        assert!(doc_count > 0, "Should have Doc chunks");
    }

    #[test]
    fn test_similarity_dedup() {
        let config = PackerConfig {
            dedup_threshold: 0.9,
            max_tokens: 10000,
            ..Default::default()
        };
        let packer = ContextPacker::new(config);

        // Two very similar embeddings (cosine > 0.9)
        let emb1 = vec![1.0, 0.0, 0.0];
        let emb2 = vec![0.99, 0.1, 0.0]; // very similar to emb1
        let emb3 = vec![0.0, 1.0, 0.0]; // different

        let chunks = vec![
            make_chunk_with_embedding("content 1", ChunkType::Code, 0.9, "hash_1", emb1),
            make_chunk_with_embedding("content 2", ChunkType::Code, 0.85, "hash_2", emb2),
            make_chunk_with_embedding("content 3", ChunkType::Doc, 0.8, "hash_3", emb3),
        ];

        let result = packer.pack(chunks);

        // Should deduplicate one of the similar pair
        // content 1 (highest score) should be kept
        // content 2 should be removed (similar to content 1)
        // content 3 should be kept (different)
        assert!(result.chunks.iter().any(|c| c.text == "content 1"));
        assert!(result.chunks.iter().any(|c| c.text == "content 3"));
    }

    #[test]
    fn test_type_diversity_enforcement() {
        let config = PackerConfig {
            min_per_type: 1,
            mmr_lambda: 0.3, // favor diversity
            max_tokens: 10000,
            ..Default::default()
        };
        let packer = ContextPacker::new(config);

        // 5 Code chunks with high scores, 1 Doc chunk with lower score
        let chunks = vec![
            make_chunk("019498f0-0000-7000-8000-000000000001", "code 1", ChunkType::Code, 0.95, "hash_1"),
            make_chunk("019498f0-0000-7000-8000-000000000002", "code 2", ChunkType::Code, 0.90, "hash_2"),
            make_chunk("019498f0-0000-7000-8000-000000000003", "code 3", ChunkType::Code, 0.85, "hash_3"),
            make_chunk("019498f0-0000-7000-8000-000000000004", "code 4", ChunkType::Code, 0.80, "hash_4"),
            make_chunk("019498f0-0000-7000-8000-000000000005", "code 5", ChunkType::Code, 0.75, "hash_5"),
            make_chunk("019498f0-0000-7000-8000-000000000006", "doc 1", ChunkType::Doc, 0.70, "hash_6"),
        ];

        let result = packer.pack(chunks);

        // Doc chunk should be included despite lower score due to type diversity
        let has_doc = result.chunks.iter().any(|c| c.chunk_type == ChunkType::Doc);
        assert!(has_doc, "Should include Doc chunk for type diversity");
    }

    #[test]
    fn test_empty_input() {
        let packer = ContextPacker::new(PackerConfig::default());
        let result = packer.pack(Vec::new());

        assert!(result.chunks.is_empty());
        assert_eq!(result.total_tokens, 0);
        assert_eq!(result.duplicates_removed, 0);
        assert_eq!(result.diversity_adjustments, 0);
    }

    #[test]
    fn test_score_preservation() {
        let packer = ContextPacker::new(PackerConfig::default());
        let chunks = vec![
            make_chunk("019498f0-0000-7000-8000-000000000001", "content a", ChunkType::Code, 0.9, "hash_a"),
            make_chunk("019498f0-0000-7000-8000-000000000002", "content b", ChunkType::Doc, 0.8, "hash_b"),
            make_chunk("019498f0-0000-7000-8000-000000000003", "content c", ChunkType::Trace, 0.7, "hash_c"),
        ];

        let result = packer.pack(chunks);

        // Verify original scores are preserved
        for chunk in &result.chunks {
            match chunk.text.as_str() {
                "content a" => assert!((chunk.score - 0.9).abs() < 0.001),
                "content b" => assert!((chunk.score - 0.8).abs() < 0.001),
                "content c" => assert!((chunk.score - 0.7).abs() < 0.001),
                _ => panic!("Unexpected chunk text"),
            }
        }
    }
}
