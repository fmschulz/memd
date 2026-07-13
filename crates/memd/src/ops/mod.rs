//! Protocol-neutral operation handlers.
//!
//! Each handler validates parameters, calls the store, and formats the response
//! consumed by direct CLI commands, `memd call`, and `memd batch`.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

/// Whether retrieval that carries a `project_id` may widen across every
/// tenant on this daemon that contains the same project string.
///
/// Off by default: tenant isolation is the expected behavior, the
/// fallback is a migration shim only. CLI startup sets this from the
/// compatibility routing config.
static ALLOW_CROSS_TENANT_PROJECT_FALLBACK: AtomicBool = AtomicBool::new(false);
static PROJECT_ALIASES: OnceLock<std::sync::RwLock<Vec<ProjectAliasConfig>>> = OnceLock::new();
const HOT_CONTEXT_SCAN_TIMEOUT_MS: u64 = 2_000;
const EXACT_RESCUE_PAGE_SIZE: usize = 500;
const EXACT_RESCUE_PROJECT_SCAN_LIMIT: usize = 50_000;
const EXACT_RESCUE_GLOBAL_SCAN_LIMIT: usize = 10_000;
const EXACT_RESCUE_SCORE_BOOST: f32 = 20.0;
const LEXICAL_OVERLAP_PROJECT_SCAN_LIMIT: usize = 500;
const LEXICAL_OVERLAP_SCORE_BOOST: f32 = 16.0;
const LEXICAL_OVERLAP_MAX_CANDIDATES: usize = 100;

/// Apply process-wide compatibility routing for operation handlers.
pub fn configure_operation_routing(
    allow_cross_tenant_project_fallback: bool,
    project_aliases: Vec<ProjectAliasConfig>,
) {
    set_cross_tenant_project_fallback(allow_cross_tenant_project_fallback);
    set_project_aliases(project_aliases);
}

/// Enable or disable the cross-tenant project fallback. Exposed as
/// `pub(crate)` so unit tests can flip it.
pub(crate) fn set_cross_tenant_project_fallback(enabled: bool) {
    ALLOW_CROSS_TENANT_PROJECT_FALLBACK.store(enabled, Ordering::Relaxed);
}

#[allow(dead_code)]
pub(crate) fn set_project_aliases(aliases: Vec<ProjectAliasConfig>) {
    let lock = PROJECT_ALIASES.get_or_init(|| std::sync::RwLock::new(Vec::new()));
    if let Ok(mut guard) = lock.write() {
        *guard = aliases;
    }
}

fn cross_tenant_project_fallback_enabled() -> bool {
    ALLOW_CROSS_TENANT_PROJECT_FALLBACK.load(Ordering::Relaxed)
}

fn configured_project_aliases(primary_tenant: &TenantId, project_id: &str) -> Vec<OriginScope> {
    let Some(lock) = PROJECT_ALIASES.get() else {
        return Vec::new();
    };
    let Ok(aliases) = lock.read() else {
        return Vec::new();
    };
    aliases
        .iter()
        .filter(|rule| rule.tenant_id == primary_tenant.as_str() && rule.project_id == project_id)
        .flat_map(|rule| {
            rule.aliases.iter().map(move |alias| {
                let alias_project = alias.project_id.as_deref().unwrap_or(project_id);
                OriginScope {
                    requested_tenant_id: primary_tenant.to_string(),
                    origin_tenant_id: alias.tenant_id.clone(),
                    origin_project_id: Some(alias_project.to_string()),
                    alias_reason: alias
                        .reason
                        .clone()
                        .unwrap_or_else(|| "configured_project_alias".to_string()),
                }
            })
        })
        .collect()
}

/// Resolve an `agent_id` for an artifact write from an explicit param.
///
/// Rationale — the v0.3.0 prototype maintained a process-global default
/// derived from an implicit client handshake in a `static RwLock<Option<String>>`.
/// That was unsound because one caller could overwrite another's identity and
/// bypass the distinct-writer countersignature rule.
///
/// v0.3.1 therefore keeps agent identity **explicit**: callers supply
/// `agent_id` on artifact writes when they want countersignature
/// promotion. The trust-tier check in `promote_if_countersigned` already
/// requires both the current and the parent artifact to have a
/// non-empty `agent_id`, so anonymous writes simply cannot produce a
/// false `VerifiedRecord`.
///
/// Per-session auto-population (without the bleed hazard) lands in
/// Phase 2 alongside the HTTP session model.
pub(crate) fn resolved_agent_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub mod error;

pub use error::OperationError;
use error::OperationError as McpError;
// `pub use` keeps `memd::ops::PostWriteEvent` canonical while the compatibility
// module continues to expose the historical `memd::mcp::*` paths.
use crate::config::ProjectAliasConfig;
use crate::maintenance::{
    apply_lifecycle_actions, build_reclaimed, disk_snapshot, estimated_hidden_payload_bytes,
    plan_duplicate_projection_retirements, prune_sparse_index_for_actions,
    related_artifact_ids_from_actions, related_artifact_ids_from_project_artifacts,
    status_for_report, unsupported_exact_safe_warning, DreamAction, DreamParams, DreamPolicy,
    DreamReport, DreamScope, DreamStateSnapshot, PhysicalCompactionResult,
    DIGEST_ROLE_DREAM_REPORT,
};
use crate::metrics::{IndexStats, MetricsCollector};
pub use crate::post_write_hooks::PostWriteEvent;
use crate::retrieval::{ContextPacker, PackerConfig, PackerInput};
use crate::store::metadata::MetadataStore;
use crate::store::usage::{query_hash_hex, UsageEvent, UsageOp};
use crate::store::{
    rank_candidate_chunks, DuplicateHealth, FeedbackEntry, HealthCounts, IndexCoverageHealth,
    OutcomeEvent, OutcomeKind, OutcomeVerifier, PayloadHealth, RankingPolicyMode, RelevanceLabel,
    RetrievalEpisode, RetrievalEpisodeId, RetrievalEpisodeItem, Store, StoreHealthSnapshot,
    StoreStats, TenantManager, OUTCOME_POLICY_VERSION,
};
use crate::task_memory::{
    build_library_digest_artifact, build_project_brief_digest_artifact, build_project_brief_view,
    build_task_projections, build_task_projections_minimal, build_task_resume_digest_artifact,
    build_task_resume_view, derive_artifact_promotion_state, derive_artifact_trust_tier,
    derive_chunk_trust_tier, infer_decision_items, infer_evidence_items, infer_failure_items,
    infer_highlight_items, stable_digest_identity, ArtifactKind, ContributorRef, DatasetRef,
    DecisionViewItem, EntityRef, EvidenceViewItem, FailureViewItem, HighlightViewItem,
    ProjectBriefView, TaskArtifact, TaskProvenance, TaskResumeView, TaskSearchFilters, TrustTier,
    DIGEST_ROLE_DECISION_LIBRARY, DIGEST_ROLE_EVIDENCE_LIBRARY, DIGEST_ROLE_FAILURE_LIBRARY,
    DIGEST_ROLE_HIGHLIGHT_LIBRARY, DIGEST_ROLE_PROJECT_BRIEF, DIGEST_ROLE_TASK_RESUME,
};
use crate::tiered::TieredTiming;
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, LifecycleDelta, MemoryChunk, ProjectId, Source, TenantId,
    VisibilityPolicy,
};
use crate::write_service::{PrepareWriteRequest, PreparedWrite};
#[cfg(test)]
use crate::write_service::{WRITE_ADMISSION_PROGRESS_TTL_MS, WRITE_ADMISSION_RUN_TRACE_TTL_MS};

mod shared_types;
pub use shared_types::*;

mod add;
mod context;
mod feedback;
mod lifecycle;
mod maintenance;
mod search;
mod task;

pub use add::*;
pub use context::*;
pub use feedback::*;
pub use lifecycle::*;
pub use maintenance::*;
pub use search::*;
pub use task::*;

#[derive(Debug, Clone)]
struct ProjectSearchScope {
    tenant_id: TenantId,
    project_id: Option<String>,
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- Helper Functions ----------

fn validate_search_k(k: usize) -> Result<(), McpError> {
    if (1..=100).contains(&k) {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "invalid 'k': must be between 1 and 100".to_string(),
    ))
}

fn validate_search_time_range(
    filters: Option<&SearchFilters>,
) -> Result<(Option<i64>, Option<i64>), McpError> {
    let Some(time_range) = filters.and_then(|f| f.time_range.as_ref()) else {
        return Ok((None, None));
    };

    let from_ms = time_range
        .from
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.from: {}", e))
            })
        })
        .transpose()?;

    let to_ms = time_range
        .to
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.to: {}", e))
            })
        })
        .transpose()?;

    if let (Some(from_ms), Some(to_ms)) = (from_ms, to_ms) {
        if from_ms > to_ms {
            return Err(McpError::InvalidParams(
                "invalid filters.time_range: 'from' must be <= 'to'".to_string(),
            ));
        }
    }

    Ok((from_ms, to_ms))
}

#[derive(Debug, Default)]
struct ParsedSearchFilters {
    chunk_types: Option<HashSet<ChunkType>>,
    episode_id: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
}

fn parse_search_filters(filters: Option<&SearchFilters>) -> Result<ParsedSearchFilters, McpError> {
    let (from_ms, to_ms) = validate_search_time_range(filters)?;

    let chunk_types = filters
        .and_then(|f| f.types.as_ref())
        .map(|types| {
            types
                .iter()
                .map(|t| parse_chunk_type(t))
                .collect::<Result<HashSet<_>, _>>()
        })
        .transpose()?;

    Ok(ParsedSearchFilters {
        chunk_types,
        episode_id: filters.and_then(|f| f.episode_id.clone()),
        from_ms,
        to_ms,
    })
}

fn apply_search_filters(
    scored_chunks: Vec<(MemoryChunk, f32)>,
    project_id: Option<&str>,
    filters: &ParsedSearchFilters,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    scored_chunks
        .into_iter()
        .filter(|(chunk, _)| {
            if let Some(project_id) = project_id {
                if chunk.project_id.as_option() != Some(project_id) {
                    return false;
                }
            }

            if let Some(types) = filters.chunk_types.as_ref() {
                if !types.contains(&chunk.chunk_type) {
                    return false;
                }
            }

            if let Some(episode_id) = filters.episode_id.as_deref() {
                let expected_tag = format!("episode:{}", episode_id);
                if !chunk.tags.iter().any(|tag| tag == &expected_tag) {
                    return false;
                }
            }

            if let Some(from_ms) = filters.from_ms {
                if chunk.timestamp_created < from_ms {
                    return false;
                }
            }

            if let Some(to_ms) = filters.to_ms {
                if chunk.timestamp_created > to_ms {
                    return false;
                }
            }

            true
        })
        .take(k)
        .collect()
}

/// Apply the lifecycle visibility policy to an over-sampled ranked list and
/// trim to `k`. Superseded, Expired, and History-tier chunks are dropped
/// unless the corresponding `include_*` flag is set; rows with an
/// `expires_at_ms` that has already passed are dropped unless
/// `include_expired` is set; Deleted and Error rows are always dropped
/// regardless of flags (the `Error` hide is the reason this loop cannot
/// be short-circuited when all three `include_*` are true — the ranker
/// backends only filter `Deleted`, so `Error` can still reach the
/// handler and must be caught here).
///
/// "Oversample-and-refill" is the whole point: callers request more than
/// `k` candidates from the ranker so that even when the top hits are
/// hidden we can still return a full page of visible results.
///
/// Cross-tenant correctness: when the operator explicitly enables the
/// legacy project fallback, `memory.search` can return hits across
/// tenants. The visibility lookup must use the hit row's own
/// `chunk.tenant_id`, not an outer tenant parameter, or a project-scoped
/// search across tenants would point at the wrong overlay rows.
///
/// Cost: one `get_with_lifecycle` per kept candidate. With the default
/// `oversample_factor=3` and `k=20`, this is up to 60 metadata reads per
/// query. This is a known tail-latency cost of the visibility overlay;
/// a cheaper design that carries `ResolvedChunk` from the ranker is a
/// future optimisation (tracked as a followup) but would require
/// changing the search return shape.
async fn apply_visibility_filter<S: Store>(
    store: &S,
    ranked: Vec<(MemoryChunk, f32)>,
    policy: &VisibilityPolicy,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    let now_ms = current_time_ms();
    let mut out: Vec<(MemoryChunk, f32)> = Vec::with_capacity(k.min(ranked.len()));
    for (chunk, score) in ranked {
        if out.len() >= k {
            break;
        }
        match store
            .get_with_lifecycle(&chunk.tenant_id, &chunk.chunk_id)
            .await
        {
            Ok(Some(resolved)) => {
                if policy.is_visible_at(resolved.status, &resolved.lifecycle, now_ms) {
                    // Use the resolved chunk payload (same content, but
                    // from the overlay path — keeps any future overlay-
                    // side payload annotations consistent with memory.get).
                    out.push((resolved.chunk, score));
                }
            }
            Ok(None) => {
                // Row was deleted between the ranker pull and the
                // visibility check — drop it.
            }
            Err(e) => {
                // Transient overlay lookup failure: log and drop this
                // row rather than failing the whole search. Fail-closed
                // (drop) is safer than leaking a row whose status we
                // couldn't verify.
                warn!(
                    chunk_id = %chunk.chunk_id,
                    tenant_id = %chunk.tenant_id,
                    error = %e,
                    "visibility filter: get_with_lifecycle failed, dropping hit"
                );
            }
        }
    }
    out
}

