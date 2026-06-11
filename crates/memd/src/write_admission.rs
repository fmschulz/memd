//! Write-admission guardrails for public memory writes.
//!
//! This layer is intentionally conservative. It rejects only records
//! that are clearly low-value or generated-wrapper noise, while leaving
//! existing document/code ingestion compatible.

use crate::auto_priority::has_explicit_priority;
use crate::types::{ChunkType, IngestionMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    Durable,
    Ephemeral,
    Reject,
}

impl AdmissionDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Ephemeral => "ephemeral",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionOutcome {
    pub decision: AdmissionDecision,
    pub reason: String,
    /// In-band caller warning, e.g. a high-priority write admitted at
    /// downgraded priority.
    pub warning: Option<String>,
}

impl AdmissionOutcome {
    pub fn durable(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Durable,
            reason: reason.into(),
            warning: None,
        }
    }

    pub fn durable_with_warning(reason: impl Into<String>, warning: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Durable,
            reason: reason.into(),
            warning: Some(warning.into()),
        }
    }

    pub fn ephemeral(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Ephemeral,
            reason: reason.into(),
            warning: None,
        }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Reject,
            reason: reason.into(),
            warning: None,
        }
    }
}

pub fn classify_write(
    chunk_type: ChunkType,
    text: &str,
    tags: &[String],
    mode: IngestionMode,
) -> AdmissionOutcome {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return AdmissionOutcome::reject("empty memory text is not useful durable context");
    }

    if is_generated_digest_wrapper(trimmed, tags) {
        return AdmissionOutcome::reject(
            "generated digest wrapper records are not accepted through memory.add",
        );
    }

    if explicit_high_priority(tags) && !has_concrete_agent_action(trimmed) {
        // Admit-and-downgrade instead of reject: losing a legitimate
        // lesson outright costs more than storing it one notch lower.
        return AdmissionOutcome::durable_with_warning(
            "high-priority memory without concrete Agent action; priority downgraded to 7",
            format!(
                "priority:8+ or importance:8+ memories need a concrete 'Agent action:' \
                 sentence (>= 24 chars containing one of: {}); stored at priority 7",
                ACTION_VERBS.join(", ")
            ),
        );
    }

    let explicit_priority = has_explicit_priority(tags);
    if explicit_priority {
        return AdmissionOutcome::durable("explicit priority or importance tag");
    }

    let durable_reason = durable_signal_reason(chunk_type, trimmed, tags);

    if tags.iter().any(|tag| tag == "kind:progress") {
        if is_low_signal_progress(trimmed) && durable_reason.is_none() {
            if mode == IngestionMode::Conversation {
                return AdmissionOutcome::ephemeral(
                    "low-signal conversation progress is stored as short-lived hidden context",
                );
            }
            return AdmissionOutcome::reject(
                "low-signal progress chatter needs a concrete result, decision, failure, command, path, or explicit priority",
            );
        }
    }

    if let Some(reason) = durable_reason {
        return AdmissionOutcome::durable(reason);
    }

    AdmissionOutcome::durable("accepted")
}

pub fn is_generated_digest_wrapper(text: &str, tags: &[String]) -> bool {
    let generated = tags.iter().any(|tag| tag == "task:status:generated");
    let digest_like = tags
        .iter()
        .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"));
    if generated && digest_like {
        return true;
    }

    let lowered = text.to_ascii_lowercase();
    lowered.starts_with("task digest status generated")
        || lowered.contains("artifact role: highlight_library")
        || lowered.contains("artifact role: project_brief")
        || lowered.contains("artifact role: failure_library")
        || lowered.contains("artifact role: decision_library")
        || lowered.contains("artifact role: evidence_library")
}

fn is_low_signal_progress(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let compact_words = lowered.split_whitespace().count();
    lowered.starts_with("todo")
        || lowered.starts_with("starting")
        || lowered.starts_with("about to ")
        || lowered.starts_with("working on ")
        || lowered.starts_with("checking ")
        || lowered.starts_with("investigating ")
        || lowered.starts_with("looking into ")
        || (compact_words <= 8
            && !lowered.contains('/')
            && !lowered.contains("http")
            && !lowered.contains("validated")
            && !lowered.contains("root cause"))
}

fn explicit_high_priority(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        tag.strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"))
            .and_then(|value| value.parse::<f32>().ok())
            .map(|value| value >= 8.0)
            .unwrap_or(false)
    })
}

/// Rewrite `priority:`/`importance:` tags valued 8+ down to 7. Applied
/// when a high-priority write lacks a concrete `Agent action:` line.
pub fn downgrade_high_priority_tags(tags: &mut [String]) {
    for tag in tags.iter_mut() {
        let Some(prefix) = ["priority:", "importance:"]
            .iter()
            .find(|p| tag.starts_with(*p))
        else {
            continue;
        };
        if tag[prefix.len()..]
            .parse::<f32>()
            .map(|v| v >= 8.0)
            .unwrap_or(false)
        {
            *tag = format!("{prefix}7");
        }
    }
}

