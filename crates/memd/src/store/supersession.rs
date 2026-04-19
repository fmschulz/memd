//! Content-identity helpers for writer-driven supersession.
//!
//! This module owns the canonicalization step consumed by
//! `PersistentStore::add_chunk_with_lifecycle` and the conflict-aware
//! add path (Track D) that rewrites near-duplicates into explicit
//! supersession edges.
//!
//! Two layers are exposed:
//! - `canonicalize_text` — unconditional trim + lowercase + collapse
//!   internal whitespace. Used for prose-shaped chunks and as the basis
//!   for the trigram set.
//! - `canonicalize_for_type` — chunk-type-aware variant. For
//!   `ChunkType::Code` we preserve case (identifiers are case-sensitive)
//!   while still trimming + collapsing whitespace; everything else
//!   delegates to `canonicalize_text`.
//!
//! `is_near_duplicate(a, b, threshold)` reports whether two strings share
//! at least `threshold` of their byte-trigrams (Jaccard). The trigram
//! input is always lowercased so similarity is independent of the
//! per-type canonical form.

use crate::types::ChunkType;
use std::collections::HashSet;

/// Lowercase and collapse internal whitespace runs to a single space.
/// Empty / whitespace-only input returns an empty string. (`split_whitespace`
/// implicitly trims, so no separate `.trim()` is needed.)
pub(crate) fn canonicalize_text(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chunk-type-aware canonical form. Code chunks preserve case so that
/// identifiers like `fn Foo()` and `fn foo()` collide correctly under the
/// dedup index; other types fall through to `canonicalize_text`.
pub(crate) fn canonicalize_for_type(text: &str, kind: ChunkType) -> String {
    match kind {
        ChunkType::Code => text.split_whitespace().collect::<Vec<_>>().join(" "),
        _ => canonicalize_text(text),
    }
}

/// Returns `true` when `a` and `b` share at least `threshold` of their
/// byte-trigrams (Jaccard similarity over a lowercased view of each
/// input). Threshold range is the caller's responsibility; this fn does
/// not validate. Used by Track D's `supersede_near_duplicates` flow.
#[allow(dead_code)] // wired in D3
pub(crate) fn is_near_duplicate(a: &str, b: &str, threshold: f32) -> bool {
    jaccard_trigram(a, b) >= threshold
}

/// Public wrapper exposing the underlying similarity score for callers
/// that need to sort or report the value alongside a candidate (e.g.
/// `memory.find_near_duplicates` in D5).
#[allow(dead_code)] // wired in D5
pub(crate) fn jaccard_trigram_score(a: &str, b: &str) -> f32 {
    jaccard_trigram(a, b)
}

#[allow(dead_code)] // wired in D3 / D5
fn jaccard_trigram(a: &str, b: &str) -> f32 {
    let ta = trigram_set(a);
    let tb = trigram_set(b);
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[allow(dead_code)] // wired in D3 / D5
fn trigram_set(s: &str) -> HashSet<[u8; 3]> {
    let lower = s.to_lowercase();
    let bytes = lower.as_bytes();
    let mut out = HashSet::with_capacity(bytes.len().saturating_sub(2));
    for w in bytes.windows(3) {
        out.insert([w[0], w[1], w[2]]);
    }
    out
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
    fn canonicalize_text_strips_trivial_variations() {
        assert_eq!(
            canonicalize_text("  Release freeze begins Thursday. "),
            canonicalize_text("release freeze begins thursday. "),
        );
        assert_eq!(
            canonicalize_text("Release\tfreeze\nbegins Thursday."),
            canonicalize_text("release freeze begins thursday."),
        );
        assert_ne!(
            canonicalize_text("release freeze begins thursday"),
            canonicalize_text("release freeze begins friday"),
        );
    }

    #[test]
    fn canonicalize_preserves_case_sensitive_tokens_in_code_chunks() {
        // Code chunks must NOT lowercase — identifiers are case-sensitive.
        assert_ne!(
            canonicalize_for_type("fn Foo()", ChunkType::Code),
            canonicalize_for_type("fn foo()", ChunkType::Code),
        );
        // But code still trims + collapses whitespace.
        assert_eq!(
            canonicalize_for_type("  fn   Foo()\n", ChunkType::Code),
            "fn Foo()",
        );
        // Non-code path keeps lowercase semantics.
        assert_eq!(
            canonicalize_for_type("fn Foo()", ChunkType::Doc),
            "fn foo()",
        );
    }

    #[test]
    fn is_near_duplicate_trigram_jaccard() {
        // Inserting one short word ("on") gives a real-world trigram
        // Jaccard of ~0.85; we use a slightly loose 0.80 so the test is
        // resilient to small algorithm tweaks (e.g. future canonicalize
        // changes that shift trigram counts by ±1).
        assert!(is_near_duplicate(
            "Release freeze begins Thursday.",
            "Release freeze begins on Thursday.",
            0.80,
        ));
        // Completely unrelated text: trigram Jaccard ≈ 0; any threshold
        // above 0.05 must reject.
        assert!(!is_near_duplicate(
            "release freeze thursday",
            "migration rolled back",
            0.85,
        ));
        // Single-word substitution drops Jaccard to ~0.6; the strict
        // 0.92 threshold (D3 paraphrase tier) must reject.
        assert!(!is_near_duplicate(
            "Release freeze begins Thursday.",
            "Release LOCK begins Thursday.",
            0.92,
        ));
    }

    #[test]
    fn jaccard_trigram_score_is_one_for_identical_inputs() {
        assert!((jaccard_trigram_score("hello world", "hello world") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_trigram_score_is_one_for_two_empty_inputs() {
        // Both-empty short-circuits to 1.0 so the dedup path treats two
        // empty strings as identical rather than dividing by zero.
        assert!((jaccard_trigram_score("", "") - 1.0).abs() < 1e-6);
    }
}