/// Resolve the effective `VisibilityPolicy` and oversample factor for a
/// search call. The oversample factor is capped at 10 so a pathological
/// caller can't force a 100x ranker pull by setting it to 1000.
fn resolve_visibility_and_oversample(params: &SearchParams) -> (VisibilityPolicy, usize) {
    let policy = VisibilityPolicy {
        include_superseded: params.include_superseded.unwrap_or(false),
        include_expired: params.include_expired.unwrap_or(false),
        include_history: params.include_history.unwrap_or(false),
    };
    // When every include_* is true, the filter is effectively a no-op —
    // don't oversample.
    let all_permissive =
        policy.include_superseded && policy.include_expired && policy.include_history;
    let oversample = if all_permissive {
        1
    } else {
        params.oversample_factor.unwrap_or(3).clamp(1, 10)
    };
    (policy, oversample)
}

fn parse_tag_usize(tags: &[String], prefix: &str) -> Option<usize> {
    tags.iter().find_map(|tag| {
        tag.strip_prefix(prefix)
            .and_then(|value| value.parse().ok())
    })
}

fn extract_episode_id(tags: &[String]) -> Option<String> {
    tags.iter()
        .find_map(|tag| tag.strip_prefix("episode:").map(|value| value.to_string()))
}

const EVENT_TAG_PREFIX: &str = "event:";
const EVENT_SIBLING_EXPANSION_LIMIT: usize = 20;

fn event_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter(|tag| tag.starts_with(EVENT_TAG_PREFIX) && tag.len() > EVENT_TAG_PREFIX.len())
        .cloned()
        .collect()
}

fn make_episode_tag(episode_id: &str) -> String {
    format!("episode:{}", episode_id)
}

fn validate_episode_id(episode_id: &str) -> Result<(), McpError> {
    if episode_id.is_empty() {
        return Err(McpError::InvalidParams(
            "episode_id must not be empty".to_string(),
        ));
    }

    if episode_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "episode_id must contain only letters, digits, '_' or '-'".to_string(),
    ))
}

fn parse_relevance_label(value: &str) -> Result<RelevanceLabel, McpError> {
    match value.to_ascii_lowercase().as_str() {
        "relevant" | "positive" => Ok(RelevanceLabel::Relevant),
        "irrelevant" | "negative" => Ok(RelevanceLabel::Irrelevant),
        _ => Err(McpError::InvalidParams(
            "invalid relevance: must be one of [relevant, irrelevant]".to_string(),
        )),
    }
}

fn build_citation(chunk: &MemoryChunk) -> CitationResult {
    let hash_prefix = chunk.hash.get(..12).unwrap_or(&chunk.hash);
    CitationResult {
        citation_id: format!("{}:{}", chunk.chunk_id, hash_prefix),
        content_hash: chunk.hash.clone(),
        source_uri: chunk.source.uri.clone(),
        source_repo: chunk.source.repo.clone(),
        source_commit: chunk.source.commit.clone(),
        source_path: chunk.source.path.clone(),
        source_tool_name: chunk.source.tool_name.clone(),
        source_tool_call_id: chunk.source.tool_call_id.clone(),
        chunk_index: parse_tag_usize(&chunk.tags, "chunk_index:"),
        total_chunks: parse_tag_usize(&chunk.tags, "total_chunks:"),
        char_start: parse_tag_usize(&chunk.tags, "char_start:"),
        char_end: parse_tag_usize(&chunk.tags, "char_end:"),
    }
}

fn build_grounding_ref(artifact: &TaskArtifact, citation: Option<CitationResult>) -> GroundingRef {
    GroundingRef {
        artifact_id: artifact.artifact_id.clone(),
        task_id: artifact.task_id.clone(),
        thread_id: artifact.thread_key().to_string(),
        artifact_kind: artifact.artifact_kind.as_str().to_string(),
        artifact_role: artifact.artifact_role.clone(),
        promotion_state: artifact.promotion_state.to_string(),
        citation,
    }
}

fn verification_hint_for_trust_tier(trust_tier: TrustTier) -> VerificationHint {
    match trust_tier {
        TrustTier::SemanticCandidate => VerificationHint {
            requires_verification: true,
            reason: "semantic candidate without canonical artifact grounding".to_string(),
        },
        TrustTier::CanonicalRecord => VerificationHint {
            requires_verification: false,
            reason: "linked to a canonical non-digest artifact".to_string(),
        },
        TrustTier::CompiledDigestHint => VerificationHint {
            requires_verification: true,
            reason:
                "compiled digest hint; re-ground against canonical artifacts before trusting claims"
                    .to_string(),
        },
        TrustTier::VerifiedRecord => VerificationHint {
            requires_verification: false,
            reason: "linked to an explicit verification or otherwise verified record".to_string(),
        },
    }
}

fn artifact_text_for_grounding(artifact: &TaskArtifact) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = artifact
        .summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(summary.clone());
    }
    if let Some(goal) = artifact
        .goal
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(goal.clone());
    }
    if let Some(question) = artifact
        .scientific_question
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(question.clone());
    }
    if let Some(method) = artifact
        .method_summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(method.clone());
    }
    if let Some(command) = artifact
        .command
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(command.clone());
    }
    parts.extend(artifact.validation.clone());
    parts.extend(artifact.what_worked.clone());
    parts.extend(artifact.what_failed.clone());
    parts.extend(artifact.outputs.clone());
    parts.extend(artifact.followups.clone());
    if let Some(event_summary) = artifact.event_summary() {
        parts.push(event_summary);
    }
    parts.join(" ")
}

fn artifact_claim_score(artifact: &TaskArtifact, claim: &str) -> f32 {
    score_text_candidate(
        claim,
        &artifact_text_for_grounding(artifact),
        artifact.timestamp_created,
    )
}

fn artifact_has_negative_marker(artifact: &TaskArtifact) -> bool {
    if artifact.supports_claim == Some(false) {
        return true;
    }

    matches!(
        artifact
            .verification_status
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "rejected"
                    | "failed"
                    | "conflicted"
                    | "unsupported"
                    | "insufficient_grounding"
                    | "invalid"
            )
    )
}

fn artifact_supports_claim(artifact: &TaskArtifact, claim: &str, score: f32) -> bool {
    if matches!(
        derive_artifact_trust_tier(artifact),
        TrustTier::SemanticCandidate | TrustTier::CompiledDigestHint
    ) {
        return false;
    }
    if artifact_has_negative_marker(artifact) {
        return false;
    }

    score > 0.0
        || artifact_claim_score(artifact, claim) > 0.0
        || artifact.supports_claim == Some(true)
        || !artifact.validation.is_empty()
}

fn result_metadata(
    artifact: Option<&TaskArtifact>,
    citation: Option<CitationResult>,
) -> (TrustTier, Vec<GroundingRef>, VerificationHint) {
    let trust_tier = derive_chunk_trust_tier(artifact);
    let grounding_refs = artifact
        .map(|artifact| vec![build_grounding_ref(artifact, citation.clone())])
        .unwrap_or_default();
    let verification_hint = verification_hint_for_trust_tier(trust_tier);
    (trust_tier, grounding_refs, verification_hint)
}

fn build_artifact_search_hit(
    artifact: TaskArtifact,
    score: f32,
    matched_chunk: Option<&MemoryChunk>,
) -> ArtifactSearchHit {
    let trust_tier = derive_artifact_trust_tier(&artifact);
    let grounding_refs = vec![build_grounding_ref(
        &artifact,
        matched_chunk.map(build_citation),
    )];
    let summary = artifact.event_summary();
    ArtifactSearchHit {
        artifact_id: artifact.artifact_id.clone(),
        task_id: artifact.task_id.clone(),
        artifact_kind: artifact.artifact_kind.as_str().to_string(),
        artifact_role: artifact.artifact_role.clone(),
        project_id: artifact.project_id.as_option().map(str::to_string),
        thread_id: artifact.thread_id.clone(),
        summary,
        artifact: Some(artifact),
        score,
        matched_chunk_id: matched_chunk.map(|chunk| chunk.chunk_id.to_string()),
        matched_text: matched_chunk.map(|chunk| chunk.text.clone()),
        trust_tier,
        grounding_refs,
        verification_hint: verification_hint_for_trust_tier(trust_tier),
        origin: None,
    }
}

fn artifact_hit_record(hit: &ArtifactSearchHit) -> &TaskArtifact {
    hit.artifact
        .as_ref()
        .expect("internal artifact search hit must carry full artifact before response shaping")
}

async fn artifact_lookup_tenants<S: Store>(
    store: &S,
    primary_tenant: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<TenantId>, McpError> {
    if let Some(project_id) = project_id.filter(|value| !value.trim().is_empty()) {
        return scoped_tenants_for_project(store, primary_tenant, Some(project_id)).await;
    }

    // Without a project_id filter, looking up an artifact by id normally
    // stays within the caller's tenant. The daemon-wide sweep is only
    // available when the operator has opted into the cross-tenant
    // compatibility fallback.
    if !cross_tenant_project_fallback_enabled() {
        return Ok(vec![primary_tenant.clone()]);
    }

    let mut tenants = vec![primary_tenant.clone()];
    let mut seen = HashSet::from([primary_tenant.to_string()]);
    for tenant in store
        .list_tenants()
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        if seen.insert(tenant.to_string()) {
            tenants.push(tenant);
        }
    }
    Ok(tenants)
}

async fn get_artifact_by_id_in_scope<S: Store>(
    store: &S,
    lookup_tenants: &[TenantId],
    artifact_id: &str,
) -> Result<Option<TaskArtifact>, McpError> {
    for tenant in lookup_tenants {
        if let Some(artifact) = store
            .get_task_artifact(tenant, artifact_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            return Ok(Some(artifact));
        }
    }
    Ok(None)
}

async fn resolve_grounding_refs_by_artifact_ids<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    artifact_ids: &[String],
    limit: usize,
) -> Result<Vec<GroundingRef>, McpError> {
    let lookup_tenants = artifact_lookup_tenants(store, tenant_id, project_id).await?;
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for artifact_id in artifact_ids {
        if !seen.insert(artifact_id.clone()) {
            continue;
        }
        if let Some(artifact) =
            get_artifact_by_id_in_scope(store, &lookup_tenants, artifact_id).await?
        {
            refs.push(build_grounding_ref(&artifact, None));
            if refs.len() >= limit {
                break;
            }
        }
    }
    Ok(refs)
}

const TAG_CTX_TIER_HOT: &str = "ctx:tier:hot";
const TAG_CTX_TIER_COLD: &str = "ctx:tier:cold";
const TAG_CTX_DOC: &str = "ctx:doc";
const TAG_CTX_SUBSYSTEM_PREFIX: &str = "ctx:subsystem:";
const TAG_CTX_FILE_PREFIX: &str = "ctx:file:";
const TAG_CTX_TRIGGER_PREFIX: &str = "ctx:trigger:";
const TAG_CTX_AGENT_PREFIX: &str = "ctx:agent:";

fn has_exact_tag(tags: &[String], expected: &str) -> bool {
    tags.iter().any(|tag| tag == expected)
}

fn tag_values(tags: &[String], prefix: &str) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| tag.strip_prefix(prefix).map(str::to_string))
        .collect()
}

fn chunk_matches_subsystem(chunk: &MemoryChunk, subsystem_key: &str) -> bool {
    tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX)
        .iter()
        .any(|value| value == subsystem_key)
}

fn chunk_matches_any_subsystem(chunk: &MemoryChunk, subsystem_keys: &[String]) -> bool {
    if subsystem_keys.is_empty() {
        return true;
    }
    subsystem_keys
        .iter()
        .any(|key| chunk_matches_subsystem(chunk, key))
}

fn chunk_matches_tier(chunk: &MemoryChunk, tier: Option<&str>) -> bool {
    match tier {
        Some("hot") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT),
        Some("cold") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD),
        Some(_) => false,
        None => true,
    }
}

fn is_context_chunk(chunk: &MemoryChunk) -> bool {
    if has_exact_tag(&chunk.tags, TAG_CTX_DOC)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD)
        || !tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX).is_empty()
    {
        return true;
    }

    matches!(
        chunk.chunk_type,
        ChunkType::Doc
            | ChunkType::Research
            | ChunkType::Decision
            | ChunkType::Plan
            | ChunkType::Summary
    )
}

fn chunk_to_result(
    chunk: &MemoryChunk,
    score: f32,
    source_tier: Option<String>,
    artifact: Option<TaskArtifact>,
) -> ChunkResult {
    let citation = Some(build_citation(chunk));
    let (trust_tier, grounding_refs, verification_hint) =
        result_metadata(artifact.as_ref(), citation.clone());
    ChunkResult {
        chunk_id: chunk.chunk_id.to_string(),
        tenant_id: chunk.tenant_id.to_string(),
        project_id: chunk.project_id.as_option().map(str::to_string),
        text: chunk.text.clone(),
        score,
        chunk_type: chunk.chunk_type.to_string(),
        promotion_state: chunk.promotion_state.to_string(),
        source: SourceResult::from(&chunk.source),
        timestamp_created: chunk.timestamp_created,
        tags: chunk.tags.clone(),
        episode_id: extract_episode_id(&chunk.tags),
        citation,
        trust_tier,
        grounding_refs,
        verification_hint,
        source_tier,
        artifact,
        origin: None,
        expanded_siblings: Vec::new(),
    }
}