fn has_concrete_agent_action(text: &str) -> bool {
    concrete_agent_action_candidates(text, "agent action:").any(is_concrete_agent_action)
}

fn concrete_agent_action_candidates<'a>(
    text: &'a str,
    marker: &'static str,
) -> impl Iterator<Item = &'a str> {
    let lowered = text.to_ascii_lowercase();
    let mut marker_starts = Vec::new();
    let mut search_start = 0;
    while let Some(relative_start) = lowered[search_start..].find(marker) {
        let marker_start = search_start + relative_start;
        marker_starts.push(marker_start);
        search_start = marker_start + marker.len();
    }

    marker_starts.into_iter().map(move |marker_start| {
        let body_start = marker_start + marker.len();
        let line_end = text[body_start..]
            .find(|ch| matches!(ch, '\n' | '\r'))
            .map(|offset| body_start + offset)
            .unwrap_or(text.len());
        let next_marker = lowered[body_start..]
            .find(marker)
            .map(|offset| body_start + offset)
            .unwrap_or(text.len());
        let body_end = line_end.min(next_marker);
        text[body_start..body_end].trim()
    })
}

fn is_concrete_agent_action(action: &str) -> bool {
    action.chars().count() >= 24 && contains_action_verb(action)
}

/// Imperative verbs that make an `Agent action:` sentence concrete.
/// Shared with memory-md rendering so the gate and the renderer agree.
pub(crate) const ACTION_VERBS: &[&str] = &[
    "apply", "avoid", "check", "configure", "confirm", "disable", "do", "enable", "export",
    "follow", "include", "keep", "pin", "point", "prefer", "record", "resolve", "reuse", "run",
    "set", "treat", "update", "use", "verify", "write",
];

fn contains_action_verb(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| ACTION_VERBS.contains(&word.to_ascii_lowercase().as_str()))
}

