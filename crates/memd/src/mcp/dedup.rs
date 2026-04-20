//! Track D conflict-aware ingestion helpers.
//!
//! Computes the set of prior chunks the incoming write should
//! atomically supersede. Both `memory.add` (D3) and `memory.add_batch`
//! (D4) call into the same primitive so the surface stays consistent.

use crate::error::{MemdError, Result};
use crate::mcp::error::McpError;
use crate::mcp::handlers::{DedupConfig, DedupSpec};
use crate::store::metadata::MetadataStore;
use crate::store::persistent::PersistentStore;
use crate::store::supersession::{canonicalize_for_type, is_near_duplicate};
use crate::types::{ChunkId, ChunkType, TenantId};

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
pub fn compute_dedup_candidates(
    ps: &PersistentStore,
    tenant_id: &TenantId,
    text: &str,
    chunk_type: ChunkType,
    project_id: Option<&str>,
    cfg: &ResolvedDedup,
) -> std::result::Result<Vec<ChunkId>, McpError> {
    let canonical = canonicalize_for_type(text, chunk_type);
    let scope = if cfg.scope_project { project_id } else { None };

    match cfg.mode {
        DedupMode::Exact => {
            let metas = ps
                .metadata()
                .list_by_canonical_text(tenant_id, scope, &canonical)
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            Ok(metas.into_iter().map(|m| m.chunk_id).collect())
        }
        DedupMode::Fuzzy => {
            let metas = ps
                .metadata()
                .list_recent_for_project(tenant_id, scope, FUZZY_RECENT_POOL_SIZE)
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            Ok(metas
                .into_iter()
                .filter(|m| {
                    is_near_duplicate(
                        &canonical,
                        m.canonical_text.as_deref().unwrap_or(""),
                        cfg.threshold,
                    )
                })
                .map(|m| m.chunk_id)
                .collect())
        }
    }
}
