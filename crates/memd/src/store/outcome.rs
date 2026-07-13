//! Privacy-safe retrieval episodes and explicit task outcomes.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{MemdError, Result};
use crate::types::{ChunkId, TenantId};

pub const OUTCOME_POLICY_VERSION: &str = "outcome-v1";
pub const OUTCOME_HALF_LIFE_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
pub const MAX_OUTCOME_ADJUSTMENT: f32 = 0.20;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RetrievalEpisodeId(Uuid);

impl RetrievalEpisodeId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn parse(value: &str) -> Result<Self> {
        Uuid::parse_str(value).map(Self).map_err(|error| {
            MemdError::ValidationError(format!("invalid retrieval episode id: {error}"))
        })
    }
}

impl Default for RetrievalEpisodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RetrievalEpisodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutcomeEventId(Uuid);

impl OutcomeEventId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn parse(value: &str) -> Result<Self> {
        Uuid::parse_str(value).map(Self).map_err(|error| {
            MemdError::ValidationError(format!("invalid outcome event id: {error}"))
        })
    }
}

impl Default for OutcomeEventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OutcomeEventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingPolicyMode {
    Off,
    Shadow,
    Serve,
}

impl RankingPolicyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Serve => "serve",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "shadow" => Ok(Self::Shadow),
            "serve" => Ok(Self::Serve),
            other => Err(MemdError::ValidationError(format!(
                "invalid ranking policy mode: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Passed,
    Accepted,
    Corrected,
    Failed,
    Abandoned,
}

impl OutcomeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Accepted => "accepted",
            Self::Corrected => "corrected",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "passed" => Ok(Self::Passed),
            "accepted" => Ok(Self::Accepted),
            "corrected" => Ok(Self::Corrected),
            "failed" => Ok(Self::Failed),
            "abandoned" => Ok(Self::Abandoned),
            other => Err(MemdError::ValidationError(format!(
                "invalid retrieval outcome: {other}"
            ))),
        }
    }

    pub const fn credits_used(self) -> bool {
        matches!(self, Self::Passed | Self::Accepted)
    }

    pub const fn credits_harmful(self) -> bool {
        matches!(self, Self::Corrected | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeVerifier {
    User,
    AutomatedTest,
    ExternalTool,
    TaskSystem,
    AgentSelfReport,
}

impl OutcomeVerifier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::AutomatedTest => "automated_test",
            Self::ExternalTool => "external_tool",
            Self::TaskSystem => "task_system",
            Self::AgentSelfReport => "agent_self_report",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "automated_test" => Ok(Self::AutomatedTest),
            "external_tool" => Ok(Self::ExternalTool),
            "task_system" => Ok(Self::TaskSystem),
            "agent_self_report" => Ok(Self::AgentSelfReport),
            other => Err(MemdError::ValidationError(format!(
                "invalid outcome verifier: {other}"
            ))),
        }
    }

    pub const fn is_ranking_eligible(self) -> bool {
        !matches!(self, Self::AgentSelfReport)
    }

    pub const fn requires_evidence(self) -> bool {
        matches!(
            self,
            Self::AutomatedTest | Self::ExternalTool | Self::TaskSystem
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalEpisode {
    pub episode_id: RetrievalEpisodeId,
    pub tenant_id: TenantId,
    pub project_id: Option<String>,
    pub query_hash: String,
    pub query_mode: String,
    pub requested_k: usize,
    pub fetched_k: usize,
    pub rendered_k: usize,
    pub policy_version: String,
    pub policy_mode: RankingPolicyMode,
    pub task_id: Option<String>,
    pub thread_id: Option<String>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalEpisodeItem {
    pub episode_id: RetrievalEpisodeId,
    pub chunk_id: ChunkId,
    pub origin_tenant_id: TenantId,
    pub origin_project_id: Option<String>,
    pub original_rank: usize,
    pub original_score: f32,
    pub lane_scores_json: String,
    pub outcome_adjustment: f32,
    pub served_rank: Option<usize>,
    pub shadow_rank: Option<usize>,
    pub rendered: bool,
    pub source_dedup_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeEvent {
    pub event_id: OutcomeEventId,
    pub episode_id: RetrievalEpisodeId,
    pub outcome: OutcomeKind,
    pub verifier: OutcomeVerifier,
    pub used_chunk_ids: Vec<ChunkId>,
    pub harmful_chunk_ids: Vec<ChunkId>,
    pub evidence_reference: Option<String>,
    pub ranking_eligible: bool,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomePrior {
    pub chunk_id: ChunkId,
    pub eligible_episode_count: usize,
    pub positive_weight: f32,
    pub negative_weight: f32,
    pub last_outcome_at_ms: i64,
}

impl OutcomePrior {
    pub fn new(chunk_id: ChunkId) -> Self {
        Self {
            chunk_id,
            eligible_episode_count: 0,
            positive_weight: 0.0,
            negative_weight: 0.0,
            last_outcome_at_ms: 0,
        }
    }

    pub fn add(&mut self, positive: bool, weight: f32, timestamp_ms: i64) {
        self.eligible_episode_count = self.eligible_episode_count.saturating_add(1);
        if positive {
            self.positive_weight += weight;
        } else {
            self.negative_weight += weight;
        }
        self.last_outcome_at_ms = self.last_outcome_at_ms.max(timestamp_ms);
    }

    /// Bayesian shrinkage keeps one outcome from dominating semantic score;
    /// repeated independent episodes asymptotically approach the hard cap.
    pub fn bounded_adjustment(&self) -> f32 {
        let evidence = self.positive_weight + self.negative_weight;
        if evidence <= f32::EPSILON {
            return 0.0;
        }
        ((self.positive_weight - self.negative_weight) / (evidence + 4.0) * MAX_OUTCOME_ADJUSTMENT)
            .clamp(-MAX_OUTCOME_ADJUSTMENT, MAX_OUTCOME_ADJUSTMENT)
    }
}

pub fn decayed_outcome_weight(timestamp_ms: i64, now_ms: i64) -> f32 {
    let age_ms = now_ms.saturating_sub(timestamp_ms).max(0) as f64;
    2.0_f64.powf(-age_ms / OUTCOME_HALF_LIFE_MS as f64) as f32
}

impl OutcomeEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        episode_id: RetrievalEpisodeId,
        outcome: OutcomeKind,
        verifier: OutcomeVerifier,
        used_chunk_ids: Vec<ChunkId>,
        harmful_chunk_ids: Vec<ChunkId>,
        evidence_reference: Option<String>,
        timestamp_ms: i64,
    ) -> Self {
        let ranking_eligible = verifier.is_ranking_eligible()
            && ((outcome.credits_used() && !used_chunk_ids.is_empty())
                || (outcome.credits_harmful() && !harmful_chunk_ids.is_empty()));
        Self {
            event_id: OutcomeEventId::new(),
            episode_id,
            outcome,
            verifier,
            used_chunk_ids,
            harmful_chunk_ids,
            evidence_reference,
            ranking_eligible,
            timestamp_ms,
        }
    }
}

pub fn stable_query_hash(query: &str) -> String {
    let canonical = query.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

pub fn validate_retrieval_episode(
    episode: &RetrievalEpisode,
    items: &[RetrievalEpisodeItem],
) -> Result<()> {
    if episode.query_hash.len() != 64
        || !episode
            .query_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MemdError::ValidationError(
            "retrieval episode query hash must be a SHA-256 hex digest".to_string(),
        ));
    }
    if episode.query_mode.trim().is_empty() || episode.policy_version.trim().is_empty() {
        return Err(MemdError::ValidationError(
            "retrieval episode mode and policy version must not be empty".to_string(),
        ));
    }
    for (name, value) in [
        ("task_id", episode.task_id.as_deref()),
        ("thread_id", episode.thread_id.as_deref()),
    ] {
        if let Some(value) = value {
            if value.trim().is_empty() || value.len() > 512 {
                return Err(MemdError::ValidationError(format!(
                    "retrieval episode {name} must contain 1 to 512 bytes"
                )));
            }
        }
    }
    if episode.expires_at_ms <= episode.created_at_ms {
        return Err(MemdError::ValidationError(
            "retrieval episode expiry must follow creation".to_string(),
        ));
    }
    if episode.fetched_k != items.len() {
        return Err(MemdError::ValidationError(format!(
            "retrieval episode fetched_k {} does not match {} candidate items",
            episode.fetched_k,
            items.len()
        )));
    }
    if episode.requested_k > 200 || episode.fetched_k > 200 || episode.rendered_k > 200 {
        return Err(MemdError::ValidationError(
            "retrieval episode exceeds bounded k limits".to_string(),
        ));
    }
    if episode.rendered_k != items.iter().filter(|item| item.rendered).count() {
        return Err(MemdError::ValidationError(
            "retrieval episode rendered_k does not match rendered items".to_string(),
        ));
    }

    let mut chunk_ids = HashSet::new();
    let mut original_ranks = HashSet::new();
    let mut served_ranks = HashSet::new();
    let mut shadow_ranks = HashSet::new();
    for item in items {
        if item.episode_id != episode.episode_id {
            return Err(MemdError::ValidationError(
                "retrieval episode item references a different episode".to_string(),
            ));
        }
        if !chunk_ids.insert(item.chunk_id.clone()) || !original_ranks.insert(item.original_rank) {
            return Err(MemdError::ValidationError(
                "retrieval episode candidate IDs and original ranks must be unique".to_string(),
            ));
        }
        if !item.original_score.is_finite() || !item.outcome_adjustment.is_finite() {
            return Err(MemdError::ValidationError(
                "retrieval episode scores must be finite".to_string(),
            ));
        }
        if item.lane_scores_json.len() > 4096
            || !matches!(
                serde_json::from_str::<serde_json::Value>(&item.lane_scores_json),
                Ok(serde_json::Value::Object(_))
            )
        {
            return Err(MemdError::ValidationError(
                "retrieval episode lane scores must be a bounded JSON object".to_string(),
            ));
        }
        if let Some(rank) = item.served_rank {
            if !served_ranks.insert(rank) {
                return Err(MemdError::ValidationError(
                    "retrieval episode served ranks must be unique".to_string(),
                ));
            }
        }
        if let Some(rank) = item.shadow_rank {
            if !shadow_ranks.insert(rank) {
                return Err(MemdError::ValidationError(
                    "retrieval episode shadow ranks must be unique".to_string(),
                ));
            }
        }
        if item.rendered && item.served_rank.is_none() {
            return Err(MemdError::ValidationError(
                "a rendered retrieval item must have a served rank".to_string(),
            ));
        }
    }
    Ok(())
}

/// Replace the served/rendered projection after a caller-side reranker or
/// packer changes the final output. Candidate identity and shadow ranks remain
/// immutable; only the observable served projection is rewritten.
pub fn apply_rendered_order(
    episode: &mut RetrievalEpisode,
    items: &mut [RetrievalEpisodeItem],
    rendered_chunk_ids: &[ChunkId],
) -> Result<()> {
    if rendered_chunk_ids.len() > episode.requested_k {
        return Err(MemdError::ValidationError(format!(
            "rendered retrieval count {} exceeds requested k {}",
            rendered_chunk_ids.len(),
            episode.requested_k
        )));
    }
    let mut requested = HashSet::new();
    for chunk_id in rendered_chunk_ids {
        if !requested.insert(chunk_id.clone()) {
            return Err(MemdError::ValidationError(
                "rendered retrieval order contains duplicate chunk IDs".to_string(),
            ));
        }
        if !items.iter().any(|item| &item.chunk_id == chunk_id) {
            return Err(MemdError::ValidationError(format!(
                "rendered chunk {chunk_id} was not in the retrieval candidate pool"
            )));
        }
    }
    for item in items.iter_mut() {
        item.served_rank = None;
        item.rendered = false;
    }
    for (rank, chunk_id) in rendered_chunk_ids.iter().enumerate() {
        let item = items
            .iter_mut()
            .find(|item| &item.chunk_id == chunk_id)
            .expect("candidate membership checked above");
        item.served_rank = Some(rank);
        item.rendered = true;
    }
    episode.rendered_k = rendered_chunk_ids.len();
    validate_retrieval_episode(episode, items)
}

pub fn validate_outcome_event(
    tenant_id: &TenantId,
    episode: &RetrievalEpisode,
    items: &[RetrievalEpisodeItem],
    event: &OutcomeEvent,
) -> Result<()> {
    if &episode.tenant_id != tenant_id || event.episode_id != episode.episode_id {
        return Err(MemdError::ValidationError(
            "outcome tenant or episode does not match the retrieval episode".to_string(),
        ));
    }
    if event.timestamp_ms < episode.created_at_ms || event.timestamp_ms > episode.expires_at_ms {
        return Err(MemdError::ValidationError(
            "outcome timestamp falls outside the retrieval episode retention window".to_string(),
        ));
    }
    if event.verifier.requires_evidence()
        && event
            .evidence_reference
            .as_deref()
            .is_none_or(|reference| reference.trim().is_empty())
    {
        return Err(MemdError::ValidationError(format!(
            "{} outcomes require an evidence reference",
            event.verifier.as_str()
        )));
    }
    if event
        .evidence_reference
        .as_ref()
        .is_some_and(|reference| reference.len() > 2048)
    {
        return Err(MemdError::ValidationError(
            "outcome evidence reference exceeds 2048 bytes".to_string(),
        ));
    }

    let rendered = items
        .iter()
        .filter(|item| item.rendered)
        .map(|item| item.chunk_id.clone())
        .collect::<HashSet<_>>();
    let used = event.used_chunk_ids.iter().cloned().collect::<HashSet<_>>();
    let harmful = event
        .harmful_chunk_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    if used.len() != event.used_chunk_ids.len() || harmful.len() != event.harmful_chunk_ids.len() {
        return Err(MemdError::ValidationError(
            "outcome chunk IDs must not contain duplicates".to_string(),
        ));
    }
    if used.iter().any(|chunk_id| harmful.contains(chunk_id)) {
        return Err(MemdError::ValidationError(
            "outcome used and harmful chunk IDs must not overlap".to_string(),
        ));
    }
    if used
        .iter()
        .chain(harmful.iter())
        .any(|chunk_id| !rendered.contains(chunk_id))
    {
        return Err(MemdError::ValidationError(
            "outcome attribution is limited to chunks rendered in the episode".to_string(),
        ));
    }
    let expected_eligible = event.verifier.is_ranking_eligible()
        && ((event.outcome.credits_used() && !used.is_empty())
            || (event.outcome.credits_harmful() && !harmful.is_empty()));
    if event.ranking_eligible != expected_eligible {
        return Err(MemdError::ValidationError(
            "outcome ranking eligibility does not match its verifier and attribution".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_hash_is_stable_and_whitespace_canonical() {
        assert_eq!(stable_query_hash("a  b"), stable_query_hash("a b"));
        assert_eq!(stable_query_hash("A b"), stable_query_hash("A b"));
        assert_ne!(stable_query_hash("A b"), stable_query_hash("a b"));
    }

    #[test]
    fn agent_self_reports_never_become_ranking_eligible() {
        let event = OutcomeEvent::new(
            RetrievalEpisodeId::new(),
            OutcomeKind::Passed,
            OutcomeVerifier::AgentSelfReport,
            vec![ChunkId::new()],
            Vec::new(),
            None,
            1,
        );
        assert!(!event.ranking_eligible);
    }

    #[test]
    fn corrected_requires_harmful_attribution_and_abandoned_is_never_eligible() {
        let chunk_id = ChunkId::new();
        let corrected = OutcomeEvent::new(
            RetrievalEpisodeId::new(),
            OutcomeKind::Corrected,
            OutcomeVerifier::User,
            Vec::new(),
            vec![chunk_id.clone()],
            None,
            1,
        );
        assert!(corrected.ranking_eligible);

        let corrected_without_attribution = OutcomeEvent::new(
            RetrievalEpisodeId::new(),
            OutcomeKind::Corrected,
            OutcomeVerifier::User,
            Vec::new(),
            Vec::new(),
            None,
            1,
        );
        assert!(!corrected_without_attribution.ranking_eligible);

        let abandoned = OutcomeEvent::new(
            RetrievalEpisodeId::new(),
            OutcomeKind::Abandoned,
            OutcomeVerifier::User,
            vec![chunk_id],
            Vec::new(),
            None,
            1,
        );
        assert!(!abandoned.ranking_eligible);
    }

    #[test]
    fn retrieval_episode_linkage_ids_are_bounded() {
        let episode_id = RetrievalEpisodeId::new();
        let episode = RetrievalEpisode {
            episode_id: episode_id.clone(),
            tenant_id: TenantId::new("linkage_test").unwrap(),
            project_id: None,
            query_hash: stable_query_hash("bounded linkage"),
            query_mode: "generic".to_string(),
            requested_k: 0,
            fetched_k: 0,
            rendered_k: 0,
            policy_version: OUTCOME_POLICY_VERSION.to_string(),
            policy_mode: RankingPolicyMode::Off,
            task_id: Some("x".repeat(513)),
            thread_id: None,
            created_at_ms: 1,
            expires_at_ms: 2,
        };
        let error = validate_retrieval_episode(&episode, &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("task_id must contain 1 to 512 bytes"));
    }
}
