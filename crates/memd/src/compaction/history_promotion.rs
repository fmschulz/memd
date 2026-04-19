//! History promotion — moves stale Superseded/Expired rows onto the
//! `MemoryTier::History` tier when their overlay has not changed for
//! at least `age_threshold_ms`.
//!
//! Why the overlay clock and not `timestamp_created`:
//! - `timestamp_created` is the write-time clock and never moves after
//!   insert. If a chunk was stored a year ago but was superseded
//!   today, it is still an active supersession edge that callers may
//!   want to trace; demoting it to History immediately would erase
//!   visible lineage.
//! - `lifecycle_updated_at_ms` moves every time the overlay itself
//!   changes (status flip, tier flip, expiry update, etc.). Using it
//!   as the clock means "demote rows whose lifecycle has settled for
//!   at least N days" — which is exactly the history-tier intent.
//!
//! Composition:
//! - Candidate gather via `MetadataStore::list_stale_superseded`
//!   (already filters `status IN ('superseded','expired') AND
//!   tier != 'history' AND lifecycle_updated_at_ms < cutoff`).
//! - Per-row `update_lifecycle` sets `tier=History` and bumps
//!   `lifecycle_updated_at_ms` so a second run does not re-promote.
//! - Cache bump follows the same "only when hybrid is Some" pattern
//!   as `ExpirySweep`.
//!
//! Race considerations:
//! - Unlike expiry, promotion here is idempotent on the target row
//!   (another writer changing the row back to `tier != History`
//!   would be a manual action we explicitly don't defend against).
//!   The list_stale_superseded pre-filter already excludes rows with
//!   `tier = 'history'`, so double-promotion is prevented. A
//!   concurrent delete or status change would just drop the row from
//!   the next sweep's candidate list without corrupting state.

use crate::error::Result;
use crate::store::hybrid::HybridSearcher;
use crate::store::metadata::MetadataStore;
use crate::types::{LifecycleDelta, MemoryTier, TenantId};

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Outcome of one `HistoryPromotion::run` call.
#[derive(Debug, Clone, Default)]
pub struct PromotionResult {
    /// Number of rows whose overlay tier advanced to `History` during
    /// this run.
    pub promoted_count: usize,
}

/// History promotion job. Stateless aside from `age_threshold_ms`,
/// which callers tune per compaction policy.
#[derive(Debug, Clone)]
pub struct HistoryPromotion {
    /// Minimum overlay-idle window before a stale supersession edge or
    /// expired row is demoted to `History`. Expressed in milliseconds.
    pub age_threshold_ms: i64,
}

impl HistoryPromotion {
    /// Create a new promotion job with the given age threshold.
    pub fn new(age_threshold_ms: i64) -> Self {
        Self { age_threshold_ms }
    }

    /// Run the promotion against `tenant_id`, demoting every row whose
    /// overlay has been in `Superseded`/`Expired` for at least
    /// `age_threshold_ms` milliseconds.
    pub fn run(
        &self,
        metadata: &impl MetadataStore,
        hybrid: Option<&HybridSearcher>,
        tenant_id: &TenantId,
    ) -> Result<PromotionResult> {
        let now = current_time_ms();
        let cutoff = now.saturating_sub(self.age_threshold_ms);
        let ids = metadata.list_stale_superseded(tenant_id, cutoff)?;
        if ids.is_empty() {
            return Ok(PromotionResult { promoted_count: 0 });
        }

        let count = ids.len();
        for id in &ids {
            metadata.update_lifecycle(
                tenant_id,
                id,
                &LifecycleDelta {
                    tier: Some(MemoryTier::History),
                    lifecycle_updated_at_ms: Some(now),
                    ..Default::default()
                },
            )?;
        }

        if let Some(h) = hybrid {
            h.bump_tenant_memory_version(tenant_id);
        }

        Ok(PromotionResult {
            promoted_count: count,
        })
    }
}

#[cfg(test)]
mod tests {
    // End-to-end coverage for the promotion + overlay-clock contract
    // lives in crates/memd/tests/expiry_and_history.rs:
    //   - history_promotion_uses_lifecycle_updated_clock_not_created
    //   - history_promotion_is_idempotent_across_consecutive_runs
    //
    // The cache-bump path is tested by the CompactionRunner integration
    // once Task C5 lands.
    #[test]
    fn promotion_is_default_constructable_with_threshold() {
        let p = super::HistoryPromotion::new(30 * 86_400_000);
        assert_eq!(p.age_threshold_ms, 30 * 86_400_000);
    }
}
