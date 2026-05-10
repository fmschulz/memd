//! Phase 4.1: Background digest sweeper.
//!
//! Phase 3.4 introduced a writer-driven dirty tracker
//! (`task_memory::digest_dirty`) that flags which (tenant, project,
//! role) digests need regeneration after an artifact/evidence write.
//! That tracker was only drained by explicit `memory.compact` calls —
//! a usable hook for operators but not the full freshness story.
//!
//! This module runs a background tokio task that drains the tracker
//! on a timer and calls `mcp::handlers::sweep_dirty_digests` to
//! regenerate the flagged digests. Failures are automatically
//! re-marked by the sweeper so transient errors do not silently drop
//! invalidations.
//!
//! The task is created by long-lived local worker paths. One-shot CLI calls
//! keep freshness explicit through write hooks and `memory.compact`.
//!
//! Configuration:
//!   - `MEMD_DIGEST_SWEEP_INTERVAL_SEC` — how often to drain (default 10s).
//!     Set to `0` to disable the background sweeper entirely (the
//!     `memory.compact` hook still works).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::store::Store;

const DEFAULT_SWEEP_INTERVAL_SEC: u64 = 10;

/// Handle returned by `spawn_digest_sweeper`. Owns the background
/// task and a shutdown channel. Dropping the handle aborts the task.
pub struct DigestSweeperHandle {
    shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl DigestSweeperHandle {
    /// Signal the sweeper to stop and wait for the background task to
    /// drain. Safe to call multiple times; subsequent calls are
    /// no-ops.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// True when a background task is actually running. Returns false
    /// if the sweeper was constructed with `interval == 0` (disabled)
    /// or after `shutdown()` has consumed the handle.
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }
}

impl Drop for DigestSweeperHandle {
    fn drop(&mut self) {
        // Best-effort cancellation: if the handle is dropped without
        // calling `shutdown`, abort the background task so it does
        // not outlive the daemon.
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Spawn the background digest sweeper. The sweeper wakes every
/// `interval` and calls [`super::handlers::sweep_dirty_digests`] on
/// the provided store. Returns a handle whose drop aborts the task.
///
/// When `interval` is `Duration::ZERO`, the sweeper is disabled (the
/// caller gets an already-shutdown handle back). Callers can
/// determine disablement from `DEFAULT_SWEEP_INTERVAL_SEC` or the
/// `MEMD_DIGEST_SWEEP_INTERVAL_SEC` env var.
pub fn spawn_digest_sweeper<S>(store: Arc<S>, interval: Duration) -> DigestSweeperHandle
where
    S: Store + Send + Sync + 'static,
{
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    if interval.is_zero() {
        tracing::info!("digest sweeper disabled (interval = 0)");
        // Return a handle whose task is already finished. Make the
        // watch channel the source of truth for "was told to stop".
        let _ = shutdown_tx.send(true);
        return DigestSweeperHandle {
            shutdown_tx,
            task: None,
        };
    }

    tracing::info!(
        interval_ms = interval.as_millis() as u64,
        "spawning background digest sweeper"
    );

    let task = tokio::spawn(async move {
        // Skip the immediate first tick that `tokio::time::interval`
        // fires on creation; we want the first run to happen after
        // `interval` has elapsed.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let rebuilt =
                        crate::mcp::handlers::sweep_dirty_digests(&*store).await;
                    if rebuilt > 0 {
                        tracing::debug!(
                            rebuilt,
                            "background digest sweeper refreshed dirty digests"
                        );
                    }
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        tracing::info!("background digest sweeper stopping");
                        return;
                    }
                }
            }
        }
    });

    DigestSweeperHandle {
        shutdown_tx,
        task: Some(task),
    }
}

/// Resolve the sweep interval from the env variable, falling back to
/// the default. Exposed so workers and tests can agree on the resolution
/// logic.
pub fn resolve_sweep_interval_from_env() -> Duration {
    let secs = std::env::var("MEMD_DIGEST_SWEEP_INTERVAL_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SWEEP_INTERVAL_SEC);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use crate::task_memory::digest_dirty::{global, DigestDirtyKey};
    use std::time::Duration;

    #[tokio::test]
    async fn sweeper_drains_dirty_keys_on_tick() {
        // Drain any pre-existing entries so we can observe the
        // sweeper's effect cleanly.
        let _ = global().drain_dirty();

        let store = Arc::new(MemoryStore::new());
        let handle = spawn_digest_sweeper(store, Duration::from_millis(80));

        // Seed a dirty key. The default tenant here intentionally uses
        // a name that will resolve to a valid TenantId.
        global().mark_dirty(DigestDirtyKey {
            tenant_id: "sweeper_tick".to_string(),
            project_id: Some("proj".to_string()),
            role: crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY.to_string(),
        });

        // Give the sweeper up to ~500ms to wake and drain.
        let drained_within = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let peeked = global().contains(&DigestDirtyKey {
                    tenant_id: "sweeper_tick".to_string(),
                    project_id: Some("proj".to_string()),
                    role: crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY.to_string(),
                });
                if !peeked {
                    return ();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;

        handle.shutdown().await;
        drained_within.expect("sweeper must drain the dirty key within 500ms");
    }

    #[tokio::test]
    async fn interval_zero_disables_sweeper() {
        // Deterministic check independent of the process-global
        // tracker's state: when interval==0 no background task is
        // spawned, so the handle reports `is_running() == false`.
        // Observing tracker state under a global singleton gets
        // flaky when sibling tests drain or populate concurrently,
        // so we avoid that path here.
        let store = Arc::new(MemoryStore::new());
        let handle = spawn_digest_sweeper(store, Duration::ZERO);
        assert!(
            !handle.is_running(),
            "interval=0 must not spawn a background task"
        );
        handle.shutdown().await;
    }
}
