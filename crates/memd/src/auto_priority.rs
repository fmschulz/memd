//! Heuristic priority stamping for `memd add` (Phase 1).
//!
//! Agents rarely pass an explicit `priority:N` tag, which means the
//! `priority_score` formula in `cli/memory_md.rs` never gets the
//! `explicit_priority` boost (×10). This module computes a heuristic
//! priority from signals that are already present on the chunk (its
//! `ChunkType`, `kind:*` and validation tags, and a small amount of
//! repetition signal). The CLI and MCP add paths call
//! [`stamp_auto_priority`] right before persisting; it is a no-op if
//! the user already provided a `priority:` or `importance:` tag.
//!
//! The stamped value is conservative (1-10 range) so an explicit user
//! tag always wins on overlap. The goal is to make the existing
//! ranking machinery in `memory.md` actually fire, not to override it.
//!
//! Stamped tags use the form `priority:N` with `N` an integer in
//! [`MIN_AUTO_PRIORITY`]..=[`MAX_AUTO_PRIORITY`]. The value scales as
//! `N * 10` inside `priority_score`, so a heuristic 7 contributes 70
//! points — enough to dominate raw search score (capped at 50) but
//! still allow user-tagged 8/9/10 chunks to out-rank a heuristic
//! guess.

use crate::types::ChunkType;

/// Highest heuristic priority emitted. Reserved values 8-10 stay
/// available for user-set tags so an explicit lesson can always
/// out-rank an automatic guess. The cap is deliberately kept below
/// the `memory.md` suppression-preserve threshold (8) so a heuristic
/// stamp can never masquerade as deliberate operator judgement.
pub const MAX_AUTO_PRIORITY: u8 = 7;

/// Lowest non-trivial priority. Anything below this isn't worth
/// stamping — `priority_score` already weights the chunk via type
/// and kind tags.
pub const MIN_AUTO_PRIORITY: u8 = 3;

/// If the caller already set `priority:` or `importance:`, leave the
/// tag set unchanged. Otherwise compute a heuristic priority from
/// `chunk_type` and tags and append it as `priority:N`.
///
/// Returns the stamped value if a new tag was added, or `None` if no
/// tag was added (because the caller set one, or the heuristic
/// decided it wasn't worth stamping).
pub fn stamp_auto_priority(
    chunk_type: ChunkType,
    text: &str,
    tags: &mut Vec<String>,
) -> Option<u8> {
    if has_explicit_priority(tags) {
        return None;
    }

    let computed = compute_priority(chunk_type, text, tags);
    if computed < MIN_AUTO_PRIORITY {
        return None;
    }
    let clamped = computed.min(MAX_AUTO_PRIORITY);
    tags.push(format!("priority:{clamped}"));
    Some(clamped)
}

/// True if the caller already set a `priority:` or `importance:` tag.
pub fn has_explicit_priority(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        tag.starts_with("priority:") || tag.starts_with("importance:")
    })
}