fn durable_signal_reason(
    chunk_type: ChunkType,
    text: &str,
    tags: &[String],
) -> Option<&'static str> {
    if matches!(chunk_type, ChunkType::Decision | ChunkType::Code) {
        return Some(match chunk_type {
            ChunkType::Decision => "decision chunk type",
            ChunkType::Code => "code or path-bearing chunk type",
            _ => unreachable!(),
        });
    }

    if let Some(reason) = tags.iter().find_map(|tag| match tag.as_str() {
        "kind:decision" => Some("decision tag"),
        "kind:evidence" => Some("evidence tag"),
        "kind:finish" => Some("finished task outcome tag"),
        "kind:consolidated" => Some("consolidated durable lesson tag"),
        "kind:run" => Some("run/result evidence tag"),
        _ => None,
    }) {
        return Some(reason);
    }

    let lowered = text.to_ascii_lowercase();
    if lowered.contains("decision:") || lowered.contains("rationale:") {
        return Some("decision with rationale");
    }
    if lowered.contains("validated:")
        || lowered.contains("validation:")
        || lowered.contains("reproduced")
        || lowered.contains("confirmed")
        || lowered.contains("passed")
    {
        return Some("validated result");
    }
    if lowered.contains("root cause") || lowered.contains("failed because") {
        return Some("root-cause failure");
    }
    if lowered.contains("fix:") || lowered.contains("fixed by") || lowered.contains("solution:") {
        return Some("validated fix");
    }
    if lowered.contains("command:")
        || lowered.contains("path:")
        || lowered.contains("parameter:")
        || lowered.contains("parameters:")
        || lowered.contains("/home/")
        || lowered.contains("crates/")
        || lowered.contains("tasks/")
        || lowered.contains(".rs")
        || lowered.contains(".md")
        || lowered.contains("http://")
        || lowered.contains("https://")
    {
        return Some("command/path/parameter evidence");
    }
    if lowered.contains("next step:")
        || lowered.contains("follow-up:")
        || lowered.contains("followup:")
    {
        return Some("durable follow-up");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn rejects_generated_digest_wrapper_tags() {
        let outcome = classify_write(
            ChunkType::Summary,
            "Task digest status generated. Summary: Highlight library for p contains 0 ranked lessons.",
            &tags(&["task:status:generated", "task:role:highlight_library"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Reject);
        assert!(outcome.reason.contains("generated digest wrapper"));
    }

    #[test]
    fn rejects_obvious_progress_chatter() {
        let outcome = classify_write(
            ChunkType::Summary,
            "starting to inspect the code",
            &tags(&["kind:progress"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Reject);
        assert!(outcome.reason.contains("low-signal progress"));
    }

    #[test]
    fn downgrades_low_signal_conversation_progress_to_ephemeral() {
        let outcome = classify_write(
            ChunkType::Summary,
            "starting to inspect the code",
            &tags(&["kind:progress"]),
            IngestionMode::Conversation,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Ephemeral);
        assert!(outcome.reason.contains("short-lived hidden context"));
    }

    #[test]
    fn accepts_progress_with_concrete_validation() {
        let outcome = classify_write(
            ChunkType::Summary,
            "Validation: cargo test -p memd passed after fixing digest idempotence.",
            &tags(&["kind:progress"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
    }

    #[test]
    fn accepts_required_durable_categories_with_specific_reasons() {
        let cases = [
            (
                ChunkType::Summary,
                "Decision: keep project aliases explicit. Rationale: silent merging hides scope drift.",
                vec!["kind:progress"],
                "decision with rationale",
            ),
            (
                ChunkType::Summary,
                "Fix: validate alias project IDs before search.",
                vec!["kind:progress"],
                "validated fix",
            ),
            (
                ChunkType::Summary,
                "Validation: cargo test -p memd passed after fixing alias lookup.",
                vec!["kind:progress"],
                "validated result",
            ),
            (
                ChunkType::Summary,
                "Root cause: generated digest wrappers were written as durable summaries.",
                vec!["kind:progress"],
                "root-cause failure",
            ),
            (
                ChunkType::Trace,
                "Command: cargo test -p memd write_admission -- --nocapture.",
                vec!["kind:progress"],
                "command/path/parameter evidence",
            ),
            (
                ChunkType::Summary,
                "Path: crates/memd/src/write_admission.rs holds the write-admission classifier.",
                vec!["kind:progress"],
                "command/path/parameter evidence",
            ),
            (
                ChunkType::Summary,
                "Follow-up: run memory-md useful-top-10 evaluator after Phase 3 rendering changes.",
                vec!["kind:progress"],
                "durable follow-up",
            ),
            (
                ChunkType::Summary,
                "Observed result from benchmark smoke.",
                vec!["kind:evidence"],
                "evidence tag",
            ),
        ];

        for (chunk_type, text, case_tags, expected_reason) in cases {
            let outcome = classify_write(
                chunk_type,
                text,
                &case_tags
                    .iter()
                    .map(|tag| tag.to_string())
                    .collect::<Vec<_>>(),
                IngestionMode::Document,
            );
            assert_eq!(outcome.decision, AdmissionDecision::Durable, "{text}");
            assert_eq!(outcome.reason, expected_reason, "{text}");
        }
    }

    #[test]
    fn downgrades_high_priority_without_agent_action() {
        let outcome = classify_write(
            ChunkType::Summary,
            "starting",
            &tags(&["kind:progress", "priority:9"]),
            IngestionMode::Document,
        );
        // Admitted, not rejected — but downgraded with an in-band
        // warning naming the verb allowlist.
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
        assert!(outcome.reason.contains("downgraded to 7"));
        let warning = outcome.warning.expect("downgrade warning");
        assert!(warning.contains("Agent action"));
        assert!(warning.contains("set"), "warning lists the allowlist: {warning}");
    }

    #[test]
    fn accepts_high_priority_with_expanded_verbs() {
        // "set"/"pin" were missing from the old 16-verb allowlist and
        // rejected legitimate lessons in testing.
        let outcome = classify_write(
            ChunkType::Summary,
            "Validation: batch size fix passed. Agent action: always set ALPHA_EMBED_BATCH=32 before running the embed worker.",
            &tags(&["kind:finish", "priority:9"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn downgrade_high_priority_tags_rewrites_only_eight_plus() {
        let mut tags = vec![
            "priority:9".to_string(),
            "importance:8".to_string(),
            "priority:5".to_string(),
            "kind:finish".to_string(),
        ];
        downgrade_high_priority_tags(&mut tags);
        assert_eq!(
            tags,
            vec![
                "priority:7".to_string(),
                "importance:7".to_string(),
                "priority:5".to_string(),
                "kind:finish".to_string(),
            ]
        );
    }

    #[test]
    fn accepts_high_priority_with_agent_action() {
        let outcome = classify_write(
            ChunkType::Summary,
            "Validation: cache key fix passed. Agent action: Verify tenant_id and project_id before reusing cached retrieval results.",
            &tags(&["kind:progress", "priority:9"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
    }

    #[test]
    fn accepts_high_priority_when_marker_is_explained_before_action() {
        let outcome = classify_write(
            ChunkType::Summary,
            "Validation: memory quality gate passed. High-priority records mention Agent action: in documentation. Agent action: Write every high-priority durable memory with a concrete action sentence that tells future agents what to verify or reuse.",
            &tags(&["kind:finish", "priority:9"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
    }

    #[test]
    fn accepts_high_priority_action_with_path_punctuation() {
        let outcome = classify_write(
            ChunkType::Summary,
            "Validation: installed skill bundle. Agent action: Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior.",
            &tags(&["kind:finish", "priority:9"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
    }
}
