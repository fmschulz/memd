//! Content-identity helpers for writer-driven supersession.
//!
//! This module owns the canonicalization step consumed by
//! `PersistentStore::add_chunk_with_lifecycle` and, later, by the
//! conflict-aware add path (Track D) that rewrites near-duplicates into
//! explicit supersession edges.
//!
//! The chunk type lets docs/messages normalize natural-language noise
//! while code/trace chunks preserve punctuation and case for exact
//! identity checks.

use std::collections::HashSet;

use crate::types::{ChunkId, ChunkType};

#[derive(Debug, Clone)]
pub struct NearDuplicateCandidate {
    pub chunk_id: ChunkId,
    pub text: String,
    pub score: f32,
    pub exact: bool,
}

pub(crate) fn canonicalize_for_type(text: &str, kind: ChunkType) -> String {
    match kind {
        ChunkType::Code | ChunkType::Trace => collapse_whitespace(text),
        _ => collapse_whitespace(
            &text
                .chars()
                .map(|ch| {
                    if ch.is_alphanumeric() {
                        ch.to_lowercase().collect::<String>()
                    } else {
                        " ".to_string()
                    }
                })
                .collect::<String>(),
        ),
    }
}

fn collapse_whitespace(text: &str) -> String {
    text.trim().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn trigram_jaccard(left: &str, right: &str) -> f32 {
    if left == right {
        return 1.0;
    }

    let left = trigrams(left);
    let right = trigrams(right);
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let intersection = left.intersection(&right).count() as f32;
    let union = left.union(&right).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn trigrams(text: &str) -> HashSet<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return HashSet::new();
    }
    if chars.len() < 3 {
        return [text.to_string()].into_iter().collect();
    }
    chars
        .windows(3)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_trims_lowercases_and_collapses_whitespace() {
        let out = canonicalize_for_type("  Hello   World\n", ChunkType::Doc);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn canonicalize_docs_strips_punctuation() {
        let out = canonicalize_for_type("Hello, World!", ChunkType::Doc);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn canonicalize_code_preserves_case_and_punctuation() {
        let out = canonicalize_for_type("  Fn::Name ( X )  ", ChunkType::Code);
        assert_eq!(out, "Fn::Name ( X )");
    }

    #[test]
    fn canonicalize_is_stable_on_idempotent_input() {
        let once = canonicalize_for_type("already canonical", ChunkType::Code);
        let twice = canonicalize_for_type(&once, ChunkType::Code);
        assert_eq!(once, twice);
    }

    #[test]
    fn canonicalize_handles_empty_and_whitespace_only() {
        assert_eq!(canonicalize_for_type("", ChunkType::Doc), "");
        assert_eq!(canonicalize_for_type("   \t\n ", ChunkType::Doc), "");
    }

    #[test]
    fn trigram_jaccard_scores_exact_and_related_text() {
        assert_eq!(trigram_jaccard("abc", "abc"), 1.0);
        assert!(trigram_jaccard("retention policy memd", "retention policy memory") > 0.5);
        assert_eq!(trigram_jaccard("abc", "xyz"), 0.0);
    }
}
