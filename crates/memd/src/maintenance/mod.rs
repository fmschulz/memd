//! Manual retention and compaction maintenance for `memory.dream`.
//!
//! The first implementation is deliberately conservative: it plans and
//! applies reversible lifecycle transitions for duplicate digest projection
//! chunks, then reports physical compaction opportunities. It does not rewrite
//! append-only segment payloads.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{MemdError, Result};
use crate::index::sparse::SparseIndex;
use crate::store::metadata::MetadataStore;
use crate::store::persistent::PersistentStore;
use crate::store::{Store, StoreHealthSnapshot, TenantManager};
use crate::task_memory::TaskArtifact;
use crate::types::{ChunkId, ChunkStatus, LifecycleDelta, MemoryTier, TenantId};

pub const DIGEST_ROLE_DREAM_REPORT: &str = "dream_report";

fn default_true() -> bool {
    true
}

fn default_older_than_days() -> u64 {
    30
}

fn default_history_after_days() -> u64 {
    90
}

fn default_max_actions() -> usize {
    1_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetentionProfile {
    #[default]
    Safe,
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateStrategy {
    None,
    #[default]
    DigestProjections,
    ExactSafe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalDreamParams {
    #[serde(default = "default_true")]
    pub run_store_compaction: bool,
    #[serde(default)]
    pub vacuum_metadata: bool,
    #[serde(default = "default_true")]
    pub prune_sparse_index: bool,
    #[serde(default)]
    pub rewrite_segments: bool,
}

impl Default for PhysicalDreamParams {
    fn default() -> Self {
        Self {
            run_store_compaction: true,
            vacuum_metadata: false,
            prune_sparse_index: true,
            rewrite_segments: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default = "default_true")]
    pub dry_run: bool,
    #[serde(default)]
    pub retention_profile: RetentionProfile,
    #[serde(default = "default_older_than_days")]
    pub older_than_days: u64,
    #[serde(default = "default_history_after_days")]
    pub history_after_days: u64,
    #[serde(default)]
    pub purge_after_days: Option<u64>,
    #[serde(default = "default_max_actions")]
    pub max_actions: usize,
    #[serde(default)]
    pub duplicate_strategy: DuplicateStrategy,
    #[serde(default)]
    pub digest_modes: Option<Vec<String>>,
    #[serde(default)]
    pub physical: PhysicalDreamParams,
    #[serde(default = "default_true")]
    pub require_archive_before_purge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamScope {
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamPolicy {
    pub dry_run: bool,
    pub retention_profile: RetentionProfile,
    pub duplicate_strategy: DuplicateStrategy,
    pub older_than_days: u64,
    pub history_after_days: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after_days: Option<u64>,
    pub max_actions: usize,
    pub require_archive_before_purge: bool,
    pub physical: PhysicalDreamParams,
}

impl DreamPolicy {
    pub fn from_params(params: &DreamParams) -> Self {
        Self {
            dry_run: params.dry_run,
            retention_profile: params.retention_profile,
            duplicate_strategy: params.duplicate_strategy,
            older_than_days: params.older_than_days,
            history_after_days: params.history_after_days,
            purge_after_days: params.purge_after_days,
            max_actions: params.max_actions,
            require_archive_before_purge: params.require_archive_before_purge,
            physical: params.physical.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskUsageSnapshot {
    pub total_bytes: u64,
    pub tenant_bytes: u64,
    pub metadata_bytes: u64,
    pub metadata_wal_bytes: u64,
    pub sparse_index_bytes: u64,
    pub cache_bytes: u64,
    pub segment_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamStateSnapshot {
    pub health: StoreHealthSnapshot,
    pub disk: DiskUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamActionKind {
    RefreshDigest,
    CreateTakeawayDigest,
    RetireDuplicateProjection,
    PromoteToHistory,
    MarkExpired,
    ArchiveToOmf,
    PurgeMetadataRow,
    PruneSparseIndex,
    RebuildHnsw,
    VacuumMetadata,
    RewriteSegmentsUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamAction {
    pub kind: DreamActionKind,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub survivor_chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub estimated_payload_bytes: usize,
    pub reason: String,
}

impl DreamAction {
    pub fn retire_duplicate_projection(
        chunk_id: &ChunkId,
        survivor_chunk_id: &ChunkId,
        artifact_id: Option<String>,
        estimated_payload_bytes: usize,
    ) -> Self {
        Self {
            kind: DreamActionKind::RetireDuplicateProjection,
            status: "planned".to_string(),
            chunk_id: Some(chunk_id.to_string()),
            survivor_chunk_id: Some(survivor_chunk_id.to_string()),
            artifact_id,
            estimated_payload_bytes,
            reason: "duplicate digest projection with a deterministic survivor".to_string(),
        }
    }

    fn applied(mut self) -> Self {
        self.status = "applied".to_string();
        self
    }

    pub fn rewrite_segments_unsupported() -> Self {
        Self {
            kind: DreamActionKind::RewriteSegmentsUnsupported,
            status: "not_supported".to_string(),
            chunk_id: None,
            survivor_chunk_id: None,
            artifact_id: None,
            estimated_payload_bytes: 0,
            reason: "append-only segment rewrite requires the future shadow-copy protocol"
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimedBytes {
    pub metadata_bytes: u64,
    pub sparse_index_bytes: u64,
    pub tenant_bytes: u64,
    pub estimated_hidden_payload_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalCompactionResult {
    pub store_compaction_ran: bool,
    pub sparse_pruned_chunks: usize,
    pub metadata_vacuum_ran: bool,
    pub rewrite_segments_supported: bool,
}

impl Default for PhysicalCompactionResult {
    fn default() -> Self {
        Self {
            store_compaction_ran: false,
            sparse_pruned_chunks: 0,
            metadata_vacuum_ran: false,
            rewrite_segments_supported: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamReport {
    pub status: String,
    pub scope: DreamScope,
    pub policy: DreamPolicy,
    pub before: DreamStateSnapshot,
    pub planned_actions: Vec<DreamAction>,
    pub applied_actions: Vec<DreamAction>,
    pub after: DreamStateSnapshot,
    pub summary_artifacts: Vec<String>,
    pub archive_artifacts: Vec<String>,
    pub physical: PhysicalCompactionResult,
    pub reclaimed: ReclaimedBytes,
    pub warnings: Vec<String>,
    pub verification: Vec<String>,
}

#[derive(Debug, Clone)]
struct ProjectionCandidate {
    chunk_id: ChunkId,
    timestamp_created: i64,
    canonical_text: String,
    linked_digest_artifact_id: Option<String>,
    estimated_payload_bytes: usize,
}

fn is_default_visible(status: ChunkStatus, tier: MemoryTier) -> bool {
    status == ChunkStatus::Final && tier != MemoryTier::History
}

fn is_digest_projection_chunk(chunk: &crate::types::MemoryChunk) -> bool {
    chunk
        .tags
        .iter()
        .any(|tag| tag == "task:kind:digest" || tag == "task:projection:digest")
        || chunk.text.starts_with("Task digest ")
}

pub async fn plan_duplicate_projection_retirements<S: Store>(
    store: &S,
    persistent: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    max_actions: usize,
) -> Result<Vec<DreamAction>> {
    if max_actions == 0 {
        return Ok(Vec::new());
    }

    let rows = persistent
        .metadata()
        .list_recent_for_project(tenant_id, project_id, 50_000)?;
    let candidate_ids = rows
        .iter()
        .filter(|row| row.canonical_text.is_some())
        .map(|row| row.chunk_id.clone())
        .collect::<Vec<_>>();
    let linked_artifacts = persistent
        .metadata()
        .resolve_artifacts_for_chunks(tenant_id, &candidate_ids)?;

    let mut groups: BTreeMap<String, Vec<ProjectionCandidate>> = BTreeMap::new();
    for row in rows {
        if !is_default_visible(row.status, row.lifecycle.tier) {
            continue;
        }
        let Some(canonical_text) = row.canonical_text.clone() else {
            continue;
        };
        let Some(chunk) = store.get(tenant_id, &row.chunk_id).await? else {
            continue;
        };
        if !is_digest_projection_chunk(&chunk) {
            continue;
        }
        let artifact_id = linked_artifacts
            .get(&row.chunk_id.to_string())
            .filter(|artifact| artifact.artifact_kind == crate::task_memory::ArtifactKind::Digest)
            .map(|artifact| artifact.artifact_id.clone());
        groups
            .entry(canonical_text.clone())
            .or_default()
            .push(ProjectionCandidate {
                chunk_id: row.chunk_id,
                timestamp_created: row.timestamp_created,
                canonical_text,
                linked_digest_artifact_id: artifact_id,
                estimated_payload_bytes: chunk.text.len(),
            });
    }

    let mut actions = Vec::new();
    for (_canonical, mut group) in groups {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|left, right| {
            right
                .linked_digest_artifact_id
                .is_some()
                .cmp(&left.linked_digest_artifact_id.is_some())
                .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
                .then_with(|| left.chunk_id.to_string().cmp(&right.chunk_id.to_string()))
        });
        let survivor = group[0].chunk_id.clone();
        for duplicate in group.into_iter().skip(1) {
            actions.push(DreamAction::retire_duplicate_projection(
                &duplicate.chunk_id,
                &survivor,
                duplicate.linked_digest_artifact_id,
                duplicate
                    .estimated_payload_bytes
                    .max(duplicate.canonical_text.len()),
            ));
            if actions.len() >= max_actions {
                return Ok(actions);
            }
        }
    }

    actions.sort_by(|left, right| {
        left.chunk_id
            .as_deref()
            .unwrap_or("")
            .cmp(right.chunk_id.as_deref().unwrap_or(""))
    });
    Ok(actions)
}

pub async fn apply_lifecycle_actions(
    persistent: &PersistentStore,
    tenant_id: &TenantId,
    actions: &[DreamAction],
    now_ms: i64,
) -> Result<Vec<DreamAction>> {
    let mut applied = Vec::new();
    for action in actions {
        if action.kind != DreamActionKind::RetireDuplicateProjection {
            continue;
        }
        let Some(chunk_id_raw) = action.chunk_id.as_deref() else {
            continue;
        };
        let chunk_id = ChunkId::parse(chunk_id_raw)?;
        let current = persistent.metadata().get(tenant_id, &chunk_id)?;
        let Some(current) = current else {
            continue;
        };
        if !is_default_visible(current.status, current.lifecycle.tier) {
            continue;
        }
        let survivor = action
            .survivor_chunk_id
            .as_deref()
            .map(ChunkId::parse)
            .transpose()?;
        persistent
            .update_lifecycle(
                tenant_id,
                &chunk_id,
                &LifecycleDelta {
                    status: Some(ChunkStatus::Superseded),
                    tier: Some(MemoryTier::History),
                    superseded_by: survivor,
                    lifecycle_updated_at_ms: Some(now_ms),
                    ..Default::default()
                },
            )
            .await?;
        applied.push(action.clone().applied());
    }
    Ok(applied)
}

pub fn prune_sparse_index_for_actions(
    persistent: &PersistentStore,
    tenant_id: &TenantId,
    actions: &[DreamAction],
) -> Result<usize> {
    let Some(sparse) = persistent.sparse_index() else {
        return Ok(0);
    };
    let mut pruned = 0usize;
    for action in actions {
        let Some(chunk_id_raw) = action.chunk_id.as_deref() else {
            continue;
        };
        let chunk_id = ChunkId::parse(chunk_id_raw)?;
        if sparse.delete(tenant_id, &chunk_id)? {
            pruned += 1;
        }
    }
    sparse.commit()?;
    Ok(pruned)
}

fn path_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return path.metadata().map(|m| m.len()).map_err(MemdError::IoError);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(MemdError::IoError)? {
        let entry = entry.map_err(MemdError::IoError)?;
        total = total.saturating_add(path_size(&entry.path())?);
    }
    Ok(total)
}

pub fn disk_snapshot(
    tenant_manager: Option<&TenantManager>,
    tenant_id: &TenantId,
) -> DiskUsageSnapshot {
    let Some(manager) = tenant_manager else {
        return DiskUsageSnapshot::default();
    };
    let tenant_stats = manager.tenant_disk_stats(tenant_id).unwrap_or_default();
    let tenant_path = manager.tenant_path(tenant_id);
    let data_dir = tenant_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let metadata_bytes = data_dir
        .as_ref()
        .map(|dir| path_size(&dir.join("metadata.db")).unwrap_or(0))
        .unwrap_or(0);
    let metadata_wal_bytes = data_dir
        .as_ref()
        .map(|dir| path_size(&dir.join("metadata.db-wal")).unwrap_or(0))
        .unwrap_or(0);
    let sparse_index_bytes = data_dir
        .as_ref()
        .map(|dir| path_size(&dir.join("sparse_index")).unwrap_or(0))
        .unwrap_or(0);
    let cache_bytes = path_size(&tenant_path.join("cache")).unwrap_or(0);
    let total_bytes = tenant_stats
        .total_bytes
        .saturating_add(metadata_bytes)
        .saturating_add(metadata_wal_bytes)
        .saturating_add(sparse_index_bytes);

    DiskUsageSnapshot {
        total_bytes,
        tenant_bytes: tenant_stats.total_bytes,
        metadata_bytes,
        metadata_wal_bytes,
        sparse_index_bytes,
        cache_bytes,
        segment_count: tenant_stats.segment_count,
    }
}

pub fn related_artifact_ids_from_actions(actions: &[DreamAction]) -> Vec<String> {
    let mut ids = actions
        .iter()
        .filter_map(|action| action.artifact_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub fn related_artifact_ids_from_project_artifacts(artifacts: &[TaskArtifact]) -> Vec<String> {
    let mut ids = artifacts
        .iter()
        .filter(|artifact| artifact.artifact_kind != crate::task_memory::ArtifactKind::Digest)
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids.truncate(200);
    ids
}

pub fn estimated_hidden_payload_bytes(actions: &[DreamAction]) -> usize {
    actions
        .iter()
        .filter(|action| action.kind == DreamActionKind::RetireDuplicateProjection)
        .map(|action| action.estimated_payload_bytes)
        .sum()
}

pub fn status_for_report(blocked: bool, dry_run: bool, applied_count: usize) -> String {
    if blocked {
        "blocked".to_string()
    } else if dry_run {
        "dry_run".to_string()
    } else if applied_count > 0 {
        "completed".to_string()
    } else {
        "completed".to_string()
    }
}

pub fn build_reclaimed(
    before: &DiskUsageSnapshot,
    after: &DiskUsageSnapshot,
    applied_actions: &[DreamAction],
) -> ReclaimedBytes {
    ReclaimedBytes {
        metadata_bytes: before.metadata_bytes.saturating_sub(after.metadata_bytes),
        sparse_index_bytes: before
            .sparse_index_bytes
            .saturating_sub(after.sparse_index_bytes),
        tenant_bytes: before.tenant_bytes.saturating_sub(after.tenant_bytes),
        estimated_hidden_payload_bytes: estimated_hidden_payload_bytes(applied_actions),
    }
}

pub fn unsupported_exact_safe_warning(strategy: DuplicateStrategy) -> Option<String> {
    (strategy == DuplicateStrategy::ExactSafe).then(|| {
        "exact_safe currently retires digest projections only; non-digest exact duplicates remain report-only".to_string()
    })
}