async fn collect_event_siblings<S: Store>(
    store: &S,
    base_chunk: &MemoryChunk,
    policy: &VisibilityPolicy,
    limit: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    let base_event_tags = event_tags(&base_chunk.tags);
    if base_event_tags.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let base_event_tags: HashSet<String> = base_event_tags.into_iter().collect();
    let mut seen = HashSet::from([base_chunk.chunk_id.to_string()]);
    let mut siblings = Vec::new();
    let now_ms = current_time_ms();
    let page_size = 200;
    let mut offset = 0;

    while siblings.len() < limit {
        let page = store
            .list_chunks(&base_chunk.tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for candidate in page {
            if siblings.len() >= limit {
                break;
            }
            if !seen.insert(candidate.chunk_id.to_string()) {
                continue;
            }
            if candidate.project_id.as_option() != base_chunk.project_id.as_option() {
                continue;
            }
            if !candidate
                .tags
                .iter()
                .any(|tag| base_event_tags.contains(tag))
            {
                continue;
            }

            match store
                .get_with_lifecycle(&candidate.tenant_id, &candidate.chunk_id)
                .await
            {
                Ok(Some(resolved)) => {
                    if policy.is_visible_at(resolved.status, &resolved.lifecycle, now_ms) {
                        siblings.push(resolved.chunk);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        chunk_id = %candidate.chunk_id,
                        tenant_id = %candidate.tenant_id,
                        error = %e,
                        "event sibling expansion: get_with_lifecycle failed, dropping sibling"
                    );
                }
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(siblings)
}

async fn build_chunk_results<S: Store>(
    store: &S,
    scored_chunks: &[(MemoryChunk, f32)],
    source_tier: Option<String>,
    artifacts: &HashMap<String, TaskArtifact>,
    expand_event_siblings: bool,
    visibility_policy: &VisibilityPolicy,
) -> Result<Vec<ChunkResult>, McpError> {
    let mut results = Vec::with_capacity(scored_chunks.len());

    for (chunk, score) in scored_chunks {
        let mut result = chunk_to_result(
            chunk,
            *score,
            source_tier.clone(),
            artifacts.get(&chunk.chunk_id.to_string()).cloned(),
        );

        if expand_event_siblings {
            let sibling_ranked: Vec<(MemoryChunk, f32)> = collect_event_siblings(
                store,
                chunk,
                visibility_policy,
                EVENT_SIBLING_EXPANSION_LIMIT,
            )
            .await?
            .into_iter()
            .map(|sibling| (sibling, 0.0))
            .collect();
            let sibling_artifacts =
                resolve_artifacts_for_ranked_chunks(store, &sibling_ranked).await?;
            result.expanded_siblings = sibling_ranked
                .iter()
                .map(|(sibling, sibling_score)| {
                    chunk_to_result(
                        sibling,
                        *sibling_score,
                        None,
                        sibling_artifacts
                            .get(&sibling.chunk_id.to_string())
                            .cloned(),
                    )
                })
                .collect();
        }

        results.push(result);
    }

    Ok(results)
}

fn estimate_tokens_for_json<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|json| estimate_tokens(&json))
        .unwrap_or(0)
}

fn estimate_tokens(text: &str) -> usize {
    text.len().saturating_add(3) / 4
}

fn compact_snippet(text: &str) -> String {
    const MAX_CHARS: usize = 600;
    if text.len() <= MAX_CHARS {
        return text.to_string();
    }

    let mut end = 0;
    for (idx, _) in text.char_indices() {
        if idx > MAX_CHARS {
            break;
        }
        end = idx;
    }
    text[..end].trim_end().to_string()
}

fn pack_chunk_results(
    results: Vec<ChunkResult>,
    requested_budget: Option<usize>,
) -> (Vec<ChunkResult>, usize, usize, bool) {
    let Some(max_tokens) = requested_budget else {
        return (results, 0, 0, false);
    };
    if results.is_empty() {
        return (results, 0, 0, false);
    }

    let packer = ContextPacker::new(PackerConfig {
        max_tokens,
        ..Default::default()
    });
    let inputs = results
        .iter()
        .filter_map(|result| {
            let chunk_id = ChunkId::parse(&result.chunk_id).ok()?;
            Some(PackerInput {
                chunk_id,
                text: result.text.clone(),
                chunk_type: result
                    .chunk_type
                    .parse::<ChunkType>()
                    .unwrap_or(ChunkType::Other),
                score: result.score,
                hash: result
                    .citation
                    .as_ref()
                    .map(|citation| citation.content_hash.clone())
                    .unwrap_or_else(|| result.chunk_id.clone()),
                embedding: None,
                source_uri: result.source.uri.clone(),
            })
        })
        .collect::<Vec<_>>();
    let packed = packer.pack(inputs);
    let selected = packed
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id.to_string())
        .collect::<HashSet<_>>();
    let original_len = results.len();
    let kept = results
        .into_iter()
        .filter(|result| selected.contains(&result.chunk_id))
        .collect::<Vec<_>>();
    let dropped = original_len.saturating_sub(kept.len());
    let truncated = dropped > 0 || packed.total_tokens >= max_tokens;
    (kept, dropped, packed.duplicates_removed, truncated)
}

fn shape_memory_results(
    results: Vec<ChunkResult>,
    params: &SearchParams,
) -> (Vec<ChunkResult>, Option<BudgetInfo>) {
    let compact_requested = params.compact || params.token_budget.is_some();
    if !compact_requested {
        return (results, None);
    }

    let requested_budget = params.token_budget.or(Some(4000));
    let (mut packed, dropped_result_count, duplicate_drop_count, truncated_by_pack) =
        pack_chunk_results(results, requested_budget);
    let include_text = params.include_text.unwrap_or(true);
    let include_artifact = params.include_artifact.unwrap_or(false);
    let mut omitted_fields = Vec::new();

    for result in &mut packed {
        if include_text {
            result.text = compact_snippet(&result.text);
        } else {
            result.text.clear();
        }
        if !include_artifact {
            result.artifact = None;
        }
        if !result.expanded_siblings.is_empty() {
            result.expanded_siblings.clear();
            omitted_fields.push("expanded_siblings".to_string());
        }
    }

    if !include_text {
        omitted_fields.push("text".to_string());
    }
    if !include_artifact {
        omitted_fields.push("artifact".to_string());
    }
    omitted_fields.sort();
    omitted_fields.dedup();

    let estimated_output_tokens = estimate_tokens_for_json(&packed);
    let truncated = truncated_by_pack
        || requested_budget
            .map(|budget| estimated_output_tokens > budget)
            .unwrap_or(false);
    (
        packed,
        Some(BudgetInfo {
            requested_budget,
            estimated_output_tokens,
            truncated,
            omitted_fields,
            dropped_result_count,
            duplicate_drop_count,
        }),
    )
}

fn pack_artifact_hits(
    results: Vec<ArtifactSearchHit>,
    requested_budget: Option<usize>,
) -> (Vec<ArtifactSearchHit>, usize, usize, bool) {
    let Some(max_tokens) = requested_budget else {
        return (results, 0, 0, false);
    };
    if results.is_empty() {
        return (results, 0, 0, false);
    }

    let mut index_by_chunk_id = HashMap::new();
    let mut inputs = Vec::with_capacity(results.len());
    for (idx, hit) in results.iter().enumerate() {
        let chunk_id = hit
            .matched_chunk_id
            .as_deref()
            .and_then(|id| ChunkId::parse(id).ok())
            .unwrap_or_else(ChunkId::new);
        index_by_chunk_id.insert(chunk_id.to_string(), idx);
        let text = hit
            .matched_text
            .clone()
            .or_else(|| hit.summary.clone())
            .unwrap_or_else(|| hit.artifact_id.clone());
        inputs.push(PackerInput {
            chunk_id,
            text: text.clone(),
            chunk_type: ChunkType::Summary,
            score: hit.score,
            hash: format!("{}:{}", hit.artifact_id, text),
            embedding: None,
            source_uri: None,
        });
    }

    let packer = ContextPacker::new(PackerConfig {
        max_tokens,
        ..Default::default()
    });
    let packed = packer.pack(inputs);
    let selected_indexes = packed
        .chunks
        .iter()
        .filter_map(|chunk| index_by_chunk_id.get(&chunk.chunk_id.to_string()).copied())
        .collect::<HashSet<_>>();
    let original_len = results.len();
    let kept = results
        .into_iter()
        .enumerate()
        .filter_map(|(idx, hit)| selected_indexes.contains(&idx).then_some(hit))
        .collect::<Vec<_>>();
    let dropped = original_len.saturating_sub(kept.len());
    let truncated = dropped > 0 || packed.total_tokens >= max_tokens;
    (kept, dropped, packed.duplicates_removed, truncated)
}

fn shape_artifact_results(
    results: Vec<ArtifactSearchHit>,
    params: &TaskSearchParams,
) -> (Vec<ArtifactSearchHit>, Option<BudgetInfo>) {
    let compact_requested = params.compact || params.token_budget.is_some();
    if !compact_requested {
        return (results, None);
    }

    let requested_budget = params.token_budget.or(Some(4000));
    let (mut packed, dropped_result_count, duplicate_drop_count, truncated_by_pack) =
        pack_artifact_hits(results, requested_budget);
    let include_artifact = params.include_artifact.unwrap_or(false);
    let include_matched_text = params.include_matched_text.unwrap_or(false);
    let mut omitted_fields = Vec::new();

    for hit in &mut packed {
        if !include_artifact {
            hit.artifact = None;
        }
        if include_matched_text {
            if let Some(text) = hit.matched_text.as_mut() {
                *text = compact_snippet(text);
            }
        } else {
            hit.matched_text = None;
        }
    }

    if !include_artifact {
        omitted_fields.push("artifact".to_string());
    }
    if !include_matched_text {
        omitted_fields.push("matched_text".to_string());
    }

    let estimated_output_tokens = estimate_tokens_for_json(&packed);
    let truncated = truncated_by_pack
        || requested_budget
            .map(|budget| estimated_output_tokens > budget)
            .unwrap_or(false);
    (
        packed,
        Some(BudgetInfo {
            requested_budget,
            estimated_output_tokens,
            truncated,
            omitted_fields,
            dropped_result_count,
            duplicate_drop_count,
        }),
    )
}

fn scope_expansion_for(tenant_id: &TenantId, project_id: Option<&str>) -> Option<ScopeExpansion> {
    let project_id = project_id?;
    let aliases = configured_project_aliases(tenant_id, project_id);
    (!aliases.is_empty()).then(|| ScopeExpansion {
        requested_tenant_id: tenant_id.to_string(),
        requested_project_id: project_id.to_string(),
        aliases,
    })
}

fn origin_for_result(
    requested_tenant_id: &TenantId,
    expansion: Option<&ScopeExpansion>,
    origin_tenant_id: &TenantId,
    origin_project_id: Option<String>,
) -> Option<OriginScope> {
    if let Some(alias) = expansion.and_then(|expansion| {
        expansion
            .aliases
            .iter()
            .find(|alias| {
                alias.origin_tenant_id == origin_tenant_id.as_str()
                    && alias.origin_project_id == origin_project_id
            })
            .cloned()
    }) {
        return Some(alias);
    }

    if origin_tenant_id == requested_tenant_id {
        return None;
    }

    Some(OriginScope {
        requested_tenant_id: requested_tenant_id.to_string(),
        origin_tenant_id: origin_tenant_id.to_string(),
        origin_project_id,
        alias_reason: "legacy_cross_tenant_project_fallback".to_string(),
    })
}

fn annotate_chunk_origins(
    results: &mut [ChunkResult],
    requested_tenant_id: &TenantId,
    expansion: Option<&ScopeExpansion>,
) {
    for result in results {
        let Ok(origin_tenant) = TenantId::new(&result.tenant_id) else {
            continue;
        };
        result.origin = origin_for_result(
            requested_tenant_id,
            expansion,
            &origin_tenant,
            result.project_id.clone(),
        );
    }
}

fn annotate_artifact_origins(
    results: &mut [ArtifactSearchHit],
    requested_tenant_id: &TenantId,
    expansion: Option<&ScopeExpansion>,
) {
    for hit in results {
        let Some(artifact) = hit.artifact.as_ref() else {
            continue;
        };
        hit.origin = origin_for_result(
            requested_tenant_id,
            expansion,
            &artifact.tenant_id,
            artifact.project_id.as_option().map(str::to_string),
        );
    }
}

async fn collect_all_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    if max_chunks == 0 {
        return Ok(Vec::new());
    }

    let page_size = 200usize.min(max_chunks.max(1));
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            chunks.push(chunk);
            if chunks.len() >= max_chunks {
                return Ok(chunks);
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(chunks)
}

async fn collect_all_chunks_until_deadline<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    max_chunks: usize,
    timeout: Duration,
) -> Result<(Vec<MemoryChunk>, bool), McpError> {
    if max_chunks == 0 {
        return Ok((Vec::new(), false));
    }

    let started = Instant::now();
    let page_size = 200usize.min(max_chunks.max(1));
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        if started.elapsed() >= timeout {
            return Ok((chunks, true));
        }

        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            chunks.push(chunk);
            if chunks.len() >= max_chunks {
                return Ok((chunks, false));
            }
        }

        if started.elapsed() >= timeout {
            return Ok((chunks, true));
        }

        offset = offset.saturating_add(page_size);
    }

    Ok((chunks, false))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();

    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return text.contains(&pattern);
    }

    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        let slice = &text[cursor..];
        let Some(found) = slice.find(part) else {
            return false;
        };

        if idx == 0 && !pattern.starts_with('*') && found != 0 {
            return false;
        }

        cursor += found + part.len();
    }

    if !pattern.ends_with('*') {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }

    true
}

fn has_active_search_filters(project_id: Option<&str>, filters: &ParsedSearchFilters) -> bool {
    project_id.is_some()
        || filters.chunk_types.is_some()
        || filters.episode_id.is_some()
        || filters.from_ms.is_some()
        || filters.to_ms.is_some()
}

fn adaptive_fetch_k(k: usize, query: &str, has_filters: bool) -> usize {
    if has_filters {
        return 100;
    }

    let token_count = query.split_whitespace().count();
    let is_complex = token_count >= 6 || query.len() >= 80;
    if is_complex {
        return (k.saturating_mul(2)).clamp(1, 100);
    }

    k
}

fn normalize_query_for_repair(query: &str) -> Option<String> {
    let normalized = query
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if normalized.is_empty() {
        return None;
    }

    let original = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized == original.to_lowercase() {
        None
    } else {
        Some(normalized)
    }
}

