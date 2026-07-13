//! Durable consolidation journal types.
//!
//! The journal is the metadata-side source of truth for staged
//! consolidation. Candidate payloads may already exist in the tenant WAL
//! while a run is incomplete, but they remain invisible until the journal
//! atomically promotes them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{MemdError, Result};
use crate::types::{ChunkId, TenantId};

/// UUIDv7 identifier for one idempotent consolidation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsolidationRunId(Uuid);

impl ConsolidationRunId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn parse(value: &str) -> Result<Self> {
        Uuid::parse_str(value).map(Self).map_err(|error| {
            MemdError::ValidationError(format!("invalid consolidation run id: {error}"))
        })
    }
}

impl Default for ConsolidationRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConsolidationRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// State machine persisted for both a run and each output entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationState {
    Planned,
    CandidateWritten,
    Validated,
    Committed,
    Rejected,
    RolledBack,
    FailedRecoverable,
}

impl ConsolidationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::CandidateWritten => "candidate_written",
            Self::Validated => "validated",
            Self::Committed => "committed",
            Self::Rejected => "rejected",
            Self::RolledBack => "rolled_back",
            Self::FailedRecoverable => "failed_recoverable",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Rejected | Self::RolledBack)
    }
}

impl fmt::Display for ConsolidationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ConsolidationState {
    type Err = MemdError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "planned" => Ok(Self::Planned),
            "candidate_written" => Ok(Self::CandidateWritten),
            "validated" => Ok(Self::Validated),
            "committed" => Ok(Self::Committed),
            "rejected" => Ok(Self::Rejected),
            "rolled_back" => Ok(Self::RolledBack),
            "failed_recoverable" => Ok(Self::FailedRecoverable),
            other => Err(MemdError::ValidationError(format!(
                "invalid consolidation state: {other}"
            ))),
        }
    }
}

/// Relationship between a committed result and one source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageRelation {
    Supersedes,
    DerivesFrom,
}

impl LineageRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supersedes => "supersedes",
            Self::DerivesFrom => "derives_from",
        }
    }
}

impl fmt::Display for LineageRelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LineageRelation {
    type Err = MemdError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "supersedes" => Ok(Self::Supersedes),
            "derives_from" => Ok(Self::DerivesFrom),
            other => Err(MemdError::ValidationError(format!(
                "invalid lineage relation: {other}"
            ))),
        }
    }
}

/// One durable consolidation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationRun {
    pub run_id: ConsolidationRunId,
    pub tenant_id: TenantId,
    pub project_id: Option<String>,
    pub input_hash: String,
    pub state: ConsolidationState,
    pub consolidator: String,
    pub consolidator_command: Option<String>,
    pub consolidator_model: Option<String>,
    pub consolidator_version: Option<String>,
    pub prompt_hash: Option<String>,
    pub response_hash: Option<String>,
    pub audit_artifact_path: Option<String>,
    pub validation_result: Option<String>,
    pub error: Option<String>,
    pub sparse_cleanup_done: bool,
    /// Durable operator/policy intent. A validated run cannot promote while
    /// this is false, including during crash recovery.
    pub promotion_requested: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// One output slot in a consolidation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationEntryRecord {
    pub run_id: ConsolidationRunId,
    pub entry_index: usize,
    pub candidate_chunk_id: Option<ChunkId>,
    pub source_set_hash: String,
    pub state: ConsolidationState,
    pub validation_error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// A normalized source-to-result lineage edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineage {
    pub run_id: ConsolidationRunId,
    pub tenant_id: TenantId,
    pub project_id: Option<String>,
    pub source_chunk_id: ChunkId,
    pub result_chunk_id: ChunkId,
    pub relation: LineageRelation,
    pub created_at_ms: i64,
}

/// Result of an atomic journal promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionOutcome {
    Committed,
    AlreadyCommitted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_round_trip_and_terminal_states_are_explicit() {
        for state in [
            ConsolidationState::Planned,
            ConsolidationState::CandidateWritten,
            ConsolidationState::Validated,
            ConsolidationState::Committed,
            ConsolidationState::Rejected,
            ConsolidationState::RolledBack,
            ConsolidationState::FailedRecoverable,
        ] {
            assert_eq!(state.as_str().parse::<ConsolidationState>().unwrap(), state);
        }
        assert!(ConsolidationState::Committed.is_terminal());
        assert!(ConsolidationState::Rejected.is_terminal());
        assert!(ConsolidationState::RolledBack.is_terminal());
        assert!(!ConsolidationState::FailedRecoverable.is_terminal());
    }

    #[test]
    fn lineage_relations_round_trip() {
        for relation in [LineageRelation::Supersedes, LineageRelation::DerivesFrom] {
            assert_eq!(
                relation.as_str().parse::<LineageRelation>().unwrap(),
                relation
            );
        }
    }
}
