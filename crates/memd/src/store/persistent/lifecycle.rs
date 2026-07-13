use super::*;

impl PersistentStore {
    /// Apply a lifecycle delta through the metadata overlay and bump the tenant
    /// cache version so any tiered searcher invalidates entries that predate
    /// the lifecycle change.
    ///
    /// Intentionally `async` even though the current body has no `.await`:
    /// callers introduced by A6+ (e.g. `supersede_chunk`) and C6 live in
    /// async contexts, so keeping the signature async now avoids a
    /// breaking-change churn later.
    #[allow(clippy::unused_async)]
    pub async fn update_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<()> {
        self.ensure_writable("update_lifecycle")?;
        self.metadata.update_lifecycle(tenant_id, chunk_id, delta)?;
        if let Some(h) = self.hybrid() {
            h.bump_tenant_memory_version(tenant_id);
        }
        Ok(())
    }

    /// Apply a lifecycle delta and report whether the row existed.
    ///
    /// Returns `Ok(true)` when exactly one row was updated, `Ok(false)`
    /// when the UPDATE matched zero rows (non-existent chunk_id OR
    /// cross-tenant access). The cache-version bump only fires on a
    /// successful update — a failed match is a no-op end-to-end.
    ///
    /// Used by `memory.set_expiry` (Track C6) to make the tool's
    /// `{"updated": true}` payload a load-bearing claim rather than a
    /// silent success.
    #[allow(clippy::unused_async)]
    pub async fn update_lifecycle_if_exists(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<bool> {
        self.ensure_writable("update_lifecycle_if_exists")?;
        let rows = self
            .metadata
            .update_lifecycle_counted(tenant_id, chunk_id, delta)?;
        if rows > 0 {
            if let Some(h) = self.hybrid() {
                h.bump_tenant_memory_version(tenant_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Write a chunk plus its initial lifecycle overlay in one logical step.
    ///
    /// Flow:
    /// 1. Write the payload through the normal `Store::add` path
    ///    (WAL + segment + metadata + async index), which already bumps the
    ///    tenant memory version when hybrid/tiered is enabled.
    /// 2. Persist the canonical text used by writer-driven digest /
    ///    supersession-by-content-identity flows (Track D).
    /// 3. If the initial delta has any non-default field, apply it through
    ///    the overlay UPDATE and bump the tenant cache version a second
    ///    time so consumers observing a snapshot between (1) and (3)
    ///    invalidate it.
    ///
    /// Structural indexing is intentionally NOT performed here — it lives
    /// at the MCP/server layer via post-write hooks so the store stays
    /// agnostic about language-aware extraction.
    pub async fn add_chunk_with_lifecycle(
        &self,
        chunk: MemoryChunk,
        initial: LifecycleDelta,
    ) -> Result<ChunkId> {
        self.ensure_writable("add_chunk_with_lifecycle")?;
        let tenant_id = chunk.tenant_id.clone();

        // Step 1: write payload via existing add path (WAL + segment +
        // SQLite + async index). `add_chunks_internal` already populates
        // `canonical_text` on every inserted row from each row's own
        // `chunk.text` (D2). The previous design did a follow-up
        // `set_canonical_text` here using the WHOLE original document's
        // canonical form, which silently overwrote the primary row's
        // per-row canonical_text whenever `split_for_add` produced
        // multiple rows — Codex round-1 D2 review HIGH finding. The
        // INSERT-side write is now the single source of truth.
        let chunk_id = <Self as Store>::add(self, chunk).await?;

        // Step 2: apply the initial lifecycle delta only if non-empty so
        // we skip a no-op UPDATE on the common "no overlay yet" call.
        if !initial.is_empty() {
            let now = current_time_ms();
            let mut delta = initial;
            if delta.lifecycle_updated_at_ms.is_none() {
                delta.lifecycle_updated_at_ms = Some(now);
            }
            self.metadata
                .update_lifecycle(&tenant_id, &chunk_id, &delta)?;
            // Bump again to invalidate any snapshot captured between
            // `add()` and this overlay UPDATE.
            if let Some(h) = self.hybrid() {
                h.bump_tenant_memory_version(&tenant_id);
            }
        }

        Ok(chunk_id)
    }

    /// Persist one journaled consolidation candidate with its preallocated
    /// chunk id. Candidates are intentionally not split: each journal entry
    /// must map to exactly one payload and one lineage target.
    pub async fn add_consolidation_candidate(&self, chunk: MemoryChunk) -> Result<ChunkId> {
        self.add_consolidation_candidate_with_hook(chunk, |_| Ok(()))
            .await
    }

    pub(crate) async fn add_consolidation_candidate_with_hook<F>(
        &self,
        chunk: MemoryChunk,
        hook: F,
    ) -> Result<ChunkId>
    where
        F: FnMut(CandidatePersistenceStage) -> Result<()>,
    {
        self.ensure_writable("add_consolidation_candidate")?;
        if chunk.status != ChunkStatus::Candidate {
            return Err(MemdError::ValidationError(
                "consolidation candidates must use status=candidate".to_string(),
            ));
        }
        let expected_id = chunk.chunk_id.clone();
        let ids = self
            .add_chunks_internal_with_ids_and_hook(vec![chunk], true, hook)
            .await?;
        let stored_id = ids.into_iter().next().ok_or_else(|| {
            MemdError::StorageError("no consolidation candidate id produced".into())
        })?;
        if stored_id != expected_id {
            return Err(MemdError::StorageError(
                "consolidation candidate id changed during persistence".into(),
            ));
        }
        Ok(stored_id)
    }

    /// Reindex candidates after their metadata transaction promotes them to
    /// Final. This is required after crash recovery because startup HNSW
    /// backfill intentionally excludes Candidate rows before session-start
    /// has reconciled the journal.
    pub(crate) async fn refresh_promoted_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        self.ensure_writable("refresh_promoted_chunks")?;
        if let Some(hybrid) = self.hybrid() {
            hybrid.bump_tenant_memory_version(tenant_id);
        }

        let mut index_rows = Vec::with_capacity(chunk_ids.len());
        for chunk_id in chunk_ids {
            let metadata = self.metadata.get(tenant_id, chunk_id)?.ok_or_else(|| {
                MemdError::StorageError(format!("promoted chunk {chunk_id} is missing"))
            })?;
            if metadata.status != ChunkStatus::Final {
                return Err(MemdError::StorageError(format!(
                    "promoted chunk {chunk_id} has status {}",
                    metadata.status
                )));
            }
            let chunk = self.get_chunk(tenant_id, chunk_id).await?.ok_or_else(|| {
                MemdError::StorageError(format!("promoted chunk {chunk_id} payload is missing"))
            })?;
            index_rows.push((chunk_id.clone(), chunk.text));
        }

        self.metadata
            .mark_index_pending(tenant_id, chunk_ids, current_time_ms())?;
        if self.hybrid_searcher.is_none() && self.dense_searcher.is_none() {
            return Ok(());
        }
        if self.async_indexing_enabled() {
            if let Some(indexer) = self.async_indexer.as_ref() {
                let (ack_tx, ack_rx) = oneshot::channel();
                let job = IndexJob {
                    tenant_id: tenant_id.clone(),
                    chunk_ids: chunk_ids.to_vec(),
                    index_rows,
                    completion: Some(ack_tx),
                };
                if indexer.job_tx.send(job).is_err() {
                    let error = "async indexer queue is closed after consolidation promotion";
                    mark_index_failed_many(self.metadata.as_ref(), tenant_id, chunk_ids, error);
                    return Err(MemdError::StorageError(error.to_string()));
                }
                await_index_ack(tenant_id, ack_rx).await;
                return Ok(());
            }
        }

        let result = if let Some(hybrid) = self.hybrid_searcher.as_ref() {
            hybrid.index_batch(tenant_id, &index_rows).await
        } else if let Some(dense) = self.dense_searcher.as_ref() {
            dense.index_batch(tenant_id, &index_rows).await
        } else {
            Ok(())
        };
        match result {
            Ok(()) => self
                .metadata
                .mark_indexed(tenant_id, chunk_ids, current_time_ms()),
            Err(error) => {
                mark_index_failed_many(
                    self.metadata.as_ref(),
                    tenant_id,
                    chunk_ids,
                    &error.to_string(),
                );
                Err(error)
            }
        }
    }

    /// Refresh only this process's dense graph after a separate recovery
    /// writer committed metadata. This path performs no disk or SQLite write
    /// and is therefore safe for session-start's read-only store handle.
    pub(crate) async fn refresh_promoted_chunks_in_memory(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<()> {
        let Some(dense) = self.dense_searcher.as_ref() else {
            return Ok(());
        };
        if let Some(hybrid) = self.hybrid() {
            hybrid.bump_tenant_memory_version(tenant_id);
        }
        let mut index_rows = Vec::with_capacity(chunk_ids.len());
        for chunk_id in chunk_ids {
            let metadata = self.metadata.get(tenant_id, chunk_id)?.ok_or_else(|| {
                MemdError::StorageError(format!("promoted chunk {chunk_id} is missing"))
            })?;
            if metadata.status != ChunkStatus::Final {
                continue;
            }
            let chunk = self.get_chunk(tenant_id, chunk_id).await?.ok_or_else(|| {
                MemdError::StorageError(format!("promoted chunk {chunk_id} payload is missing"))
            })?;
            index_rows.push((chunk_id.clone(), chunk.text));
        }
        if index_rows.is_empty() {
            return Ok(());
        }
        dense.index_batch(tenant_id, &index_rows).await
    }

    /// Atomically supersede `old_id` with a newly written `new_chunk`.
    ///
    /// Flow:
    /// 0. Reject `tenant_id` / `new_chunk.tenant_id` mismatches — an
    ///    easy-to-make caller mistake that would otherwise persist
    ///    `new_chunk` under a different tenant from the supersession
    ///    edge.
    /// 1. Confirm `old_id` exists in `tenant_id` and is not already
    ///    `Deleted`. Doing this BEFORE writing `new_chunk` is what
    ///    makes the operation safe at the store layer: if we deferred
    ///    the check to `atomic_supersede` (step 4), a missing /
    ///    cross-tenant `old_id` would surface as an error AFTER
    ///    `new_chunk` has already been committed to WAL + segment +
    ///    metadata, leaving an orphan row behind.
    /// 2. Walk the `superseded_by` chain from `old_id` for up to 64
    ///    hops to detect pre-existing cycles before we touch disk.
    /// 3. Write `new_chunk` through `add_chunk_with_lifecycle`, which
    ///    runs the full WAL + segment + metadata + canonical-text path
    ///    and bumps the tenant cache version.
    /// 4. Link old ↔ new in one SQLite transaction via
    ///    `MetadataStore::atomic_supersede`. The pair of UPDATEs is
    ///    all-or-nothing on SQLite's side, so a mid-call crash cannot
    ///    leave a half-linked edge.
    /// 5. Best-effort drop of `old_id` from the BM25 sparse index
    ///    (immediate, when hybrid+sparse is enabled) and bump the
    ///    tenant cache version so tiered/in-memory snapshots taken
    ///    between (3) and (4) invalidate. HNSW exclusion happens at
    ///    next compaction rebuild. Authoritative invisibility of
    ///    superseded rows in retrieval is the visibility filter at
    ///    the handler boundary (Track B), not anything in this layer.
    ///
    /// Structural indexing is intentionally NOT performed here — it
    /// happens at the MCP/server layer via post-write hooks after the
    /// caller's dispatch arm invokes this method.
    pub async fn supersede_chunk(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_chunk: MemoryChunk,
    ) -> Result<ChunkId> {
        self.supersede_chunk_with_lifecycle(tenant_id, old_id, new_chunk, LifecycleDelta::default())
            .await
    }

    /// Supersede while applying prepared retention/lifecycle metadata to the
    /// replacement before the old/new edge is linked. This prevents a
    /// successful replacement from briefly or permanently losing the write
    /// policy when a follow-up lifecycle update fails.
    pub async fn supersede_chunk_with_lifecycle(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_chunk: MemoryChunk,
        lifecycle: LifecycleDelta,
    ) -> Result<ChunkId> {
        self.ensure_writable("supersede_chunk")?;
        // Step 0: refuse tenant mismatch immediately. Without this guard
        // the new chunk would be written under `new_chunk.tenant_id`
        // while `atomic_supersede` looks for `old_id` under
        // `tenant_id` — the second would fail and orphan the first.
        if new_chunk.tenant_id != *tenant_id {
            return Err(MemdError::ValidationError(format!(
                "supersede_chunk: new_chunk.tenant_id {} does not match tenant_id {}",
                new_chunk.tenant_id, tenant_id
            )));
        }

        // Step 1: confirm `old_id` exists in `tenant_id` and is not
        // Deleted before we commit any new state. Linking a Deleted
        // row would produce an unreachable supersession edge. The
        // head check is deferred to step 2a so that a pre-existing
        // cycle in a forged / corrupted chain is reported as a cycle
        // rather than masked as a generic not-current-head error —
        // the cycle is a structural bug in the overlay, the not-head
        // case is a normal caller error, and both have distinct
        // remediation paths.
        let old_meta = match self.metadata.get(tenant_id, old_id)? {
            Some(m) if m.status == ChunkStatus::Deleted => {
                return Err(MemdError::ValidationError(format!(
                    "supersede_chunk: old chunk {old_id} is deleted in tenant {tenant_id}"
                )));
            }
            Some(m) => m,
            None => {
                return Err(MemdError::ValidationError(format!(
                    "supersede_chunk: old chunk {old_id} not found in tenant {tenant_id}"
                )));
            }
        };

        // Step 2: cycle detection — guards against a pre-existing loop
        // in the `superseded_by` chain that would make the new edge
        // nonsensical. Walks the visited-set from `old_id` and fails
        // on any revisit (not only return-to-start), no length bound.
        self.detect_supersession_cycle(tenant_id, old_id)?;

        // Step 2a: require `old_id` to be the current head (no
        // existing successor) — cycle detection already passed, so if
        // `superseded_by.is_some()` we're in the plain double-supersede
        // case, not a cycle. The SQL layer also enforces head-only
        // semantics via a `superseded_by IS NULL` WHERE clause in
        // `atomic_supersede`, but that fires only after the new chunk
        // has been persisted — this preflight keeps the common
        // caller-error path off the orphan path.
        if let Some(existing_head) = old_meta.lifecycle.superseded_by.as_ref() {
            return Err(MemdError::ValidationError(format!(
                "supersede_chunk: old chunk {old_id} is not current head \
                 (already superseded by {existing_head}) in tenant {tenant_id}"
            )));
        }

        // Step 3: write the new chunk through the normal add path. Passing
        // a default lifecycle delta keeps this light — atomic_supersede
        // populates `supersedes` / `superseded_by` in step 4. Steps 0–2
        // have ruled out every preflight reason `atomic_supersede` would
        // reject the link; the remaining failure modes (SQLite I/O,
        // concurrent supersede racing the head check) are caught in
        // step 4 and compensated in step 4a so the orphan new chunk
        // doesn't remain visible.
        let new_id = self.add_chunk_with_lifecycle(new_chunk, lifecycle).await?;

        // Step 4: atomically link old ↔ new in a single SQLite transaction.
        // The UPDATE filters on `superseded_by IS NULL` so a concurrent
        // supersede that raced the preflight will fail here rather than
        // forking the graph.
        let now = current_time_ms();
        if let Err(link_err) = self
            .metadata
            .atomic_supersede(tenant_id, old_id, &new_id, now)
        {
            // Step 4a: compensating DURABLE delete on link failure. A
            // metadata-only mark_deleted is not enough — without a WAL
            // delete record, recover_from_wal would replay the original
            // Add after restart and resurrect the orphan as Final; and
            // the hybrid/sparse/dense/tiered indexes would still carry
            // the chunk until the next compaction. Routing through
            // `Store::delete_chunk` hits WAL + metadata + segment
            // tombstone + hybrid delete + cache invalidation, so after
            // this call the orphan is gone from every surface.
            let delete_res = self.delete_chunk(tenant_id, &new_id).await;
            if let Err(del_err) = delete_res {
                warn!(
                    tenant_id = %tenant_id,
                    new_id = %new_id,
                    link_err = %link_err,
                    del_err = %del_err,
                    "supersede_chunk: atomic_supersede failed AND compensating delete_chunk failed; \
                     new chunk is an orphan — investigate manually"
                );
            } else {
                info!(
                    tenant_id = %tenant_id,
                    new_id = %new_id,
                    link_err = %link_err,
                    "supersede_chunk: atomic_supersede failed; orphan new chunk deleted via full delete path"
                );
            }
            return Err(link_err);
        }

        // Step 5: drop old from the sparse index (immediate when
        // hybrid+sparse enabled) and bump the tenant memory version
        // for tiered/in-memory caches. HNSW exclusion lands at next
        // compaction rebuild; the handler-boundary visibility filter
        // (Track B) is the authoritative invisibility guarantee.
        if let Some(h) = self.hybrid() {
            if let Some(sparse) = h.sparse_index() {
                // Best-effort: a sparse-delete failure does not invalidate
                // the supersession edge — the BM25 entry will linger
                // until the next compaction.
                let _ = sparse.delete(tenant_id, old_id);
            }
            h.bump_tenant_memory_version(tenant_id);
        }

        Ok(new_id)
    }

    /// Walk the `superseded_by` chain starting at `start`. Returns
    /// `Err(ValidationError)` if any cycle is detected — either the
    /// chain returns to `start` or revisits a previously-seen node
    /// mid-walk. Returns `Ok(())` on termination at an empty
    /// `superseded_by`. Arbitrary-length acyclic chains are permitted;
    /// the visited-set check is what makes detection robust regardless
    /// of chain length or where the cycle enters.
    fn detect_supersession_cycle(&self, tenant: &TenantId, start: &ChunkId) -> Result<()> {
        use std::collections::HashSet;
        let mut visited: HashSet<ChunkId> = HashSet::new();
        visited.insert(start.clone());
        let mut current = start.clone();
        loop {
            let meta = self.metadata.get(tenant, &current)?;
            match meta.and_then(|m| m.lifecycle.superseded_by) {
                None => return Ok(()),
                Some(next) => {
                    if !visited.insert(next.clone()) {
                        return Err(MemdError::ValidationError(format!(
                            "supersession cycle detected: revisited {next} while walking from {start}"
                        )));
                    }
                    current = next;
                }
            }
        }
    }
}
