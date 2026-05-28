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
}

impl AdmissionOutcome {
    pub fn durable(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Durable,
            reason: reason.into(),
        }
    }

    pub fn ephemeral(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Ephemeral,
            reason: reason.into(),
        }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            decision: AdmissionDecision::Reject,
            reason: reason.into(),
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
    fn explicit_priority_overrides_short_progress() {
        let outcome = classify_write(
            ChunkType::Summary,
            "starting",
            &tags(&["kind:progress", "priority:9"]),
            IngestionMode::Document,
        );
        assert_eq!(outcome.decision, AdmissionDecision::Durable);
    }
}