fn build_episode_summary_text(episode_id: &str, chunks: &[MemoryChunk]) -> String {
    let mut lines = Vec::with_capacity(chunks.len() + 1);
    lines.push(format!(
        "Episode {} summary ({} chunks)",
        episode_id,
        chunks.len()
    ));

    for chunk in chunks {
        let snippet = chunk
            .text
            .replace('\n', " ")
            .chars()
            .take(180)
            .collect::<String>();
        lines.push(format!("- [{}] {}", chunk.chunk_type, snippet));
    }

    lines.join("\n")
}

/// Parse a chunk type string into ChunkType enum
/// Parse the `mode` request param into an `IngestionMode`. Empty / None
/// returns the default (`Document`). Unknown values fail-closed with a
/// clear MCP error so callers learn about the typo immediately.
pub(crate) fn parse_ingestion_mode(
    s: Option<&str>,
) -> Result<crate::types::IngestionMode, McpError> {
    use crate::types::IngestionMode;
    let trimmed = s.map(|x| x.trim()).filter(|x| !x.is_empty());
    match trimmed {
        None => Ok(IngestionMode::default()),
        Some(value) => value.parse::<IngestionMode>().map_err(|e| {
            McpError::InvalidParams(format!(
                "invalid ingestion mode '{}': {}; expected 'conversation' or 'document'",
                value, e
            ))
        }),
    }
}

/// E2: when ingestion_mode is Conversation and the caller did not pass
/// an explicit `review_after_ms`, default to `now() + 14 days` so the
/// chunk surfaces in the review stream after roughly two weeks.
type ResolvedAdmission = PreparedWrite;

fn resolve_write_admission(
    chunk_type: ChunkType,
    text: &str,
    tags: &[String],
    mode: crate::types::IngestionMode,
    requested_expires_at_ms: Option<i64>,
    requested_review_after_ms: Option<i64>,
) -> ResolvedAdmission {
    crate::write_service::prepare_write(PrepareWriteRequest {
        chunk_type,
        text,
        tags,
        ingestion_mode: mode,
        expires_at_ms: requested_expires_at_ms,
        review_after_ms: requested_review_after_ms,
    })
}

fn drop_optional_default_retention_for_in_memory_store<S: Store>(
    store: &S,
    admission: &mut ResolvedAdmission,
) {
    if store.as_persistent().is_none() {
        admission.strip_optional_retention_defaults();
    }
}

fn admission_lifecycle_delta(admission: &ResolvedAdmission) -> LifecycleDelta {
    admission.lifecycle_delta()
}

fn apply_admission_tags(chunk: MemoryChunk, admission: &ResolvedAdmission) -> MemoryChunk {
    admission.apply_to_chunk(chunk)
}

fn admission_decision_string(admission: &ResolvedAdmission) -> String {
    admission.decision().to_string()
}

fn admission_lifecycle_tier_string(admission: &ResolvedAdmission) -> Option<String> {
    admission.lifecycle_tier_name()
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn is_digest_projection_tag(tag: &str) -> bool {
    tag == "task:kind:digest"
        || tag == "task:projection:digest"
        || tag.starts_with("task:digest:")
        || tag.starts_with("task:role:")
}

fn default_content_dedupe_exempt(chunk: &MemoryChunk, caller_tags: &[String]) -> bool {
    caller_tags
        .iter()
        .chain(chunk.tags.iter())
        .any(|tag| is_digest_projection_tag(tag))
        || crate::auto_priority::has_explicit_priority(caller_tags)
}

fn caller_tags_already_preserved(existing: &MemoryChunk, caller_tags: &[String]) -> bool {
    caller_tags
        .iter()
        .all(|tag| existing.tags.iter().any(|existing_tag| existing_tag == tag))
}

async fn find_default_content_duplicate<S: Store>(
    store: &S,
    chunk: &MemoryChunk,
    caller_tags: &[String],
) -> Result<Option<ChunkId>, McpError> {
    if default_content_dedupe_exempt(chunk, caller_tags) {
        return Ok(None);
    }

    let Some(ps) = store.as_persistent() else {
        return Ok(None);
    };

    let project_id = chunk.project_id.as_option().map(|s| s.to_string());
    let hash = sha256_hex(&chunk.text);
    let candidates = ps
        .metadata()
        .list_live_by_content_hash(
            &chunk.tenant_id,
            project_id.as_deref(),
            chunk.chunk_type,
            &hash,
            8,
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    for candidate in candidates {
        let Some(existing) = ps
            .get(&chunk.tenant_id, &candidate.chunk_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        else {
            continue;
        };
        if existing.text == chunk.text
            && existing.chunk_type == chunk.chunk_type
            && existing.project_id == chunk.project_id
            && (chunk.source == Source::empty() || chunk.source == existing.source)
            && caller_tags_already_preserved(&existing, caller_tags)
        {
            return Ok(Some(candidate.chunk_id));
        }
    }

    Ok(None)
}

fn parse_chunk_type(s: &str) -> Result<ChunkType, McpError> {
    match s.to_lowercase().as_str() {
        "code" => Ok(ChunkType::Code),
        "doc" | "scientific" => Ok(ChunkType::Doc), // Map scientific documents to Doc type
        "trace" => Ok(ChunkType::Trace),
        "decision" => Ok(ChunkType::Decision),
        "plan" => Ok(ChunkType::Plan),
        "research" => Ok(ChunkType::Research),
        "message" => Ok(ChunkType::Message),
        "summary" => Ok(ChunkType::Summary),
        "general" | "other" => Ok(ChunkType::Other),
        _ => Err(McpError::InvalidParams(format!(
            "invalid chunk type '{}', must be one of: code, doc, scientific, trace, decision, plan, research, message, summary, general, other",
            s
        ))),
    }
}

/// Resolve and validate `tenant_id` from a tool call.
///
/// Resolution order — the first non-empty value wins:
///   1. explicit value from the call params
///   2. `$MEMD_DEFAULT_TENANT` environment variable
///   3. `~/.memd/default_tenant` file (single line, trimmed)
///   4. the literal string `"default"`
///
/// This is the Phase 2.1 adoption fix: `tenant_id` became optional on
/// every tool schema, and agents that do not know their tenant
/// (typical: a fresh Claude Code session) still end up writing to a
/// stable local tenant instead of failing the call. Operators who run
/// one daemon for multiple logical spaces can pin the default via the
/// env var or file.
///
/// The returned `TenantId` is always validated against
/// `TenantId::validate`, so even operator-supplied defaults cannot
/// escape the storage layout.
fn resolve_tenant_id(explicit: &str) -> Result<TenantId, McpError> {
    fn try_build(value: &str, source: &'static str) -> Option<Result<TenantId, McpError>> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(TenantId::new(trimmed).map_err(|e| {
            McpError::InvalidParams(format!("invalid tenant_id from {}: {}", source, e))
        }))
    }

    if let Some(result) = try_build(explicit, "call params") {
        return result;
    }

    if let Ok(env_value) = std::env::var("MEMD_DEFAULT_TENANT") {
        if let Some(result) = try_build(&env_value, "$MEMD_DEFAULT_TENANT") {
            return result;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let pinned = std::path::PathBuf::from(home)
            .join(".memd")
            .join("default_tenant");
        if let Ok(contents) = std::fs::read_to_string(&pinned) {
            if let Some(result) = try_build(&contents, "~/.memd/default_tenant") {
                return result;
            }
        }
    }

    // Final fallback: a literal "default" tenant. Always valid per
    // `TenantId::validate` (ASCII alphanumeric).
    TenantId::new("default").map_err(|e| McpError::InvalidParams(e.to_string()))
}

/// Legacy alias. Kept so older call sites that want the strict "caller
/// supplied a tenant_id" semantics do not accidentally pick up the
/// file/env/default fallback. Prefer `resolve_tenant_id` for new code.
#[allow(dead_code)]
fn validate_tenant_id(tenant_id: &str) -> Result<TenantId, McpError> {
    resolve_tenant_id(tenant_id)
}

/// Validate chunk_id and return ChunkId
fn validate_chunk_id(chunk_id: &str) -> Result<ChunkId, McpError> {
    ChunkId::parse(chunk_id).map_err(|e| McpError::InvalidParams(e.to_string()))
}

fn validate_identifier(name: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() {
        return Err(McpError::InvalidParams(format!(
            "{} must not be empty",
            name
        )));
    }
    Ok(())
}

fn validate_confidence(confidence: f32) -> Result<(), McpError> {
    if !(0.0..=1.0).contains(&confidence) {
        return Err(McpError::InvalidParams(
            "confidence must be between 0.0 and 1.0".to_string(),
        ));
    }
    Ok(())
}

/// Max number of bytes accepted in a `wiki_page` artifact's `content`
/// field. Concept / entity pages typically run 5-20KB; 256KB is ~10×
/// headroom. The cap is enforced at the MCP boundary so the storage
/// layer never has to defend against giant blobs.
pub(crate) const WIKI_PAGE_MAX_CONTENT_BYTES: usize = 256 * 1024;

/// Max number of bytes accepted in a `wiki_page` `summary` field.
/// Renders as the subtitle under the page title.
pub(crate) const WIKI_PAGE_MAX_SUMMARY_BYTES: usize = 500;

/// Phase 1 of memd-wiki v2: enforce the WikiPage-specific shape at the
/// MCP boundary. The four rules (plan §5 phase 1):
///
/// 1. `related_artifact_ids` is non-empty — concept pages MUST cite
///    something. The Python compiler treats each entry as a grounding
///    reference and renders a "Grounded by" link back to the canonical
///    artifact.
/// 2. `summary` length ≤ `WIKI_PAGE_MAX_SUMMARY_BYTES`. The summary is
///    the page subtitle; long text belongs in `content`.
/// 3. `artifact_role` ∈ {"concept", "entity"}. Keeps the lane typed so
///    the compiler can emit `concepts/<slug>.md` vs `entities/<slug>.md`
///    deterministically.
/// 4. `content` length ≤ `WIKI_PAGE_MAX_CONTENT_BYTES`.
fn validate_wiki_page_params(params: &ArtifactCreateParams) -> Result<(), McpError> {
    if params.related_artifact_ids.is_empty() {
        return Err(McpError::InvalidParams(
            "artifact.create: `wiki_page` requires a non-empty `related_artifact_ids` \
             (grounding refs to canonical artifacts the page cites)"
                .to_string(),
        ));
    }
    for (idx, artifact_id) in params.related_artifact_ids.iter().enumerate() {
        if artifact_id.trim().is_empty() {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `wiki_page.related_artifact_ids[{idx}]` must not be empty"
            )));
        }
    }

    if let Some(summary) = params.summary.as_ref() {
        if summary.len() > WIKI_PAGE_MAX_SUMMARY_BYTES {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `wiki_page.summary` is {} bytes; maximum is {}",
                summary.len(),
                WIKI_PAGE_MAX_SUMMARY_BYTES
            )));
        }
    }

    match params.artifact_role.as_deref() {
        Some(role) if role == "concept" || role == "entity" => {}
        Some(role) => {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `wiki_page.artifact_role` must be \"concept\" or \"entity\"; got {role:?}"
            )));
        }
        None => {
            return Err(McpError::InvalidParams(
                "artifact.create: `wiki_page` requires `artifact_role` = \"concept\" or \"entity\""
                    .to_string(),
            ));
        }
    }

    if let Some(content) = params.content.as_ref() {
        if content.len() > WIKI_PAGE_MAX_CONTENT_BYTES {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `wiki_page.content` is {} bytes; maximum is {}",
                content.len(),
                WIKI_PAGE_MAX_CONTENT_BYTES
            )));
        }
    }

    Ok(())
}

fn dataset_params_to_refs(params: Vec<TaskDatasetRefParams>) -> Result<Vec<DatasetRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for dataset in params {
        validate_identifier("dataset_refs[].name", &dataset.name)?;
        refs.push(DatasetRef {
            name: dataset.name,
            version: dataset.version,
            description: dataset.description,
        });
    }
    Ok(refs)
}

fn entity_params_to_refs(params: Vec<TaskEntityRefParams>) -> Result<Vec<EntityRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for entity in params {
        validate_identifier("entity_refs[].name", &entity.name)?;
        validate_identifier("entity_refs[].entity_type", &entity.entity_type)?;
        refs.push(EntityRef {
            name: entity.name,
            entity_type: entity.entity_type,
            role: entity.role,
        });
    }
    Ok(refs)
}

fn contributor_params_to_refs(
    params: Vec<TaskContributorParams>,
) -> Result<Vec<ContributorRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for contributor in params {
        validate_identifier("contributors[].contributor_id", &contributor.contributor_id)?;
        refs.push(ContributorRef {
            contributor_id: contributor.contributor_id,
            display_name: contributor.display_name,
            role: contributor.role,
            contribution: contributor.contribution,
        });
    }
    Ok(refs)
}

