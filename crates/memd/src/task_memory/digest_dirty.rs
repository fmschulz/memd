//! Writer-side digest dirty tracking (Phase 3.4).
//!
//! Before Phase 3.4 every reader path that needed a digest
//! (`artifact.find_failures`, `context.brief_project`, …) regenerated
//! the digest inline via `ensure_*_library_digest`. That shifted the
//! latency of rebuilding project-wide library views onto the hot read
//! path and meant a stale but correct persisted digest never got a
//! chance to be served cold.
//!
//! This module introduces a small per-(tenant, project, role) dirty
//! tracker that writer paths update as they persist artifacts. A
//! sweeper consumes the dirty queue and regenerates only the digests
//! that have actually changed since the last sweep. Readers can keep
//! calling `ensure_*_library_digest` defensively — when the persisted
//! digest already exists and is up-to-date, the writer-driven path
//! will have refreshed it first.
//!
//! The tracker is a process-wide singleton because, in the current
//! architecture, a single `PersistentStore` is shared across all
//! clients of an MCP daemon. A future phase that introduces per-daemon
//! tenant scoping can replace this with an instance on `TenantStore`
//! without changing the call surface.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// One dirty key — something a writer touched that invalidates a
/// digest. The (tenant, project_id, role) triple is enough to scope
/// regeneration. `role` is a `DIGEST_ROLE_*` constant string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DigestDirtyKey {
    pub tenant_id: String,
    pub project_id: Option<String>,
    pub role: String,
}

/// Process-wide dirty tracker. Kept inside the module so callers go
/// through the `mark_dirty` / `drain_dirty` API and cannot forget to
/// deduplicate.
#[derive(Default)]
pub struct DigestDirtyTracker {
    dirty: Mutex<HashSet<DigestDirtyKey>>,
}

impl DigestDirtyTracker {
    fn new() -> Self {
        Self::default()
    }

    /// Mark a (tenant, project, role) scope as needing a digest
    /// regeneration. Safe to call repeatedly for the same key — the
    /// set deduplicates. Cheap on the hot path (a single lock + hash
    /// insert).
    pub fn mark_dirty(&self, key: DigestDirtyKey) {
        self.dirty.lock().insert(key);
    }

    /// Atomically take the current dirty set, leaving an empty one in
    /// its place. The sweeper iterates the returned vector and calls
    /// the relevant `ensure_*_library_digest` helper for each entry.
    /// If regeneration fails the caller can `mark_dirty` again to
    /// retry on the next sweep.
    pub fn drain_dirty(&self) -> Vec<DigestDirtyKey> {
        let mut guard = self.dirty.lock();
        guard.drain().collect()
    }

    /// Check without consuming. Primarily for tests.
    pub fn len(&self) -> usize {
        self.dirty.lock().len()
    }

    /// True when no dirty keys are pending. Primarily for tests.
    pub fn is_empty(&self) -> bool {
        self.dirty.lock().is_empty()
    }

    /// Peek at a specific key's dirty state without consuming.
    /// Primarily for tests and diagnostics.
    pub fn contains(&self, key: &DigestDirtyKey) -> bool {
        self.dirty.lock().contains(key)
    }
}

static GLOBAL_DIGEST_DIRTY_TRACKER: OnceLock<DigestDirtyTracker> = OnceLock::new();

/// Access the process-wide tracker. Auto-initializes on first use.
pub fn global() -> &'static DigestDirtyTracker {
    GLOBAL_DIGEST_DIRTY_TRACKER.get_or_init(DigestDirtyTracker::new)
}

/// Convenience: mark a single (tenant, project, role) scope dirty on
/// the global tracker.
pub fn mark_dirty(tenant_id: impl Into<String>, project_id: Option<String>, role: &str) {
    global().mark_dirty(DigestDirtyKey {
        tenant_id: tenant_id.into(),
        project_id,
        role: role.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_dirty_deduplicates_by_triple() {
        let tracker = DigestDirtyTracker::new();
        let k = DigestDirtyKey {
            tenant_id: "t".to_string(),
            project_id: Some("p".to_string()),
            role: "evidence_library".to_string(),
        };

        tracker.mark_dirty(k.clone());
        tracker.mark_dirty(k.clone());
        tracker.mark_dirty(k.clone());
        assert_eq!(tracker.len(), 1);

        // Different role → separate entry.
        let k2 = DigestDirtyKey {
            tenant_id: "t".to_string(),
            project_id: Some("p".to_string()),
            role: "failure_library".to_string(),
        };
        tracker.mark_dirty(k2);
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn drain_dirty_returns_keys_and_leaves_tracker_empty() {
        let tracker = DigestDirtyTracker::new();
        tracker.mark_dirty(DigestDirtyKey {
            tenant_id: "t1".to_string(),
            project_id: None,
            role: "project_brief".to_string(),
        });
        tracker.mark_dirty(DigestDirtyKey {
            tenant_id: "t2".to_string(),
            project_id: Some("p".to_string()),
            role: "decision_library".to_string(),
        });

        let drained = tracker.drain_dirty();
        assert_eq!(drained.len(), 2);
        assert!(tracker.is_empty());
    }

    #[test]
    fn module_global_is_a_real_singleton() {
        // Hitting the singleton twice returns the same tracker
        // instance — the dirty set is shared across callers.
        let k = DigestDirtyKey {
            tenant_id: "singleton".to_string(),
            project_id: None,
            role: "highlight_library".to_string(),
        };
        global().mark_dirty(k.clone());
        assert!(global().contains(&k));
        // Drain so sibling tests do not observe this entry.
        let _ = global().drain_dirty();
    }
}
