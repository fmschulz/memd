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
use crate::types::TenantId;

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
        if ids.is_empty() {
            return Ok(SweepResult { expired_count: 0 });
        }

        // Per-row guarded UPDATE: `mark_expired_if_final` only promotes
        // rows whose current status is still `final`, so a concurrent
        // delete / supersession / expiry transition between the SELECT
        // above and this UPDATE is silently tolerated (the row just
        // doesn't count toward `expired_count`). Prevents the sweep
        // from clobbering newer lifecycle state.
        let mut promoted = 0usize;
        for id in &ids {
            if metadata.mark_expired_if_final(tenant_id, id, now)? {
                promoted += 1;
            }
        }

        if promoted > 0 {
            if let Some(h) = hybrid {
                h.bump_tenant_memory_version(tenant_id);
            }
        }

        Ok(SweepResult {
            expired_count: promoted,
        })
    }
}

#[cfg(test)]
mod tests {
    // Unit-level race-guard coverage lives alongside the SQLite
    // MetadataStore override (`mark_expired_if_final`). The end-to-end
    // sweep contract is covered in crates/memd/tests/expiry_and_history.rs:
    //   - expiry_sweep_marks_rows_expired (happy path, status flip)
    //   - expiry_sweep_is_idempotent_across_consecutive_runs
    //   - expiry_sweep_is_safe_against_concurrent_status_change (race guard)
    //
    // The cache-bump path (bump_tenant_memory_version when hybrid is
    // Some) is exercised by the CompactionRunner integration in Task C5
    // rather than in a standalone unit test — spinning up a
    // HybridSearcher in isolation pulls in dense search init which is
    // tempdir-hostile for small-footprint tests.
    #[test]
    fn sweep_struct_is_default_constructable() {
        let _ = super::ExpirySweep::new();
        let _ = super::ExpirySweep;
        let r = super::SweepResult::default();
        assert_eq!(r.expired_count, 0);
    }
}