fn params_to_task_provenance(params: Option<TaskProvenanceParams>) -> TaskProvenance {
    params
        .map(|p| TaskProvenance {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_version: p.tool_version,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

fn parse_task_search_filters(
    filters: Option<&TaskSearchFiltersParams>,
) -> Result<TaskSearchFilters, McpError> {
    let Some(filters) = filters else {
        return Ok(TaskSearchFilters::default());
    };

    let artifact_kind = filters
        .artifact_kind
        .as_deref()
        .map(ArtifactKind::from_str)
        .transpose()
        .map_err(McpError::InvalidParams)?;

    Ok(TaskSearchFilters {
        task_id: filters.task_id.clone(),
        artifact_kind,
        status: filters.status.clone(),
        challenge_id: filters.challenge_id.clone(),
        thread_id: filters.thread_id.clone(),
        reply_to_artifact_id: filters.reply_to_artifact_id.clone(),
        artifact_role: filters.artifact_role.clone(),
        dataset_name: filters.dataset_name.clone(),
        dataset_version: filters.dataset_version.clone(),
        entity_name: filters.entity_name.clone(),
        entity_type: filters.entity_type.clone(),
        tool_name: filters.tool_name.clone(),
        project_id: filters.project_id.clone(),
        agent_id: filters.agent_id.clone(),
        session_id: filters.session_id.clone(),
        requested_action: filters.requested_action.clone(),
        verification_status: filters.verification_status.clone(),
        relation_kind: filters.relation_kind.clone(),
    })
}

fn has_active_task_filters(filters: &TaskSearchFilters) -> bool {
    filters.task_id.is_some()
        || filters.artifact_kind.is_some()
        || filters.status.is_some()
        || filters.challenge_id.is_some()
        || filters.thread_id.is_some()
        || filters.reply_to_artifact_id.is_some()
        || filters.artifact_role.is_some()
        || filters.dataset_name.is_some()
        || filters.dataset_version.is_some()
        || filters.entity_name.is_some()
        || filters.entity_type.is_some()
        || filters.tool_name.is_some()
        || filters.project_id.is_some()
        || filters.agent_id.is_some()
        || filters.session_id.is_some()
        || filters.requested_action.is_some()
        || filters.verification_status.is_some()
        || filters.relation_kind.is_some()
}

/// Convert SourceParams to Source
fn params_to_source(params: Option<SourceParams>) -> Source {
    params
        .map(|p| Source {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

/// Format result as MCP content response
fn format_mcp_response<T: Serialize>(result: &T) -> Result<Value, McpError> {
    let json_str = serde_json::to_string(result)
        .map_err(|e| McpError::ToolError(format!("failed to serialize response: {}", e)))?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": json_str
        }]
    }))
}

async fn resolve_artifacts_for_ranked_chunks<S: Store>(
    store: &S,
    ranked: &[(MemoryChunk, f32)],
) -> Result<HashMap<String, TaskArtifact>, McpError> {
    let mut by_tenant: HashMap<String, (TenantId, Vec<ChunkId>)> = HashMap::new();
    for (chunk, _) in ranked {
        by_tenant
            .entry(chunk.tenant_id.to_string())
            .or_insert_with(|| (chunk.tenant_id.clone(), Vec::new()))
            .1
            .push(chunk.chunk_id.clone());
    }

    let mut artifacts = HashMap::new();
    for (_, (tenant_id, chunk_ids)) in by_tenant {
        artifacts.extend(
            store
                .resolve_artifacts_for_chunks(&tenant_id, &chunk_ids)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    Ok(artifacts)
}

fn default_status_for_artifact_kind(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::TaskStart | ArtifactKind::TaskProgress => "in_progress",
        ArtifactKind::RunStart => "started",
        ArtifactKind::RunFinish | ArtifactKind::TaskFinish => "completed",
        ArtifactKind::Digest => "generated",
        ArtifactKind::WikiPage => "authored",
        ArtifactKind::Evidence
        | ArtifactKind::Review
        | ArtifactKind::Revision
        | ArtifactKind::Verification
        | ArtifactKind::Decision => "recorded",
    }
}

fn score_text_candidate(query: &str, text: &str, timestamp_created: i64) -> f32 {
    if query.trim().is_empty() {
        return timestamp_created as f32 / 1_000_000_000_000.0;
    }

    let lower_text = text.to_ascii_lowercase();
    let terms = query
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut score = 0.0f32;
    for term in &terms {
        if lower_text.contains(term) {
            score += 1.0;
        }
    }
    if lower_text.contains(&query.to_ascii_lowercase()) {
        score += 2.0;
    }
    score + (timestamp_created as f32 / 1_000_000_000_000.0)
}

fn sort_ranked_items<T, F>(items: &mut [T], query: &str, score_fn: F)
where
    F: Fn(&T) -> (String, i64, bool),
{
    items.sort_by(|left, right| {
        let (left_text, left_ts, left_explicit) = score_fn(left);
        let (right_text, right_ts, right_explicit) = score_fn(right);
        right_explicit
            .cmp(&left_explicit)
            .then_with(|| {
                score_text_candidate(query, &right_text, right_ts)
                    .partial_cmp(&score_text_candidate(query, &left_text, left_ts))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right_ts.cmp(&left_ts))
    });
}

fn sort_highlight_items(items: &mut [HighlightViewItem], query: &str) {
    items.sort_by(|left, right| {
        if query.trim().is_empty() {
            return right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.timestamp_created.cmp(&left.timestamp_created));
        }

        let left_text = format!("{} {}", left.summary, left.rationale);
        let right_text = format!("{} {}", right.summary, right.rationale);
        let left_rank =
            score_text_candidate(query, &left_text, left.timestamp_created) + left.score;
        let right_rank =
            score_text_candidate(query, &right_text, right.timestamp_created) + right.score;
        right_rank
            .partial_cmp(&left_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
}

async fn scoped_tenants_for_project<S: Store>(
    store: &S,
    primary_tenant: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<TenantId>, McpError> {
    let scopes = project_scopes_for_project(store, primary_tenant, project_id).await?;
    let mut tenants = Vec::new();
    let mut seen = HashSet::new();
    for scope in scopes {
        if seen.insert(scope.tenant_id.to_string()) {
            tenants.push(scope.tenant_id);
        }
    }
    Ok(tenants)
}

async fn project_scopes_for_project<S: Store>(
    store: &S,
    primary_tenant: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<ProjectSearchScope>, McpError> {
    let Some(project_id) = project_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(vec![ProjectSearchScope {
            tenant_id: primary_tenant.clone(),
            project_id: None,
        }]);
    };

    let mut scopes = vec![ProjectSearchScope {
        tenant_id: primary_tenant.clone(),
        project_id: Some(project_id.to_string()),
    }];
    let mut seen = HashSet::from([(primary_tenant.to_string(), Some(project_id.to_string()))]);
    let aliases = configured_project_aliases(primary_tenant, project_id);
    if !aliases.is_empty() {
        for alias in aliases {
            let alias_tenant = TenantId::new(&alias.origin_tenant_id)
                .map_err(|e| McpError::InvalidParams(e.to_string()))?;
            let alias_project = alias
                .origin_project_id
                .as_deref()
                .unwrap_or(project_id)
                .to_string();
            let key = (alias_tenant.to_string(), Some(alias_project.clone()));
            if seen.insert(key) && tenant_has_project(store, &alias_tenant, &alias_project).await? {
                scopes.push(ProjectSearchScope {
                    tenant_id: alias_tenant,
                    project_id: Some(alias_project),
                });
            }
        }
        return Ok(scopes);
    }

    // Default behavior: tenant isolation. Only widen when the operator
    // has explicitly opted into the legacy all-tenant fallback via
    // `server.allow_cross_tenant_project_fallback = true`.
    if !cross_tenant_project_fallback_enabled() {
        return Ok(scopes);
    }

    for tenant in store
        .list_tenants()
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        let key = (tenant.to_string(), Some(project_id.to_string()));
        if !seen.insert(key) {
            continue;
        }
        if tenant_has_project(store, &tenant, project_id).await? {
            warn!(
                primary_tenant = %primary_tenant,
                extra_tenant = %tenant,
                project_id,
                "cross-tenant project fallback widened retrieval beyond the caller's tenant"
            );
            scopes.push(ProjectSearchScope {
                tenant_id: tenant,
                project_id: Some(project_id.to_string()),
            });
        }
    }
    Ok(scopes)
}

async fn tenant_has_project<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: &str,
) -> Result<bool, McpError> {
    if !store
        .list_tasks(tenant, Some(project_id), 1)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
        .is_empty()
    {
        return Ok(true);
    }

    // Legacy fallback is opt-in and diagnostic-oriented. Include raw chunks
    // so memory.search, task/artifact search, and context search agree about
    // which tenants have material for a project.
    const PAGE_SIZE: usize = 200;
    let mut offset = 0;
    loop {
        let chunks = store
            .list_chunks(tenant, PAGE_SIZE, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if chunks.is_empty() {
            return Ok(false);
        }
        if chunks
            .iter()
            .any(|chunk| chunk.project_id.as_option() == Some(project_id))
        {
            return Ok(true);
        }
        offset = offset.saturating_add(PAGE_SIZE);
    }
}

fn merge_scored_chunk_lists(
    scored_lists: Vec<Vec<(MemoryChunk, f32)>>,
    limit: usize,
) -> Vec<(MemoryChunk, f32)> {
    let mut merged = scored_lists.into_iter().flatten().collect::<Vec<_>>();
    merged.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_chunk
                    .timestamp_created
                    .cmp(&left_chunk.timestamp_created)
            })
            .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
    });
    let mut seen = HashSet::new();
    merged
        .into_iter()
        .filter(|(chunk, _)| seen.insert(chunk.chunk_id.clone()))
        .take(limit)
        .collect()
}

/// Build the in-band [`ScopeStatus`] for a finished search. The
/// tenant-wide probe runs only when a project-scoped search came up
/// short of `k`, so the common full-result path costs one
/// `list_tenants` call.
// Search diagnostics consume the same explicit state as the retrieval path.
#[allow(clippy::too_many_arguments)]
async fn scope_status_for_search<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    query: &str,
    k: usize,
    result_count: usize,
    parsed_filters: &ParsedSearchFilters,
    visibility_policy: &VisibilityPolicy,
) -> ScopeStatus {
    let retrieval_mode = store.retrieval_mode().to_string();
    let mut warnings = Vec::new();
    if retrieval_mode == "text_fallback" {
        warnings.push(
            "semantic retrieval unavailable; results come from substring matching at constant \
             score and ranking is unreliable"
                .to_string(),
        );
    }
    if let Ok(tenants) = store.list_tenants().await {
        if !tenants.iter().any(|t| t == tenant_id) {
            warnings.push(format!(
                "tenant '{tenant_id}' has no stored memory on this machine ({} known tenant(s)); \
                 a mistyped --tenant-id returns empty results instead of failing",
                tenants.len()
            ));
        }
    }

    let mut wider_scope_hits = None;
    let mut widen_hint = None;
    if let Some(project) = project_id.filter(|p| !p.trim().is_empty()) {
        if result_count < k && !query.is_empty() {
            if let Ok(scored) = store.search_with_scores(tenant_id, query, k.max(8)).await {
                // Count only rows the real widened search would surface: apply
                // the caller's non-project filters (chunk_type, time, episode)
                // and the visibility policy, exactly as rerunning without
                // --project-id would. Otherwise the hint counts superseded,
                // expired, or filtered rows the agent would never see.
                let scored_len = scored.len();
                let filtered = apply_search_filters(scored, None, parsed_filters, scored_len);
                let filtered_len = filtered.len();
                let visible =
                    apply_visibility_filter(store, filtered, visibility_policy, filtered_len).await;
                let outside = visible
                    .iter()
                    .filter(|(chunk, _)| chunk.project_id.as_option() != Some(project))
                    .count();
                if outside > 0 {
                    wider_scope_hits = Some(outside);
                    widen_hint = Some(format!(
                        "{outside} hit(s) for this query exist in tenant '{tenant_id}' outside \
                         project '{project}'; rerun without --project-id to search tenant-wide"
                    ));
                }
            }
        }
    }

    ScopeStatus {
        tenant_id: tenant_id.to_string(),
        project_id: project_id.map(str::to_string),
        retrieval_mode,
        wider_scope_hits,
        widen_hint,
        warnings,
    }
}

async fn search_with_scores_for_project_scopes<S: Store>(
    store: &S,
    scopes: &[ProjectSearchScope],
    query: &str,
    fetch_k: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let mut lists = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let mut scored = store
            .search_with_scores(&scope.tenant_id, query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if let Some(project_id) = scope.project_id.as_deref() {
            scored.retain(|(chunk, _)| chunk.project_id.as_option() == Some(project_id));
        }
        lists.push(scored);
    }
    Ok(merge_scored_chunk_lists(
        lists,
        fetch_k.saturating_mul(scopes.len().max(1)),
    ))
}

fn exact_rescue_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split_whitespace()
        .filter_map(|term| {
            let cleaned = term
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                .to_ascii_lowercase();
            if cleaned.len() < 3 {
                return None;
            }
            let code_like = cleaned.contains('_')
                || cleaned.contains('-')
                || cleaned.chars().any(|c| c.is_ascii_digit());
            if code_like && seen.insert(cleaned.clone()) {
                Some(cleaned)
            } else {
                None
            }
        })
        .collect()
}

fn lexical_overlap_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(|term| {
            let cleaned = term.trim().to_ascii_lowercase();
            if cleaned.len() < 4 || is_lexical_overlap_stopword(&cleaned) {
                return None;
            }
            if seen.insert(cleaned.clone()) {
                Some(cleaned)
            } else {
                None
            }
        })
        .collect()
}

fn is_lexical_overlap_stopword(term: &str) -> bool {
    matches!(
        term,
        "about"
            | "after"
            | "again"
            | "also"
            | "because"
            | "before"
            | "could"
            | "from"
            | "have"
            | "into"
            | "need"
            | "needs"
            | "only"
            | "over"
            | "should"
            | "than"
            | "that"
            | "their"
            | "there"
            | "these"
            | "this"
            | "those"
            | "when"
            | "where"
            | "with"
            | "would"
    )
}

fn chunk_contains_exact_rescue_term(chunk: &MemoryChunk, terms: &[String]) -> bool {
    let haystack = format!("{} {}", chunk.text, chunk.tags.join(" ")).to_ascii_lowercase();
    terms.iter().any(|term| haystack.contains(term))
}

fn chunk_lexical_overlap_count(chunk: &MemoryChunk, terms: &[String]) -> usize {
    let haystack = format!("{} {}", chunk.text, chunk.tags.join(" ")).to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count()
}

fn lexical_overlap_minimum(terms: &[String]) -> usize {
    if terms.len() >= 8 {
        4
    } else if terms.len() >= 6 {
        3
    } else {
        2
    }
}

fn should_run_lexical_overlap_rescue(scored: &[(MemoryChunk, f32)], query: &str, k: usize) -> bool {
    let terms = lexical_overlap_terms(query);
    if terms.len() < 4 {
        return false;
    }
    let min_overlap = lexical_overlap_minimum(&terms);
    let top = scored.iter().take(k).collect::<Vec<_>>();
    if top.is_empty() {
        return true;
    }
    !top.iter()
        .any(|(chunk, _)| chunk_lexical_overlap_count(chunk, &terms) >= min_overlap)
}

