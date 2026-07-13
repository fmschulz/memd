//! Protocol-neutral preparation for every non-artifact memory write.
//!
//! Write surfaces are responsible for parsing their protocol and persisting the
//! result. This module owns the policy decision between those two boundaries so
//! CLI, MCP, batch, import, and synthesis writes cannot drift.

use std::collections::HashSet;

use crate::auto_priority::{has_explicit_priority, stamp_auto_priority};
use crate::task_memory::TrustTier;
use crate::types::lifecycle::{LifecycleDelta, MemoryTier};
use crate::types::{ChunkType, IngestionMode, MemoryChunk};
use crate::write_admission::{
    classify_write, downgrade_high_priority_tags, AdmissionDecision, AdmissionOutcome,
};

pub(crate) const WRITE_ADMISSION_EPHEMERAL_TTL_MS: i64 = 14 * 24 * 60 * 60 * 1000;
pub(crate) const WRITE_ADMISSION_PROGRESS_TTL_MS: i64 = 14 * 24 * 60 * 60 * 1000;
pub(crate) const WRITE_ADMISSION_RUN_TRACE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrioritySource {
    Explicit,
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreparedPriority {
    pub value: f32,
    pub source: PrioritySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionSource {
    None,
    Caller,
    ConversationDefault,
    EphemeralDefault,
    ProgressDefault,
    RunTraceDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRetention {
    pub expires_at_ms: Option<i64>,
    pub review_after_ms: Option<i64>,
    pub source: RetentionSource,
    defaulted_expires_at: bool,
    defaulted_review_after: bool,
}

impl PreparedRetention {
    fn new(expires_at_ms: Option<i64>, review_after_ms: Option<i64>) -> Self {
        Self {
            expires_at_ms,
            review_after_ms,
            source: if expires_at_ms.is_some() || review_after_ms.is_some() {
                RetentionSource::Caller
            } else {
                RetentionSource::None
            },
            defaulted_expires_at: false,
            defaulted_review_after: false,
        }
    }

    fn default_review(&mut self, value: i64, source: RetentionSource) {
        if self.review_after_ms.is_none() {
            self.review_after_ms = Some(value);
            self.defaulted_review_after = true;
            self.source = source;
        }
    }

    fn default_both(&mut self, value: i64, source: RetentionSource) {
        if self.expires_at_ms.is_none() {
            self.expires_at_ms = Some(value);
            self.defaulted_expires_at = true;
        }
        if self.review_after_ms.is_none() {
            self.review_after_ms = Some(value);
            self.defaulted_review_after = true;
        }
        self.source = source;
    }

    /// Drop only service-provided retention defaults. Explicit caller values
    /// remain intact and therefore still require a persistent lifecycle store.
    pub fn strip_defaults(&mut self) {
        if self.defaulted_expires_at {
            self.expires_at_ms = None;
            self.defaulted_expires_at = false;
        }
        if self.defaulted_review_after {
            self.review_after_ms = None;
            self.defaulted_review_after = false;
        }
        self.source = if self.expires_at_ms.is_some() || self.review_after_ms.is_some() {
            RetentionSource::Caller
        } else {
            RetentionSource::None
        };
    }
}

#[derive(Debug, Clone)]
pub struct PrepareWriteRequest<'a> {
    pub chunk_type: ChunkType,
    pub text: &'a str,
    pub tags: &'a [String],
    pub ingestion_mode: IngestionMode,
    pub expires_at_ms: Option<i64>,
    pub review_after_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PreparedWrite {
    pub outcome: AdmissionOutcome,
    pub tags: Vec<String>,
    pub priority: Option<PreparedPriority>,
    pub retention: PreparedRetention,
    pub lifecycle_tier: Option<MemoryTier>,
    pub trust_tier: TrustTier,
    pub ingestion_mode: IngestionMode,
}

impl PreparedWrite {
    pub fn is_rejected(&self) -> bool {
        self.outcome.decision == AdmissionDecision::Reject
    }

    pub fn decision(&self) -> &'static str {
        self.outcome.decision.as_str()
    }

    pub fn usage_outcome(&self) -> &'static str {
        match self.outcome.decision {
            AdmissionDecision::Durable => "admitted",
            AdmissionDecision::Ephemeral => "downgraded",
            AdmissionDecision::Reject => "rejected",
        }
    }

    pub fn lifecycle_tier_name(&self) -> Option<String> {
        self.lifecycle_tier.map(|tier| tier.to_string())
    }

    pub fn lifecycle_delta(&self) -> LifecycleDelta {
        LifecycleDelta {
            tier: self.lifecycle_tier,
            expires_at_ms: self.retention.expires_at_ms.map(Some),
            review_after_ms: self.retention.review_after_ms.map(Some),
            ..Default::default()
        }
    }

    pub fn strip_optional_retention_defaults(&mut self) {
        self.retention.strip_defaults();
    }

    pub fn apply_to_chunk(&self, chunk: MemoryChunk) -> MemoryChunk {
        chunk
            .with_tags(self.tags.clone())
            .with_ingestion_mode(self.ingestion_mode)
    }
}

pub fn prepare_write(request: PrepareWriteRequest<'_>) -> PreparedWrite {
    prepare_write_at(request, current_time_ms())
}

pub fn prepare_write_at(request: PrepareWriteRequest<'_>, now_ms: i64) -> PreparedWrite {
    let mut tags = normalize_tags(request.tags);
    let outcome = classify_write(
        request.chunk_type,
        request.text,
        &tags,
        request.ingestion_mode,
    );
    let mut retention = PreparedRetention::new(request.expires_at_ms, request.review_after_ms);

    if outcome.decision == AdmissionDecision::Reject {
        return PreparedWrite {
            outcome,
            priority: parse_priority(&tags, PrioritySource::Explicit),
            tags,
            retention,
            lifecycle_tier: None,
            trust_tier: TrustTier::SemanticCandidate,
            ingestion_mode: request.ingestion_mode,
        };
    }

    if request.ingestion_mode == IngestionMode::Conversation {
        retention.default_review(
            now_ms + WRITE_ADMISSION_PROGRESS_TTL_MS,
            RetentionSource::ConversationDefault,
        );
    }

    let mut lifecycle_tier = None;
    if outcome.decision == AdmissionDecision::Ephemeral {
        retention.default_both(
            now_ms + WRITE_ADMISSION_EPHEMERAL_TTL_MS,
            RetentionSource::EphemeralDefault,
        );
        lifecycle_tier = Some(MemoryTier::History);
    } else if should_apply_run_trace_retention(request.chunk_type, &tags) {
        retention.default_both(
            now_ms + WRITE_ADMISSION_RUN_TRACE_TTL_MS,
            RetentionSource::RunTraceDefault,
        );
    } else if should_apply_progress_summary_retention(request.chunk_type, &tags) {
        retention.default_both(
            now_ms + WRITE_ADMISSION_PROGRESS_TTL_MS,
            RetentionSource::ProgressDefault,
        );
    }

    if outcome.warning.is_some() {
        downgrade_high_priority_tags(&mut tags);
    }
    if outcome.decision == AdmissionDecision::Ephemeral {
        for tag in ["admission:ephemeral", "retention:short_lived"] {
            if !tags.iter().any(|existing| existing == tag) {
                tags.push(tag.to_string());
            }
        }
    }

    let auto_priority = if outcome.decision == AdmissionDecision::Durable {
        stamp_auto_priority(request.chunk_type, request.text, &mut tags)
    } else {
        None
    };
    let priority = auto_priority
        .map(|value| PreparedPriority {
            value: value as f32,
            source: PrioritySource::Automatic,
        })
        .or_else(|| parse_priority(&tags, PrioritySource::Explicit));

    PreparedWrite {
        outcome,
        tags,
        priority,
        retention,
        lifecycle_tier,
        trust_tier: TrustTier::SemanticCandidate,
        ingestion_mode: request.ingestion_mode,
    }
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(tags.len());
    tags.iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .filter(|tag| seen.insert((*tag).to_string()))
        .map(str::to_string)
        .collect()
}

fn parse_priority(tags: &[String], source: PrioritySource) -> Option<PreparedPriority> {
    tags.iter().find_map(|tag| {
        tag.strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"))
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .map(|value| PreparedPriority { value, source })
    })
}

fn should_apply_run_trace_retention(chunk_type: ChunkType, tags: &[String]) -> bool {
    let is_run_trace = chunk_type == ChunkType::Trace || tags.iter().any(|tag| tag == "kind:run");
    is_run_trace && !has_explicit_priority(tags) && !has_durable_retention_override(tags)
}

fn should_apply_progress_summary_retention(chunk_type: ChunkType, tags: &[String]) -> bool {
    chunk_type == ChunkType::Summary
        && tags.iter().any(|tag| tag == "kind:progress")
        && !has_explicit_priority(tags)
        && !has_durable_retention_override(tags)
}

fn has_durable_retention_override(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        matches!(
            tag.as_str(),
            "kind:evidence"
                | "kind:decision"
                | "kind:finish"
                | "kind:consolidated"
                | "retention:durable"
                | "validated:true"
                | "supports:true"
        ) || tag.starts_with("evidence:")
            || tag.starts_with("source:evidence")
    })
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(text: &'a str, tags: &'a [String]) -> PrepareWriteRequest<'a> {
        PrepareWriteRequest {
            chunk_type: ChunkType::Summary,
            text,
            tags,
            ingestion_mode: IngestionMode::Document,
            expires_at_ms: None,
            review_after_ms: None,
        }
    }

    #[test]
    fn normalizes_and_stamps_once() {
        let tags = vec![
            " kind:decision ".to_string(),
            "kind:decision".to_string(),
            String::new(),
        ];
        let prepared = prepare_write_at(
            request(
                "Decision: use scoped keys. Rationale: global keys leak context.",
                &tags,
            ),
            100,
        );
        assert_eq!(
            prepared.tags,
            vec!["kind:decision".to_string(), "priority:5".to_string()]
        );
        assert_eq!(
            prepared.priority,
            Some(PreparedPriority {
                value: 5.0,
                source: PrioritySource::Automatic,
            })
        );
    }

    #[test]
    fn strips_only_defaulted_retention() {
        let tags = vec!["kind:run".to_string()];
        let mut prepared = prepare_write_at(
            PrepareWriteRequest {
                chunk_type: ChunkType::Trace,
                text: "Command: cargo test -p memd passed.",
                tags: &tags,
                ingestion_mode: IngestionMode::Document,
                expires_at_ms: Some(999),
                review_after_ms: None,
            },
            100,
        );
        prepared.strip_optional_retention_defaults();
        assert_eq!(prepared.retention.expires_at_ms, Some(999));
        assert_eq!(prepared.retention.review_after_ms, None);
        assert_eq!(prepared.retention.source, RetentionSource::Caller);
    }
}
