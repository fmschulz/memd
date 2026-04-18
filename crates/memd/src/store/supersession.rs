//! Content-identity helpers for writer-driven supersession.
//!
//! This module owns the canonicalization step consumed by
//! `PersistentStore::add_chunk_with_lifecycle` and, later, by the
//! conflict-aware add path (Track D) that rewrites near-duplicates into
//! explicit supersession edges.
//!
//! For A6 we only need a deterministic canonical form that downstream
//! dedup logic can hash over; the richer trigram / shingling layer lives
//! in Track D (`D1`). Keeping this stub behind its own module means the
//! Track D expansion is a drop-in replacement without ripping through
//! `persistent.rs`.
//!
//! Rule of thumb for the stub:
//! - Trim leading/trailing whitespace.
//! - Lowercase.
//! - Collapse internal whitespace runs to a single space.
//!
//! The chunk type is accepted now so D1 can specialize per-type
//! canonicalization without another signature change.

use crate::types::ChunkType;

// TODO(D1): extend with trigram scaffolding and per-type specializations
// (e.g. code identifiers preserved verbatim, docs lowercased + punctuation
// stripped). The A6 writer path only needs a stable canonical string.
pub(crate) fn canonicalize_for_type(text: &str, _kind: ChunkType) -> String {
    text.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
}