#[cfg(test)]
async fn exact_lexical_candidates_for_tenants<S: Store>(
    store: &S,
    tenants: &[TenantId],
    query: &str,
    project_id_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let terms = exact_rescue_terms(query);
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let scan_limit = if project_id_filter.is_some() {
        EXACT_RESCUE_PROJECT_SCAN_LIMIT
    } else {
        EXACT_RESCUE_GLOBAL_SCAN_LIMIT
    };
    let mut candidates = Vec::new();

    for tenant in tenants {
        let mut offset = 0usize;
        let mut scanned = 0usize;
        loop {
            if scanned >= scan_limit || candidates.len() >= limit {
                break;
            }
            let page_limit = EXACT_RESCUE_PAGE_SIZE.min(scan_limit.saturating_sub(scanned));
            if page_limit == 0 {
                break;
            }
            let chunks = store
                .list_chunks_for_project(tenant, project_id_filter, page_limit, offset)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            if chunks.is_empty() {
                break;
            }
            scanned = scanned.saturating_add(page_limit);
            offset = offset.saturating_add(page_limit);

            for chunk in chunks {
                if project_id_filter.is_some() && chunk.project_id.as_option() != project_id_filter
                {
                    continue;
                }
                if chunk_contains_exact_rescue_term(&chunk, &terms) {
                    candidates.push(chunk);
                    if candidates.len() >= limit {
                        break;
                    }
                }
            }
        }
    }

    Ok(rank_candidate_chunks(candidates, query, limit)
        .into_iter()
        .map(|(chunk, score)| (chunk, score + EXACT_RESCUE_SCORE_BOOST))
        .collect())
}

async fn exact_lexical_candidates_for_project_scopes<S: Store>(
    store: &S,
    scopes: &[ProjectSearchScope],
    query: &str,
    limit: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let terms = exact_rescue_terms(query);
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for scope in scopes {
        let scan_limit = if scope.project_id.is_some() {
            EXACT_RESCUE_PROJECT_SCAN_LIMIT
        } else {
            EXACT_RESCUE_GLOBAL_SCAN_LIMIT
        };
        let mut offset = 0usize;
        let mut scanned = 0usize;
        loop {
            if scanned >= scan_limit || candidates.len() >= limit {
                break;
            }
            let page_limit = EXACT_RESCUE_PAGE_SIZE.min(scan_limit.saturating_sub(scanned));
            if page_limit == 0 {
                break;
            }
            let chunks = store
                .list_chunks_for_project(
                    &scope.tenant_id,
                    scope.project_id.as_deref(),
                    page_limit,
                    offset,
                )
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            if chunks.is_empty() {
                break;
            }
            scanned = scanned.saturating_add(page_limit);
            offset = offset.saturating_add(page_limit);

            for chunk in chunks {
                if let Some(project_id) = scope.project_id.as_deref() {
                    if chunk.project_id.as_option() != Some(project_id) {
                        continue;
                    }
                }
                if chunk_contains_exact_rescue_term(&chunk, &terms) {
                    candidates.push(chunk);
                    if candidates.len() >= limit {
                        break;
                    }
                }
            }
        }
    }

    Ok(rank_candidate_chunks(candidates, query, limit)
        .into_iter()
        .map(|(chunk, score)| (chunk, score + EXACT_RESCUE_SCORE_BOOST))
        .collect())
}

async fn lexical_overlap_candidates_for_project_scopes<S: Store>(
    store: &S,
    scopes: &[ProjectSearchScope],
    query: &str,
    limit: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    if limit == 0 || !scopes.iter().any(|scope| scope.project_id.is_some()) {
        return Ok(Vec::new());
    }
    let terms = lexical_overlap_terms(query);
    if terms.len() < 4 {
        return Ok(Vec::new());
    }
    let min_overlap = lexical_overlap_minimum(&terms);
    let candidate_cap = limit
        .saturating_mul(20)
        .clamp(limit, LEXICAL_OVERLAP_MAX_CANDIDATES);
    let mut candidates = Vec::new();

    for scope in scopes {
        let Some(project_id) = scope.project_id.as_deref() else {
            continue;
        };
        let mut offset = 0usize;
        let mut scanned = 0usize;
        while scanned < LEXICAL_OVERLAP_PROJECT_SCAN_LIMIT && candidates.len() < candidate_cap {
            let page_limit =
                EXACT_RESCUE_PAGE_SIZE.min(LEXICAL_OVERLAP_PROJECT_SCAN_LIMIT - scanned);
            if page_limit == 0 {
                break;
            }
            let chunks = store
                .list_chunks_for_project(&scope.tenant_id, Some(project_id), page_limit, offset)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            if chunks.is_empty() {
                break;
            }
            scanned = scanned.saturating_add(page_limit);
            offset = offset.saturating_add(page_limit);

            for chunk in chunks {
                if chunk.project_id.as_option() != Some(project_id) {
                    continue;
                }
                if chunk_lexical_overlap_count(&chunk, &terms) >= min_overlap {
                    candidates.push(chunk);
                    if candidates.len() >= candidate_cap {
                        break;
                    }
                }
            }
        }
    }

    Ok(rank_candidate_chunks(candidates, query, limit)
        .into_iter()
        .map(|(chunk, score)| (chunk, score + LEXICAL_OVERLAP_SCORE_BOOST))
        .collect())
}

async fn search_with_tier_info_for_project_scopes<S: Store>(
    store: &S,
    scopes: &[ProjectSearchScope],
    query: &str,
    fetch_k: usize,
) -> Result<(Vec<(MemoryChunk, f32)>, Option<TieredTiming>), McpError> {
    if scopes.len() == 1 {
        let (mut scored, timing) = store
            .search_with_tier_info(&scopes[0].tenant_id, query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if let Some(project_id) = scopes[0].project_id.as_deref() {
            scored.retain(|(chunk, _)| chunk.project_id.as_option() == Some(project_id));
        }
        return Ok((scored, timing));
    }

    let mut lists = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let (mut results, _) = store
            .search_with_tier_info(&scope.tenant_id, query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if let Some(project_id) = scope.project_id.as_deref() {
            results.retain(|(chunk, _)| chunk.project_id.as_option() == Some(project_id));
        }
        lists.push(results);
    }
    Ok((
        merge_scored_chunk_lists(lists, fetch_k.saturating_mul(scopes.len().max(1))),
        None,
    ))
}

fn finalize_artifact_for_storage(artifact: &mut TaskArtifact) {
    artifact.promotion_state = derive_artifact_promotion_state(artifact);
}

/// Promote an artifact to `PromotionState::Verified` when, and only when,
/// it countersigns a prior artifact written by a distinct agent.
///
/// The rules:
/// 1. The artifact must be of a review-style kind (`Review`, `Revision`,
///    `Verification`, or `Decision`). Other kinds stay `Canonical`.
/// 2. It must reply to a canonical parent artifact (`reply_to_artifact_id`
///    resolves, and the parent is NOT a digest).
/// 3. The current artifact's `agent_id` must be non-empty AND differ
///    from the parent's `agent_id`. This is the "distinct writer"
///    requirement — it prevents a single agent from stamping its own
///    work as verified.
/// 4. The current artifact must explicitly support the parent's claim
///    (`supports_claim = Some(true)`). `supports_claim = Some(false)`
///    (an explicit rejection) or `None` (no opinion) does NOT promote.
///
/// When all four hold, set `promotion_state = Verified` so
/// `derive_artifact_trust_tier` returns `VerifiedRecord`. Otherwise
/// leave the canonical tier that `finalize_artifact_for_storage`
/// assigned.
pub(crate) async fn promote_if_countersigned<S: Store>(
    store: &S,
    artifact: &mut TaskArtifact,
) -> Result<(), McpError> {
    use crate::types::PromotionState;

    // Rule 1: only review-style kinds are even eligible.
    let eligible = matches!(
        artifact.artifact_kind,
        ArtifactKind::Review
            | ArtifactKind::Revision
            | ArtifactKind::Verification
            | ArtifactKind::Decision
    );
    if !eligible {
        return Ok(());
    }

    // Rule 4: explicit support is required.
    if artifact.supports_claim != Some(true) {
        return Ok(());
    }

    // Rule 3a: current writer must be identified.
    let Some(my_agent) = artifact
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };

    // Rule 2: the reply-to parent must resolve.
    let Some(reply_to) = artifact.reply_to_artifact_id.as_deref() else {
        return Ok(());
    };

    let parent = store
        .get_task_artifact(&artifact.tenant_id, reply_to)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let Some(parent) = parent else {
        return Ok(());
    };

    // Rule 2 (cont): digest parents do not count as canonical trust anchors.
    if parent.artifact_kind == ArtifactKind::Digest {
        return Ok(());
    }

    // Rule 3b: distinct writer.
    let parent_agent = parent
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match parent_agent {
        Some(other) if other != my_agent => {
            artifact.promotion_state = PromotionState::Verified;
            info!(
                artifact_id = %artifact.artifact_id,
                parent_id = %parent.artifact_id,
                my_agent,
                parent_agent = other,
                "promoted artifact to VerifiedRecord via distinct-writer countersignature"
            );
        }
        _ => {
            // Either parent is anonymous, or it's the same writer.
            // Neither case promotes trust.
        }
    }

    Ok(())
}

fn digest_artifacts_equivalent(existing: &TaskArtifact, candidate: &TaskArtifact) -> bool {
    if existing.artifact_kind != ArtifactKind::Digest
        || candidate.artifact_kind != ArtifactKind::Digest
    {
        return false;
    }

    let mut lhs = existing.clone();
    let mut rhs = candidate.clone();
    lhs.timestamp_created = 0;
    rhs.timestamp_created = 0;
    lhs.timestamp_observed = None;
    rhs.timestamp_observed = None;
    // `source_updated_at_ms` is derived from the artifacts loaded to
    // build the digest. A refresh can see the previous digest wrapper as
    // the newest artifact even when the digest content is unchanged, so
    // it is not part of the idempotence identity.
    lhs.source_updated_at_ms = None;
    rhs.source_updated_at_ms = None;
    lhs == rhs
}

fn contains_empty_generated_digest_summary(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("contains 0 ranked lessons")
        || text.contains("contains 0 recent failure summaries")
        || text.contains("contains 0 explicit or inferred decisions")
        || text.contains("contains 0 evidence highlights")
        || text.contains(
            "has 0 active tasks, 0 recent completed tasks, 0 recent failures, 0 decisions, and 0 evidence highlights",
        )
}

fn is_empty_generated_digest_artifact(artifact: &TaskArtifact) -> bool {
    if artifact.artifact_kind != ArtifactKind::Digest
        || artifact.status.as_deref() != Some("generated")
    {
        return false;
    }

    let Some(role) = artifact.artifact_role.as_deref() else {
        return false;
    };
    if !matches!(
        role,
        DIGEST_ROLE_PROJECT_BRIEF
            | DIGEST_ROLE_FAILURE_LIBRARY
            | DIGEST_ROLE_DECISION_LIBRARY
            | DIGEST_ROLE_EVIDENCE_LIBRARY
            | DIGEST_ROLE_HIGHLIGHT_LIBRARY
    ) {
        return false;
    }

    let no_payload = artifact.blockers.is_empty()
        && artifact.what_worked.is_empty()
        && artifact.what_failed.is_empty()
        && artifact.validation.is_empty()
        && artifact.uncertainty.is_empty()
        && artifact.followups.is_empty()
        && artifact.expected_outputs.is_empty()
        && artifact.related_artifact_ids.is_empty();
    no_payload
        && artifact
            .summary
            .as_deref()
            .map(contains_empty_generated_digest_summary)
            .unwrap_or(false)
}

