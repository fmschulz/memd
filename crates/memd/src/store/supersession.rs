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
//! at least `threshold` of their **padded char-trigrams** (Jaccard) over
//! a lowercased view of each input. Padding follows the `pg_trgm`
//! convention (two leading + one trailing space) so that very short
//! inputs still produce distinguishing trigrams — without padding,
//! `"a"` vs `"b"` would both yield empty sets and be reported as
//! identical. Iterating over `chars()` rather than raw bytes keeps the
//! similarity Unicode-semantic for non-ASCII text.

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
/// padded char-trigrams (Jaccard similarity over a lowercased view of
/// each input). Threshold range is the caller's responsibility; this fn
/// does not validate. Used by Track D's `supersede_near_duplicates`
/// flow.
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
    // Both empty implies both inputs were empty after lowercasing — treat
    // as identical so the dedup path doesn't NaN. Two distinct non-empty
    // inputs always have non-empty trigram sets after padding.
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

/// Build the `pg_trgm`-style padded char-trigram set for `s`. Padding
/// (`"  " + s + " "`) ensures even single-char inputs produce two
/// distinct trigrams, so `"a"` and `"b"` no longer collide on an empty
/// set. Iterating over `chars()` keeps the result Unicode-semantic
/// rather than byte-dependent.
#[allow(dead_code)] // wired in D3 / D5
fn trigram_set(s: &str) -> HashSet<[char; 3]> {
    let lower: Vec<char> = s.to_lowercase().chars().collect();
    if lower.is_empty() {
        return HashSet::new();
    }
    let mut padded: Vec<char> = Vec::with_capacity(lower.len() + 3);
    padded.push(' ');
    padded.push(' ');
    padded.extend(lower);
    padded.push(' ');
    let mut out = HashSet::with_capacity(padded.len().saturating_sub(2));
    for w in padded.windows(3) {
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
        // Inserting one short word ("on") puts the padded trigram
        // Jaccard at ~0.86 — clears the plan's 0.85 bar with margin.
        assert!(is_near_duplicate(
            "Release freeze begins Thursday.",
            "Release freeze begins on Thursday.",
            0.85,
        ));
        // Completely unrelated text: trigram Jaccard ≈ 0; any
        // reasonable threshold must reject.
        assert!(!is_near_duplicate(
            "release freeze thursday",
            "migration rolled back",
            0.85,
        ));
        // Single-word substitution drops Jaccard to ~0.63; the strict
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

    // Codex round-1 review (Track D1) flagged that the previous
    // byte-trigram implementation reported "a" vs "b" (and any other
    // distinct sub-3-byte inputs) as a perfect duplicate because both
    // trigram sets were empty. Padding to "  a " and "  b " gives each
    // input two distinct trigrams, restoring the expected 0.0 Jaccard.
    #[test]
    fn is_near_duplicate_distinguishes_short_distinct_inputs() {
        assert!(!is_near_duplicate("a", "b", 0.5));
        assert!(!is_near_duplicate("ab", "cd", 0.5));
        // Identical short input still collides at 1.0.
        assert!(is_near_duplicate("a", "a", 0.99));
    }

    // Codex round-1 review (Track D1) also flagged that byte-windowing
    // on UTF-8 made similarity encoding-dependent: "é" (0xC3 0xA9) and
    // "e" (0x65) shared no bytes by accident, but multi-byte CJK pairs
    // would over-count via shared bytes that aren't shared codepoints.
    // Iterating over chars() makes the comparison Unicode-semantic.
    #[test]
    fn is_near_duplicate_is_unicode_semantic_not_byte_semantic() {
        assert!(!is_near_duplicate("é", "e", 0.5));
        // CJK pair: shares two of three chars → score >0 but well
        // below the strict default. Just confirm it scales by chars,
        // not bytes (a pure byte impl would have over-counted shared
        // UTF-8 prefix bytes).
        let s = jaccard_trigram_score("漢字仮", "漢字語");
        assert!((0.0..0.6).contains(&s), "expected ~0.33, got {s}");
    }
}
