//! Track D conflict-aware ingestion helpers.
//!
//! Computes the set of prior chunks the incoming write should
//! atomically supersede. Both `memory.add` (D3) and `memory.add_batch`
//! (D4) call into the same primitive so the surface stays consistent.

use crate::error::{MemdError, Result};
use crate::mcp::error::McpError;
use crate::mcp::handlers::{DedupConfig, DedupSpec};
use crate::store::metadata::{ChunkMetadata, MetadataStore};
use crate::store::persistent::PersistentStore;
use crate::store::supersession::{canonicalize_for_type, is_near_duplicate};
use crate::types::{ChunkId, ChunkStatus, ChunkType, TenantId};

/// Default fuzzy threshold: tuned for the padded-char-trigram Jaccard
/// implementation. ~0.92 is a strict paraphrase tier where the
/// canonical form is *almost* identical (≤ 1 punctuation tweak or one
/// short stop-word inserted/removed).
pub const DEFAULT_FUZZY_THRESHOLD: f32 = 0.92;

/// How many recent rows the fuzzy candidate pool should consider per
/// scope. Trigram Jaccard is O(N) over the pool, so we cap it.
pub const FUZZY_RECENT_POOL_SIZE: usize = 128;

/// Effective dedup configuration after collapsing the bool / config
/// shorthand.
pub struct ResolvedDedup {
    pub mode: DedupMode,
    pub threshold: f32,
    pub scope_project: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupMode {
    Exact,
    Fuzzy,
}

/// Collapse the user-facing `DedupSpec` into a `ResolvedDedup`. Returns
/// `Ok(None)` when the spec is `Some(Bool(false))` so callers can skip
/// the dedup path without an extra branch.
pub fn resolve_spec(spec: &DedupSpec) -> Result<Option<ResolvedDedup>> {
    match spec {
        DedupSpec::Bool(false) => Ok(None),
        DedupSpec::Bool(true) => Ok(Some(ResolvedDedup {
            mode: DedupMode::Exact,
            threshold: DEFAULT_FUZZY_THRESHOLD,
            scope_project: true,
        })),
        DedupSpec::Config(cfg) => resolve_config(cfg).map(Some),
    }
}

fn resolve_config(cfg: &DedupConfig) -> Result<ResolvedDedup> {
    let mode = match cfg.mode.as_deref().unwrap_or("exact") {
        "exact" => DedupMode::Exact,
        "fuzzy" => DedupMode::Fuzzy,
        other => {
            return Err(MemdError::ValidationError(format!(
                "supersede_near_duplicates.mode: expected 'exact' or 'fuzzy', got '{other}'"
            )));
        }
    };
    let threshold = cfg.threshold.unwrap_or(DEFAULT_FUZZY_THRESHOLD);
    let scope_project = match cfg.scope.as_deref().unwrap_or("project") {
        "project" => true,
        "tenant" => false,
        other => {
            return Err(MemdError::ValidationError(format!(
                "supersede_near_duplicates.scope: expected 'project' or 'tenant', got '{other}'"
            )));
        }
    };
    Ok(ResolvedDedup {
        mode,
        threshold,
        scope_project,
    })
}

/// Compute the chunk_ids the incoming `(text, chunk_type, project_id)`
/// should atomically supersede on its way in, given a resolved dedup
/// config. Read-only on the store.
///
/// Two safety filters apply on top of the canonical / fuzzy match:
/// * `scope: "project"` and the incoming chunk has `project_id = None`
///   matches only rows whose `project_id` is also NULL — without this
///   post-filter the SQL helpers treat a `None` project as "any
///   project", which would silently widen "scope: project" to the
///   whole tenant for unscoped writes (Codex round-1 D3 MEDIUM).
/// * Only "live head" rows (status == Final, superseded_by IS NONE)
///   are returned. Without this filter a historical chain (A → B → C)
///   would surface A and B as candidates, which then fail
///   `supersede_chunk`'s head-only guard or — worse — get rewritten by
///   blind `update_lifecycle` calls that overwrite an existing
///   `superseded_by` edge (Codex round-1 D3 HIGH-2).
pub fn compute_dedup_candidates(
    ps: &PersistentStore,
    tenant_id: &TenantId,
    text: &str,
    chunk_type: ChunkType,
    project_id: Option<&str>,
    cfg: &ResolvedDedup,
) -> std::result::Result<Vec<ChunkId>, McpError> {
    let canonical = canonicalize_for_type(text, chunk_type);

    // SQL pre-filter: tenant scope means "no project filter"; project
    // scope means "filter to this project_id" — but the SQL helpers
    // can only filter when project_id is Some. The Rust scope filter
    // below covers the project_id-is-None case.
    let sql_project = if cfg.scope_project { project_id } else { None };

    let candidates = match cfg.mode {
        DedupMode::Exact => ps
            .metadata()
            .list_by_canonical_text(tenant_id, sql_project, &canonical)
            .map_err(|e| McpError::ToolError(e.to_string()))?,
        DedupMode::Fuzzy => fuzzy_candidates(ps, tenant_id, &canonical, cfg, project_id)?,
    };

    Ok(candidates
        .into_iter()
        .filter(|m| project_scope_matches(m, cfg.scope_project, project_id))
        .filter(|m| is_live_head_row(m))
        .map(|m| m.chunk_id)
        .collect())
}

/// Fuzzy-mode candidate pool. Has to short-circuit on the
/// `scope=project + project_id=None` case because
/// `list_recent_for_project(..., None, ...)` widens to "any project"
/// before applying LIMIT — so a valid older NULL-project candidate can
/// be evicted by recent project-scoped traffic. Use the dedicated
/// `list_recent_with_null_project` helper there to keep the pre-LIMIT
/// filter aligned with the requested scope (Codex round-2 D3/D4
/// MEDIUM finding).
fn fuzzy_candidates(
    ps: &PersistentStore,
    tenant_id: &TenantId,
    canonical: &str,
    cfg: &ResolvedDedup,
    project_id: Option<&str>,
) -> std::result::Result<Vec<ChunkMetadata>, McpError> {
    let metas = if cfg.scope_project && project_id.is_none() {
        ps.metadata()
            .list_recent_with_null_project(tenant_id, FUZZY_RECENT_POOL_SIZE)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        let sql_project = if cfg.scope_project { project_id } else { None };
        ps.metadata()
            .list_recent_for_project(tenant_id, sql_project, FUZZY_RECENT_POOL_SIZE)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };
    Ok(metas
        .into_iter()
        .filter(|m| {
            is_near_duplicate(
                canonical,
                m.canonical_text.as_deref().unwrap_or(""),
                cfg.threshold,
            )
        })
        .collect())
}

fn project_scope_matches(
    m: &ChunkMetadata,
    scope_project: bool,
    requested_project: Option<&str>,
) -> bool {
    if !scope_project {
        return true;
    }
    m.project_id.as_deref() == requested_project
}

/// A row is a valid supersession target only when it is the live head
/// of its chain: status == Final AND superseded_by IS NULL. Anything
/// else (Superseded, Expired, Deleted, or already-superseded Final) is
/// either non-mutable or owned by another writer's edge.
fn is_live_head_row(m: &ChunkMetadata) -> bool {
    m.status == ChunkStatus::Final && m.lifecycle.superseded_by.is_none()
}