async fn persist_digest_artifact<S: Store>(
    store: &S,
    mut artifact: TaskArtifact,
) -> Result<TaskArtifact, McpError> {
    finalize_artifact_for_storage(&mut artifact);
    if is_empty_generated_digest_artifact(&artifact) {
        debug!(
            artifact_id = %artifact.artifact_id,
            role = ?artifact.artifact_role,
            project_id = ?artifact.project_id.as_option(),
            "skipping persistence for empty generated digest"
        );
        return Ok(artifact);
    }
    if let Some(existing) = store
        .get_task_artifact(&artifact.tenant_id, &artifact.artifact_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        if digest_artifacts_equivalent(&existing, &artifact) {
            return Ok(existing);
        }
    }
    let projections = build_task_projections(&artifact);
    store
        .add_task_artifact(artifact.clone(), projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    Ok(artifact)
}

async fn load_task_views<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskResumeView>, McpError> {
    let tenants = scoped_tenants_for_project(store, tenant_id, project_id).await?;
    let mut views = Vec::new();
    for tenant in tenants {
        let tasks = store
            .list_tasks(&tenant, project_id, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for task in tasks {
            let artifacts = store
                .list_task_artifacts(&tenant, &task.task_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            views.push(build_task_resume_view(task, &artifacts));
        }
    }
    Ok(views)
}

async fn load_project_artifacts<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskArtifact>, McpError> {
    let mut artifacts = Vec::new();
    let tenants = scoped_tenants_for_project(store, tenant_id, project_id).await?;
    for tenant in tenants {
        let tasks = store
            .list_tasks(&tenant, project_id, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for task in tasks {
            artifacts.extend(
                store
                    .list_task_artifacts(&tenant, &task.task_id)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?,
            );
        }
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.timestamp_created));
    Ok(artifacts)
}

async fn ensure_project_brief_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: &str,
    include_related_projects: bool,
) -> Result<(TaskArtifact, ProjectBriefView), McpError> {
    let task_views = load_task_views(store, tenant_id, Some(project_id), 200).await?;
    let same_project_artifacts =
        load_project_artifacts(store, tenant_id, Some(project_id), 200).await?;
    let recent_failures = infer_failure_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();
    let recent_decisions = infer_decision_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();
    let evidence_highlights = infer_evidence_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();

    let related_projects = if include_related_projects {
        store
            .list_tasks(tenant_id, None, 200)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
            .into_iter()
            .filter_map(|task| task.project_id.as_option().map(str::to_string))
            .filter(|candidate| candidate != project_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(5)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let brief = build_project_brief_view(
        tenant_id,
        project_id,
        task_views,
        recent_failures.clone(),
        recent_decisions.clone(),
        evidence_highlights.clone(),
        related_projects,
    );
    let artifact =
        persist_digest_artifact(store, build_project_brief_digest_artifact(&brief)).await?;
    Ok((artifact, brief))
}

async fn ensure_task_resume_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    task_id: &str,
) -> Result<(TaskArtifact, TaskResumeView), McpError> {
    let mut task = store
        .list_tasks(tenant_id, None, 500)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
        .into_iter()
        .find(|task| task.task_id == task_id);
    // Fall back to a daemon-wide scan ONLY when the cross-tenant fallback
    // is explicitly enabled. Otherwise a missing task in the caller's
    // tenant means "not found here" — the previous unconditional sweep
    // leaked the existence (and full 500-task listing) of every other
    // tenant on the daemon whenever a task_id was unknown.
    if task.is_none() && cross_tenant_project_fallback_enabled() {
        for other_tenant in store
            .list_tenants()
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            if &other_tenant == tenant_id {
                continue;
            }
            task = store
                .list_tasks(&other_tenant, None, 500)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?
                .into_iter()
                .find(|task| task.task_id == task_id);
            if task.is_some() {
                warn!(
                    primary_tenant = %tenant_id,
                    extra_tenant = %other_tenant,
                    task_id,
                    "task.resume digest resolved via cross-tenant fallback"
                );
                break;
            }
        }
    }
    let task = task.ok_or_else(|| McpError::ToolError("task not found".to_string()))?;
    let artifacts = store
        .list_task_artifacts(&task.tenant_id, task_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let resume = build_task_resume_view(task, &artifacts);
    let artifact =
        persist_digest_artifact(store, build_task_resume_digest_artifact(&resume)).await?;
    Ok((artifact, resume))
}

fn build_scope_key(project_id: Option<&str>, tenant_id: &TenantId, suffix: &str) -> String {
    project_id
        .map(|project_id| format!("project:{}:{}", project_id, suffix))
        .unwrap_or_else(|| format!("tenant:{}:{}", tenant_id, suffix))
}

fn digest_source_artifacts(artifacts: &[TaskArtifact]) -> Vec<TaskArtifact> {
    artifacts
        .iter()
        .filter(|artifact| artifact.artifact_kind != ArtifactKind::Digest)
        .cloned()
        .collect()
}

fn latest_source_update_ms(artifacts: &[TaskArtifact]) -> i64 {
    artifacts
        .iter()
        .map(|artifact| artifact.timestamp_created)
        .max()
        .unwrap_or(0)
}

async fn ensure_failure_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<FailureViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let source_artifacts = digest_source_artifacts(&artifacts);
    let failures = infer_failure_items(&source_artifacts);
    let source_updated_at_ms = latest_source_update_ms(&source_artifacts);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(ProjectId::from),
        DIGEST_ROLE_FAILURE_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_FAILURE_LIBRARY),
        format!(
            "Failure library for {} contains {} recent failure summaries.",
            project_id.unwrap_or(tenant_id.as_str()),
            failures.len()
        ),
        failures
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        Vec::new(),
        failures
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, failures))
}

async fn ensure_decision_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<DecisionViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let source_artifacts = digest_source_artifacts(&artifacts);
    let decisions = infer_decision_items(&source_artifacts);
    let source_updated_at_ms = latest_source_update_ms(&source_artifacts);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(ProjectId::from),
        DIGEST_ROLE_DECISION_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_DECISION_LIBRARY),
        format!(
            "Decision library for {} contains {} explicit or inferred decisions.",
            project_id.unwrap_or(tenant_id.as_str()),
            decisions.len()
        ),
        Vec::new(),
        decisions
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        decisions
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, decisions))
}

async fn ensure_evidence_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<EvidenceViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let source_artifacts = digest_source_artifacts(&artifacts);
    let evidence = infer_evidence_items(&source_artifacts);
    let source_updated_at_ms = latest_source_update_ms(&source_artifacts);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(ProjectId::from),
        DIGEST_ROLE_EVIDENCE_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_EVIDENCE_LIBRARY),
        format!(
            "Evidence library for {} contains {} evidence highlights.",
            project_id.unwrap_or(tenant_id.as_str()),
            evidence.len()
        ),
        Vec::new(),
        evidence
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        evidence
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, evidence))
}

async fn ensure_highlight_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<HighlightViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let source_artifacts = digest_source_artifacts(&artifacts);
    let highlights = infer_highlight_items(&source_artifacts);
    let source_updated_at_ms = latest_source_update_ms(&source_artifacts);
    let task_id_by_artifact_id = source_artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact.task_id.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let covered_task_ids = highlights
        .iter()
        .flat_map(|item| {
            let mut task_ids = item
                .supporting_artifact_ids
                .iter()
                .filter_map(|artifact_id| task_id_by_artifact_id.get(artifact_id.as_str()))
                .map(|task_id| (*task_id).to_string())
                .collect::<Vec<_>>();
            if task_ids.is_empty() && !item.task_id.is_empty() {
                task_ids.push(item.task_id.clone());
            }
            task_ids
        })
        .filter(|id| !id.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    let mut summary = format!(
        "Highlight library for {} contains {} ranked lessons with future-agent uplift.",
        project_id.unwrap_or(tenant_id.as_str()),
        highlights.len()
    );
    if !covered_task_ids.is_empty() {
        // memory.md uses this `Covers tasks:` line to suppress raw
        // task_finish chunks already represented in the digest.
        summary.push_str("\nCovers tasks: ");
        let joined = covered_task_ids
            .iter()
            .map(|id| format!("task:id:{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        summary.push_str(&joined);
    }
    let warning_highlights = highlights
        .iter()
        .filter(|item| item.category == "warning")
        .map(|item| item.summary.clone())
        .take(12)
        .collect::<Vec<_>>();
    let validated_highlights = highlights
        .iter()
        .filter(|item| item.category != "warning")
        .map(|item| item.summary.clone())
        .take(12)
        .collect::<Vec<_>>();
    let related_artifact_ids = highlights
        .iter()
        .flat_map(|item| item.supporting_artifact_ids.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(ProjectId::from),
        DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_HIGHLIGHT_LIBRARY),
        summary,
        warning_highlights,
        validated_highlights,
        Vec::new(),
        related_artifact_ids,
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, highlights))
}

