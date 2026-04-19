//! Expiry sweep — materialises `status=Expired` on rows whose
//! `expires_at_ms` has elapsed.
//!
//! Track C2 (lazy retrieval hiding) hides rows with past
//! `expires_at_ms` at the retrieval boundary via
//! `VisibilityPolicy::is_visible_at`. C3 closes the loop by promoting
//! the lifecycle overlay so the row's authoritative status matches the
//! visibility story, which is what metrics, exports, and later
//! `HistoryPromotion` (C4) depend on.
//!
//! Design:
//! - Read-only ID gather via `MetadataStore::list_expired_before` (the
//!   SQL helper already skips rows already in terminal states, so
//!   consecutive sweeps are idempotent).
//! - Per-row `MetadataStore::update_lifecycle` with
//!   `status = Expired` and a stamped `lifecycle_updated_at_ms`. That
//!   bump is load-bearing for C4 — history promotion uses the overlay
//!   clock, not `timestamp_created`.
//! - Cache invalidation via `HybridSearcher::bump_tenant_memory_version`
//!   is guarded: when called standalone (hybrid=Some) the sweep bumps
//!   once after all rows are updated; when called from
//!   `CompactionRunner` (Task C5) callers pass `hybrid=None` and do a
//!   single centralised bump for the whole cycle. This keeps cache
//!   bumps at exactly one per tenant per sweep regardless of call site.

use crate::error::Result;
use crate::store::hybrid::HybridSearcher;
use crate::store::metadata::MetadataStore;
use crate::types::{ChunkStatus, LifecycleDelta, TenantId};

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Outcome of one `ExpirySweep::run` call.
#[derive(Debug, Clone, Default)]
pub struct SweepResult {
    /// Number of rows whose overlay flipped from `Final` (or any other
    /// non-terminal status) to `Expired` during this sweep.
    pub expired_count: usize,
}

/// Stateless expiry sweep. Holds no per-tenant configuration — callers
/// invoke `run` directly whenever they want the overlay to reflect
/// wall-clock expiry. `CompactionRunner` will fold this in as part of
/// task C5.
#[derive(Debug, Clone, Default)]
pub struct ExpirySweep;

impl ExpirySweep {
    /// Create a new sweep. Trivial constructor kept for call-site parity
    /// with `HistoryPromotion::new(age_threshold_ms)` and to give future
    /// configuration (batch size, clamp, etc.) a natural home.
    pub fn new() -> Self {
        Self
    }

    /// Run the sweep against `tenant_id`, promoting every row whose
    /// `expires_at_ms <= now_ms` to `status=Expired`.
    ///
    /// - `metadata`: the SQLite metadata store (trait so this module
    ///   stays decoupled from the concrete `SqliteMetadataStore`).
    /// - `hybrid`: when `Some` and at least one row was promoted, bump
    ///   the tenant memory version so any warm-tier snapshot invalidates
    ///   now rather than at next access.
    /// - `tenant_id`: the tenant to sweep. Callers fan out per tenant.
    ///
    /// Returns `SweepResult { expired_count }`.
    pub fn run(
        &self,
        metadata: &impl MetadataStore,
        hybrid: Option<&HybridSearcher>,
        tenant_id: &TenantId,
    ) -> Result<SweepResult> {
        let now = current_time_ms();
        let ids = metadata.list_expired_before(tenant_id, now)?;
        let count = ids.len();
        if count == 0 {
            return Ok(SweepResult { expired_count: 0 });
        }

        // Per-row update so a single bad row does not cancel the batch;
        // the next sweep will re-surface whatever stayed at Final.
        for id in &ids {
            metadata.update_lifecycle(
                tenant_id,
                id,
                &LifecycleDelta {
                    status: Some(ChunkStatus::Expired),
                    lifecycle_updated_at_ms: Some(now),
                    ..Default::default()
                },
            )?;
        }

        if let Some(h) = hybrid {
            h.bump_tenant_memory_version(tenant_id);
        }

        Ok(SweepResult {
            expired_count: count,
        })
    }
}