/// Compute the heuristic priority. Public for tests.
pub fn compute_priority(chunk_type: ChunkType, text: &str, tags: &[String]) -> u8 {
    let mut score: i32 = 0;

    // Chunk type base.
    score += match chunk_type {
        ChunkType::Decision => 5,
        ChunkType::Summary => 3,
        ChunkType::Research => 3,
        ChunkType::Trace => 1,
        ChunkType::Plan => 2,
        _ => 0,
    };

    // kind:* signals. These are the strongest signal the writer can
    // provide; they dominate the heuristic when present.
    for tag in tags {
        match tag.as_str() {
            "kind:decision" => score += 4,
            "kind:finish" => score += 3,
            "kind:evidence" => score += 3,
            "kind:run" => score += 1,
            "kind:progress" => score += 0,
            _ if tag.starts_with("kind:consolidated") => score += 5,
            _ if tag.starts_with("kind:superseded") => return 0, // never stamp tombstones
            _ if tag.starts_with("validated:true") || tag == "supports:true" => score += 1,
            _ if tag.starts_with("status:failed") => score += 1,
            _ if tag.starts_with("ctx:file:") || tag.starts_with("ctx:subsystem:") => score += 1,
            _ => {}
        }
    }

    // Concrete-evidence signals from text content. We look for hints
    // that this chunk records a *verified* outcome — these are the
    // ones future agents most need to find. Cheap substring checks
    // only, no regex.
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("validated:")
        || lowered.contains("validation:")
        || lowered.contains("reproduced")
        || lowered.contains("confirmed")
    {
        score += 1;
    }
    if lowered.contains("root cause") || lowered.contains("fix:") {
        score += 1;
    }
    // Penalise pure progress chatter so we don't stamp every TODO.
    if lowered.starts_with("todo")
        || lowered.starts_with("starting")
        || lowered.starts_with("about to ")
    {
        score -= 2;
    }

    score.clamp(0, MAX_AUTO_PRIORITY as i32) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_chunk_with_kind_decision_gets_high_priority() {
        let mut tags = vec!["kind:decision".to_string(), "task:T1".to_string()];
        let stamped =
            stamp_auto_priority(ChunkType::Decision, "Use tenant-scoped cache keys.", &mut tags);
        assert_eq!(stamped, Some(MAX_AUTO_PRIORITY));
        assert!(tags.iter().any(|t| t == &format!("priority:{MAX_AUTO_PRIORITY}")));
    }

    #[test]
    fn explicit_user_priority_is_preserved() {
        let mut tags = vec!["priority:5".to_string(), "kind:decision".to_string()];
        let stamped =
            stamp_auto_priority(ChunkType::Decision, "Use tenant-scoped cache keys.", &mut tags);
        assert_eq!(stamped, None);
        assert!(tags.iter().filter(|t| t.starts_with("priority:")).count() == 1);
    }

    #[test]
    fn explicit_importance_is_preserved() {
        let mut tags = vec!["importance:9".to_string()];
        let stamped = stamp_auto_priority(ChunkType::Doc, "any", &mut tags);
        assert_eq!(stamped, None);
    }

    #[test]
    fn low_signal_chunk_is_not_stamped() {
        let mut tags = vec!["kind:progress".to_string()];
        let stamped = stamp_auto_priority(ChunkType::Doc, "todo: investigate", &mut tags);
        assert_eq!(stamped, None);
        assert!(!tags.iter().any(|t| t.starts_with("priority:")));
    }

    #[test]
    fn evidence_with_validation_text_gets_stamped() {
        let mut tags = vec!["kind:evidence".to_string(), "supports:true".to_string()];
        let stamped = stamp_auto_priority(
            ChunkType::Research,
            "The failure reproduced before the patch and passed after; validated: yes.",
            &mut tags,
        );
        let value = stamped.expect("evidence with validation should stamp");
        assert!(value >= 5, "expected >= 5, got {value}");
    }

    #[test]
    fn superseded_chunk_is_never_stamped() {
        let mut tags = vec!["kind:superseded".to_string(), "kind:decision".to_string()];
        let stamped = stamp_auto_priority(ChunkType::Decision, "old text", &mut tags);
        assert_eq!(stamped, None);
    }

    #[test]
    fn consolidated_summary_gets_high_priority() {
        let mut tags = vec!["kind:consolidated".to_string()];
        let stamped = stamp_auto_priority(
            ChunkType::Summary,
            "Tenant-scoped cache keys are the canonical fix.",
            &mut tags,
        );
        assert_eq!(stamped, Some(MAX_AUTO_PRIORITY));
    }

    #[test]
    fn caps_at_max_auto_priority() {
        let mut tags = vec![
            "kind:decision".to_string(),
            "kind:evidence".to_string(),
            "kind:finish".to_string(),
            "validated:true".to_string(),
        ];
        let stamped =
            stamp_auto_priority(ChunkType::Decision, "root cause: fix: validated", &mut tags);
        assert_eq!(stamped, Some(MAX_AUTO_PRIORITY));
    }
}