/// Phase 3.4 sweeper: drain the writer-side dirty tracker and
/// regenerate the flagged digests. Returns the number of (scope,
/// role) pairs successfully regenerated. Errors for individual
/// scopes are logged but do not abort the whole sweep — they stay
/// flagged as dirty by virtue of having been drained, so a future
/// sweep will pick them up once the caller re-marks.
///
/// Called from `memory.compact` so operators have an explicit way to
/// force the refresh. A future phase can run this from a background
/// task on a timer.
pub(crate) async fn sweep_dirty_digests<S: Store>(store: &S) -> usize {
    let drained = crate::task_memory::digest_dirty::global().drain_dirty();
    if drained.is_empty() {
        return 0;
    }
    info!(pending = drained.len(), "Phase 3.4: sweeping dirty digests");

    let mut rebuilt = 0usize;
    for key in drained {
        let tenant = match TenantId::new(&key.tenant_id) {
            Ok(t) => t,
            Err(err) => {
                warn!(
                    tenant_id = %key.tenant_id,
                    error = %err,
                    "skipping dirty digest: invalid tenant_id"
                );
                continue;
            }
        };

        let result = match key.role.as_str() {
            crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY => {
                ensure_evidence_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY => {
                ensure_decision_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_FAILURE_LIBRARY => {
                ensure_failure_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY => {
                ensure_highlight_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF => match key.project_id.as_deref() {
                Some(project_id) => ensure_project_brief_digest(store, &tenant, project_id, true)
                    .await
                    .map(|_| ()),
                None => {
                    warn!(
                        role = %key.role,
                        tenant_id = %tenant,
                        "project_brief digest requires project_id; skipping"
                    );
                    continue;
                }
            },
            _ => {
                warn!(role = %key.role, "unknown digest role in dirty tracker");
                continue;
            }
        };

        match result {
            Ok(_) => rebuilt += 1,
            Err(err) => {
                // Codex follow-up on 3.4 retry semantics: a failed
                // regeneration used to be silently lost when the
                // drain consumed the key. Re-mark the key so the
                // next sweep will retry; otherwise a transient error
                // (temporary lock contention, disk blip) would leave
                // the digest stale forever.
                warn!(
                    role = %key.role,
                    tenant_id = %tenant,
                    project_id = ?key.project_id,
                    error = %err,
                    "digest sweeper failed to regenerate; re-marking for retry"
                );
                crate::task_memory::digest_dirty::global().mark_dirty(
                    crate::task_memory::digest_dirty::DigestDirtyKey {
                        tenant_id: key.tenant_id.clone(),
                        project_id: key.project_id.clone(),
                        role: key.role.clone(),
                    },
                );
            }
        }
    }
    rebuilt
}

async fn rebuild_requested_digests<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    modes: &[QueryMode],
) -> Result<Vec<String>, McpError> {
    let requested = if modes.is_empty() {
        vec![
            QueryMode::BriefProject,
            QueryMode::FindFailures,
            QueryMode::FindDecisions,
            QueryMode::FindEvidence,
            QueryMode::FindHighlights,
        ]
    } else {
        modes.to_vec()
    };

    let mut artifact_ids = Vec::new();
    for mode in requested {
        match mode {
            QueryMode::BriefProject => {
                if let Some(project_id) = project_id {
                    artifact_ids.push(
                        ensure_project_brief_digest(store, tenant_id, project_id, true)
                            .await?
                            .0
                            .artifact_id,
                    );
                }
            }
            QueryMode::FindFailures => artifact_ids.push(
                ensure_failure_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindDecisions => artifact_ids.push(
                ensure_decision_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindEvidence => artifact_ids.push(
                ensure_evidence_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindHighlights => artifact_ids.push(
                ensure_highlight_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::Generic | QueryMode::ResumeTask => {}
        }
    }
    artifact_ids.sort();
    artifact_ids.dedup();
    Ok(artifact_ids)
}

async fn collect_candidate_chunk_ids<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    filters_list: Vec<TaskSearchFilters>,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for filters in filters_list {
        let ids = store
            .search_task_projection_chunk_ids(tenant_id, &filters, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn candidate_chunk_ids_for_mode<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    mode: QueryMode,
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut filters_list = Vec::new();
    match mode {
        QueryMode::Generic => {}
        QueryMode::BriefProject => {
            if let Some(project_id) = filters.project_id.as_deref() {
                let _ = ensure_project_brief_digest(store, tenant_id, project_id, true).await?;
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(ArtifactKind::Digest),
                    artifact_role: Some(DIGEST_ROLE_PROJECT_BRIEF.to_string()),
                    project_id: Some(project_id.to_string()),
                    ..Default::default()
                });
                filters_list.push(TaskSearchFilters {
                    project_id: Some(project_id.to_string()),
                    ..Default::default()
                });
            }
        }
        QueryMode::ResumeTask => {
            if let Some(task_id) = filters.task_id.as_deref() {
                let _ = ensure_task_resume_digest(store, tenant_id, task_id).await?;
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(ArtifactKind::Digest),
                    artifact_role: Some(DIGEST_ROLE_TASK_RESUME.to_string()),
                    task_id: Some(task_id.to_string()),
                    ..Default::default()
                });
                filters_list.push(TaskSearchFilters {
                    task_id: Some(task_id.to_string()),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindFailures => {
            let _ = ensure_failure_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_FAILURE_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::TaskFinish,
                ArtifactKind::TaskProgress,
                ArtifactKind::Digest,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindDecisions => {
            let _ = ensure_decision_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_DECISION_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::Decision,
                ArtifactKind::Verification,
                ArtifactKind::TaskFinish,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindEvidence => {
            let _ = ensure_evidence_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_EVIDENCE_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [ArtifactKind::Evidence, ArtifactKind::TaskFinish] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindHighlights => {
            let _ =
                ensure_highlight_library_digest(store, tenant_id, filters.project_id.as_deref())
                    .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_HIGHLIGHT_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::TaskFinish,
                ArtifactKind::Verification,
                ArtifactKind::Decision,
                ArtifactKind::Review,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
    }

    collect_candidate_chunk_ids(store, tenant_id, filters_list, limit).await
}

async fn candidate_chunk_ids_for_tenants_and_mode<S: Store>(
    store: &S,
    tenants: &[TenantId],
    mode: QueryMode,
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tenant in tenants {
        let ids = candidate_chunk_ids_for_mode(store, tenant, mode, filters, limit).await?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn search_task_projection_chunk_ids_for_tenants<S: Store>(
    store: &S,
    tenants: &[TenantId],
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tenant in tenants {
        let ids = store
            .search_task_projection_chunk_ids(tenant, filters, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn summary_preferred_results_for_project_scopes<S: Store>(
    store: &S,
    scopes: &[ProjectSearchScope],
    query: &str,
    mode: QueryMode,
    limit: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let has_project_scope = scopes.iter().any(|scope| scope.project_id.is_some());
    let modes = if mode != QueryMode::Generic {
        vec![mode]
    } else if has_project_scope {
        vec![
            QueryMode::BriefProject,
            QueryMode::FindFailures,
            QueryMode::FindDecisions,
            QueryMode::FindEvidence,
            QueryMode::FindHighlights,
        ]
    } else {
        Vec::new()
    };

    if modes.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut lists = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let mut all_ids = Vec::new();
        let mut seen = HashSet::new();
        for mode in &modes {
            let ids = candidate_chunk_ids_for_mode(
                store,
                &scope.tenant_id,
                *mode,
                &TaskSearchFilters {
                    project_id: scope.project_id.clone(),
                    ..Default::default()
                },
                limit.saturating_mul(4),
            )
            .await?;
            for id in ids {
                if seen.insert(id.clone()) {
                    all_ids.push(id);
                }
            }
        }
        let mut ranked = store
            .rerank_chunks_for_query(&scope.tenant_id, query, &all_ids, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        ranked.retain(|(chunk, _)| !is_empty_generated_digest_chunk(chunk));
        lists.push(ranked);
    }
    Ok(merge_scored_chunk_lists(
        lists,
        limit.saturating_mul(scopes.len().max(1)),
    ))
}

fn is_empty_generated_digest_chunk(chunk: &MemoryChunk) -> bool {
    let generated = chunk.tags.iter().any(|tag| tag == "task:status:generated");
    let digest_like = chunk
        .tags
        .iter()
        .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"));
    if !generated || !digest_like {
        return false;
    }

    contains_empty_generated_digest_summary(&chunk.text)
}

fn is_generated_digest_projection_chunk(chunk: &MemoryChunk) -> bool {
    let generated = chunk.tags.iter().any(|tag| tag == "task:status:generated");
    let digest_like = chunk.tags.iter().any(|tag| {
        tag == "task:kind:digest"
            || tag == "task:projection:digest"
            || tag.starts_with("task:role:")
            || tag.starts_with("task:digest:")
    });
    generated && digest_like
}

fn suppress_generated_digest_projection_chunks(
    scored: Vec<(MemoryChunk, f32)>,
) -> Vec<(MemoryChunk, f32)> {
    scored
        .into_iter()
        .filter(|(chunk, _)| !is_generated_digest_projection_chunk(chunk))
        .collect()
}

/// Collapse ranked chunks that share a `source.uri` to the best-ranked one.
///
/// Large documents are stored as several chunks that all carry the parent
/// URI; without collapsing, fragments of one document crowd the top-k while
/// other relevant sources never surface. Input must already be sorted best
/// first (it is: callers pass the merged, score-sorted candidate list), so
/// keeping the first occurrence keeps the best. Chunks without a source URI
/// are never collapsed.
fn dedupe_scored_chunks_by_source_uri(scored: Vec<(MemoryChunk, f32)>) -> Vec<(MemoryChunk, f32)> {
    let mut seen = HashSet::new();
    scored
        .into_iter()
        .filter(|(chunk, _)| match chunk.source.uri.as_deref() {
            Some(uri) if !uri.is_empty() => seen.insert(uri.to_string()),
            _ => true,
        })
        .collect()
}

fn merge_preferred_and_raw(
    preferred: Vec<(MemoryChunk, f32)>,
    raw: Vec<(MemoryChunk, f32)>,
    limit: usize,
) -> Vec<(MemoryChunk, f32)> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for (chunk, score) in preferred {
        if seen.insert(chunk.chunk_id.clone()) {
            merged.push((chunk, score + 10.0));
        }
    }
    for (chunk, score) in raw {
        if seen.insert(chunk.chunk_id.clone()) {
            merged.push((chunk, score));
        }
    }
    merged.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_chunk
                    .timestamp_created
                    .cmp(&left_chunk.timestamp_created)
            })
            .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
    });
    merged.truncate(limit);
    merged
}

struct CommonArtifactFields {
    project_id: Option<String>,
    parent_task_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    status: Option<String>,
    artifact_role: Option<String>,
    challenge_id: Option<String>,
    thread_id: Option<String>,
    reply_to_artifact_id: Option<String>,
    relation_kind: Option<String>,
    dataset_refs: Vec<DatasetRef>,
    entity_refs: Vec<EntityRef>,
    contributors: Vec<ContributorRef>,
    provenance: TaskProvenance,
}

fn apply_common_artifact_fields(artifact: &mut TaskArtifact, fields: CommonArtifactFields) {
    artifact.project_id = ProjectId::from(fields.project_id);
    artifact.parent_task_id = fields.parent_task_id;
    artifact.agent_id = fields.agent_id;
    artifact.session_id = fields.session_id;
    artifact.status =
        Some(fields.status.unwrap_or_else(|| {
            default_status_for_artifact_kind(artifact.artifact_kind).to_string()
        }));
    artifact.artifact_role = fields.artifact_role;
    artifact.challenge_id = fields.challenge_id;
    artifact.thread_id = fields.thread_id;
    artifact.reply_to_artifact_id = fields.reply_to_artifact_id;
    artifact.relation_kind = fields.relation_kind;
    artifact.dataset_refs = fields.dataset_refs;
    artifact.entity_refs = fields.entity_refs;
    artifact.contributors = fields.contributors;
    artifact.provenance = fields.provenance;
    artifact.tool_name = artifact
        .provenance
        .tool_name
        .clone()
        .or_else(|| artifact.tool_name.clone());
    artifact.tool_version = artifact
        .provenance
        .tool_version
        .clone()
        .or_else(|| artifact.tool_version.clone());
}

async fn collect_episode_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    episode_id: &str,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    let page_size = 200usize;
    let mut offset = 0usize;
    let mut episode_chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            if extract_episode_id(&chunk.tags).as_deref() == Some(episode_id) {
                episode_chunks.push(chunk);
                if episode_chunks.len() >= max_chunks {
                    return Ok(episode_chunks);
                }
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(episode_chunks)
}

fn record_search_usage_event<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    params: &SearchParams,
    result_count: usize,
) {
    if params.suppress_usage_event {
        return;
    }

    let detail = json!({
        "q_len": params.query.chars().count(),
        "k": params.k,
        "q_hash": query_hash_hex(&params.query),
    })
    .to_string();
    store.record_usage_event(UsageEvent {
        op: UsageOp::Search,
        tenant: Some(tenant_id.to_string()),
        project: params.project_id.clone(),
        outcome: if result_count > 0 {
            format!("hits:{result_count}")
        } else {
            "zero_hits".to_string()
        },
        chunk_count: Some(result_count as i64),
        bytes: None,
        detail: Some(detail),
    });
}

fn add_usage_outcome(admission: &ResolvedAdmission) -> &'static str {
    admission.usage_outcome()
}

fn record_add_usage_event<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<String>,
    outcome: String,
    chunk_count: i64,
    bytes: usize,
) {
    store.record_usage_event(UsageEvent {
        op: UsageOp::Add,
        tenant: Some(tenant_id.to_string()),
        project: project_id,
        outcome,
        chunk_count: Some(chunk_count),
        bytes: Some(bytes as i64),
        detail: None,
    });
}

// ---------- Handler Functions ----------

/// Prefix each ranked chunk's text with its observed (event) date for recall,
/// so a consuming answer model sees when the event happened. No-op for chunks
/// without a `timestamp_observed`. Opt-in via `SearchParams::render_event_time`.
fn render_observed_time_into_text(chunks: &mut [(MemoryChunk, f32)]) {
    for (chunk, _score) in chunks.iter_mut() {
        if let Some(ms) = chunk.timestamp_observed {
            chunk.text = format!("[{}] {}", format_epoch_ms_date(ms), chunk.text);
        }
    }
}

/// Format Unix milliseconds as a `YYYY-MM-DD` UTC date without a date crate,
/// via the civil-from-days algorithm (Howard Hinnant, public domain).
fn format_epoch_ms_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{:04}-{:02}-{:02}", year, month, day)
}

const RETRIEVAL_EPISODE_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone)]
struct OutcomeRankedPool {
    base: Vec<(MemoryChunk, f32)>,
    served: Vec<(MemoryChunk, f32)>,
    adjustments: HashMap<ChunkId, f32>,
    shadow_ranks: HashMap<ChunkId, usize>,
    shadow_order_changed: bool,
}

fn query_mode_name(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Generic => "generic",
        QueryMode::BriefProject => "brief_project",
        QueryMode::ResumeTask => "resume_task",
        QueryMode::FindFailures => "find_failures",
        QueryMode::FindDecisions => "find_decisions",
        QueryMode::FindEvidence => "find_evidence",
        QueryMode::FindHighlights => "find_highlights",
    }
}

fn source_dedup_group(chunk: &MemoryChunk) -> Option<String> {
    chunk
        .source
        .uri
        .as_deref()
        .filter(|uri| !uri.is_empty())
        .map(crate::store::stable_query_hash)
}

async fn outcome_rank_candidate_pool<S: Store>(
    store: &S,
    scope_tenant_id: &TenantId,
    scope_project_id: Option<&str>,
    candidates: Vec<(MemoryChunk, f32)>,
    policy_mode: RankingPolicyMode,
) -> Result<OutcomeRankedPool, McpError> {
    if policy_mode == RankingPolicyMode::Serve {
        return Err(McpError::InvalidParams(
            "ranking_policy=serve is not activated; pass shadow until the frozen longitudinal gate and rollback check pass"
                .to_string(),
        ));
    }
    if policy_mode == RankingPolicyMode::Off {
        return Ok(OutcomeRankedPool {
            served: candidates.clone(),
            base: candidates,
            adjustments: HashMap::new(),
            shadow_ranks: HashMap::new(),
            shadow_order_changed: false,
        });
    }
    let mut adjustments = HashMap::<ChunkId, f32>::new();
    let chunk_ids = candidates
        .iter()
        .map(|(chunk, _)| chunk.chunk_id.clone())
        .collect::<Vec<_>>();
    let now_ms = current_time_ms();
    for prior in store
        .outcome_priors(scope_tenant_id, scope_project_id, &chunk_ids, now_ms)
        .await
        .map_err(|error| McpError::ToolError(error.to_string()))?
    {
        adjustments.insert(prior.chunk_id.clone(), prior.bounded_adjustment());
    }

    let mut shadow = candidates.clone();
    shadow.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        let left_adjustment = adjustments
            .get(&left_chunk.chunk_id)
            .copied()
            .unwrap_or(0.0);
        let right_adjustment = adjustments
            .get(&right_chunk.chunk_id)
            .copied()
            .unwrap_or(0.0);
        (right_score + right_adjustment)
            .partial_cmp(&(left_score + left_adjustment))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_chunk
                    .timestamp_created
                    .cmp(&left_chunk.timestamp_created)
            })
            .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
    });
    let shadow_ranks = shadow
        .iter()
        .enumerate()
        .map(|(rank, (chunk, _))| (chunk.chunk_id.clone(), rank))
        .collect::<HashMap<_, _>>();
    let shadow_order_changed = candidates
        .iter()
        .map(|(chunk, _)| &chunk.chunk_id)
        .ne(shadow.iter().map(|(chunk, _)| &chunk.chunk_id));

    Ok(OutcomeRankedPool {
        served: candidates.clone(),
        base: candidates,
        adjustments,
        shadow_ranks,
        shadow_order_changed,
    })
}

#[allow(clippy::too_many_arguments)]
async fn record_search_retrieval_episode<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    query: &str,
    mode: QueryMode,
    requested_k: usize,
    policy_mode: RankingPolicyMode,
    task_id: Option<String>,
    thread_id: Option<String>,
    candidate_pool: &OutcomeRankedPool,
    rendered_results: &[ChunkResult],
) -> Result<(String, RankingPolicyInfo), McpError> {
    let episode_id = RetrievalEpisodeId::new();
    let created_at_ms = current_time_ms();
    let served_ranks = rendered_results
        .iter()
        .enumerate()
        .map(|(rank, result)| (result.chunk_id.clone(), rank))
        .collect::<HashMap<_, _>>();
    let items = candidate_pool
        .base
        .iter()
        .enumerate()
        .map(|(original_rank, (chunk, score))| {
            let served_rank = served_ranks.get(&chunk.chunk_id.to_string()).copied();
            RetrievalEpisodeItem {
                episode_id: episode_id.clone(),
                chunk_id: chunk.chunk_id.clone(),
                origin_tenant_id: chunk.tenant_id.clone(),
                origin_project_id: chunk.project_id.as_option().map(str::to_string),
                original_rank,
                original_score: *score,
                lane_scores_json: json!({
                    "base": score,
                    "outcome": candidate_pool.adjustments.get(&chunk.chunk_id).copied().unwrap_or(0.0),
                })
                .to_string(),
                outcome_adjustment: candidate_pool
                    .adjustments
                    .get(&chunk.chunk_id)
                    .copied()
                    .unwrap_or(0.0),
                served_rank,
                shadow_rank: candidate_pool.shadow_ranks.get(&chunk.chunk_id).copied(),
                rendered: served_rank.is_some(),
                source_dedup_group: source_dedup_group(chunk),
            }
        })
        .collect::<Vec<_>>();
    let episode = RetrievalEpisode {
        episode_id: episode_id.clone(),
        tenant_id: tenant_id.clone(),
        project_id: project_id.map(str::to_string),
        query_hash: crate::store::stable_query_hash(query),
        query_mode: query_mode_name(mode).to_string(),
        requested_k,
        fetched_k: items.len(),
        rendered_k: rendered_results.len(),
        policy_version: OUTCOME_POLICY_VERSION.to_string(),
        policy_mode,
        task_id,
        thread_id,
        created_at_ms,
        expires_at_ms: created_at_ms.saturating_add(RETRIEVAL_EPISODE_TTL_MS),
    };
    store
        .record_retrieval_episode(episode, items)
        .await
        .map_err(|error| McpError::ToolError(error.to_string()))?;
    Ok((
        episode_id.to_string(),
        RankingPolicyInfo {
            version: OUTCOME_POLICY_VERSION.to_string(),
            mode: policy_mode,
            candidate_count: candidate_pool.base.len(),
            shadow_order_changed: candidate_pool.shadow_order_changed,
        },
    ))
}

#[cfg(test)]
mod tests;
