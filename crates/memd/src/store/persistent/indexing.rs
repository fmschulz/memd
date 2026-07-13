use super::*;

pub(super) async fn run_async_index_job(
    metadata: &SqliteMetadataStore,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    dense_searcher: Option<&Arc<DenseSearcher>>,
    batch_size: usize,
    job: IndexJob,
    write_epoch: &AtomicU64,
) {
    let mut index_error: Option<String> = None;
    for rows in job.index_rows.chunks(batch_size.max(1)) {
        let result = if let Some(hybrid) = hybrid_searcher {
            hybrid.index_batch(&job.tenant_id, rows).await
        } else if let Some(searcher) = dense_searcher {
            searcher.index_batch(&job.tenant_id, rows).await
        } else {
            Ok(())
        };

        if let Err(e) = result {
            index_error = Some(e.to_string());
            break;
        }
    }

    if let Some(error_message) = index_error {
        warn!(
            tenant_id = %job.tenant_id,
            error = %error_message,
            "async index job failed"
        );
        // Async index-state writes are store-owned metadata commits but
        // do not enter through `ensure_writable`; attribute them before
        // the commit so the external-mutation probe does not warn on
        // queued async indexing.
        write_epoch.fetch_add(1, Ordering::Release);
        mark_index_failed_many(metadata, &job.tenant_id, &job.chunk_ids, &error_message);
        if let Some(tx) = job.completion {
            let _ = tx.send(Err(error_message));
        }
        return;
    }

    // See the failure branch above for why this bypass path bumps the epoch.
    write_epoch.fetch_add(1, Ordering::Release);
    if let Err(e) = metadata.mark_indexed(&job.tenant_id, &job.chunk_ids, current_time_ms()) {
        warn!(
            tenant_id = %job.tenant_id,
            error = %e,
            "failed to mark chunks indexed"
        );
    }
    if let Some(tx) = job.completion {
        let _ = tx.send(Ok(()));
    }
}

/// Wait for an enqueued index job's completion signal, mirroring the sync
/// indexing arm's semantics: an index failure is a warning (the chunk is
/// durable in WAL + metadata and marked `index_failed` for recovery), never
/// an error on the add itself. A dropped channel means the indexer shut
/// down mid-job; the sweeper or startup backfill re-covers the rows.
pub(super) async fn await_index_ack(
    tenant_id: &TenantId,
    rx: oneshot::Receiver<std::result::Result<(), String>>,
) {
    match rx.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(
                tenant_id = %tenant_id,
                error = %error,
                "async index job failed; chunks remain durable and marked for re-index"
            );
        }
        Err(_) => {
            warn!(
                tenant_id = %tenant_id,
                "async indexer shut down before acknowledging; sweeper will re-cover"
            );
        }
    }
}

fn load_chunk_text_for_index(
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
    metadata: &SqliteMetadataStore,
    tenant_id: &TenantId,
    chunk_id: &ChunkId,
) -> Result<Option<String>> {
    let meta = metadata.get(tenant_id, chunk_id)?;
    let meta = match meta {
        Some(m)
            if !matches!(
                m.status,
                ChunkStatus::Candidate | ChunkStatus::Deleted | ChunkStatus::Error
            ) =>
        {
            m
        }
        _ => return Ok(None),
    };

    let tenant_str = tenant_id.to_string();
    let tenant = match tenants.read().get(&tenant_str) {
        Some(t) => Arc::clone(t),
        None => return Ok(None),
    };

    if let Some(bytes) =
        tenant.read_payload_fallback(&meta.chunk_id, meta.segment_id, meta.ordinal)?
    {
        let chunk: MemoryChunk = serde_json::from_slice(&bytes)
            .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
        return Ok(Some(chunk.text));
    }
    Ok(None)
}

/// Sparse self-heal is on by default; `MEMD_SPARSE_SELF_HEAL=0` (or
/// false/no/off) disables the startup degradation check.
pub(super) fn sparse_self_heal_enabled() -> bool {
    std::env::var("MEMD_SPARSE_SELF_HEAL")
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
        })
        .unwrap_or(true)
}

pub(super) async fn run_hnsw_backfill(
    dense_searcher: Option<&Arc<DenseSearcher>>,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
) -> Result<BackfillStats> {
    let mut stats = BackfillStats::default();
    let Some(dense) = dense_searcher else {
        // Dense search disabled — nothing to do.
        return Ok(stats);
    };

    let tenant_strs: Vec<String> = tenants.read().keys().cloned().collect();
    for tenant_str in tenant_strs {
        let tenant_id = match TenantId::new(&tenant_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_str,
                    error = %e,
                    "skipping tenant with invalid id during HNSW backfill"
                );
                continue;
            }
        };

        // Snapshot the tenant's active chunk metadata into memory in one
        // shot. Paging with OFFSET would race with concurrent writes that
        // shift rows between pages; a single `list` call binds the result
        // set to the SQLite view at one point in time and avoids that.
        //
        // `MetadataStore::list` already filters out soft-deleted rows
        // (see sqlite.rs: `WHERE status != 'deleted'`), so the snapshot
        // is the authoritative "active" set at this moment. Any new
        // writes arriving after this point will be indexed by their
        // write path; any chunks indexed between snapshot and the
        // per-chunk membership check will be skipped by `contains_chunk`.
        let metas = metadata.list(&tenant_id, usize::MAX, 0)?;
        if metas.is_empty() {
            continue;
        }

        // Load the persisted index (mapping + embedding cache) BEFORE deciding
        // what is missing. The per-tenant index is created lazily on first
        // search/index, so without this a clean persisted tenant looks 100%
        // missing and we needlessly re-embed every chunk. Best-effort: a load
        // failure leaves the index empty, so the cache-aware check below
        // reports all chunks missing and we re-embed (today's safe fallback).
        if let Err(e) = dense.ensure_index_loaded(&tenant_id) {
            warn!(
                tenant_id = %tenant_id,
                error = %e,
                "HNSW backfill: failed to load persisted index; treating all chunks as missing"
            );
        }

        // Cache-aware membership is the authoritative cold signal: a chunk is
        // "missing" only when it has no LIVE cached embedding (no mapping
        // entry, or a mapping entry whose embedding-cache slot is empty after a
        // missing/corrupt embeddings.bin). Mapping-only membership
        // (`contains_chunk`) would treat cache-less chunks as present and skip
        // re-embedding them, leaving the HNSW without usable vectors. Count
        // heuristics also fail when `next_id` grew past the active count due to
        // deletes (dense deletes never decrement the counter).
        let all_ids: Vec<ChunkId> = metas.iter().map(|m| m.chunk_id.clone()).collect();
        let missing_ids: std::collections::HashSet<ChunkId> = dense
            .chunks_missing_embeddings(&tenant_id, &all_ids)
            .into_iter()
            .collect();

        // Sparse self-heal: a crash (for example a warm worker killed
        // mid-repair) can leave the tantivy directory missing while
        // metadata rows stay active; reopening recreates the index EMPTY
        // and hybrid search silently serves dense-only from then on. When
        // a tenant has active chunks but zero sparse docs, re-index the
        // whole tenant through the hybrid path, which rebuilds both index
        // sides. Dense-side cost is bounded by the embedding cache.
        let sparse_cold = hybrid_searcher
            .and_then(|h| h.sparse_index())
            .map(|s| s.doc_count(&tenant_id).unwrap_or(1) == 0)
            .unwrap_or(false);

        let missing: Vec<_> = if sparse_cold {
            info!(
                tenant_id = %tenant_id,
                active_chunks = metas.len(),
                "sparse index empty for tenant with active chunks — rebuilding both index sides"
            );
            metas
        } else {
            metas
                .into_iter()
                .filter(|m| missing_ids.contains(&m.chunk_id))
                .collect()
        };

        if missing.is_empty() {
            continue;
        }

        info!(
            tenant_id = %tenant_id,
            missing_count = missing.len(),
            "HNSW cold for tenant — backfilling"
        );

        let batch_size: usize = 64;
        let mut indexed_this_tenant = 0usize;
        let mut tenant_had_batch_failure = false;
        for chunk_batch in missing.chunks(batch_size) {
            let mut index_rows: Vec<(ChunkId, String)> = Vec::with_capacity(chunk_batch.len());
            for m in chunk_batch {
                match load_chunk_text_for_index(tenants, metadata, &tenant_id, &m.chunk_id) {
                    Ok(Some(text)) => index_rows.push((m.chunk_id.clone(), text)),
                    Ok(None) => stats.chunks_skipped += 1,
                    Err(e) => {
                        warn!(
                            tenant_id = %tenant_id,
                            chunk_id = %m.chunk_id,
                            error = %e,
                            "HNSW backfill: failed to load chunk, skipping"
                        );
                        stats.chunks_skipped += 1;
                    }
                }
            }

            if index_rows.is_empty() {
                continue;
            }

            let batch_len = index_rows.len();
            let result = if let Some(hybrid) = hybrid_searcher {
                hybrid.index_batch(&tenant_id, &index_rows).await
            } else {
                dense.index_batch(&tenant_id, &index_rows).await
            };
            match result {
                Ok(()) => {
                    indexed_this_tenant += batch_len;
                    stats.chunks_indexed += batch_len;
                }
                Err(e) => {
                    // Don't abandon the rest of the tenant; a single bad
                    // batch shouldn't block the other 99%. But do record
                    // the failure so callers know coverage may be
                    // incomplete and a follow-up pass is warranted.
                    warn!(
                        tenant_id = %tenant_id,
                        batch_len = batch_len,
                        error = %e,
                        "HNSW backfill batch failed; continuing with next batch"
                    );
                    stats.chunks_skipped += batch_len;
                    tenant_had_batch_failure = true;
                }
            }
        }

        if indexed_this_tenant > 0 {
            stats.tenants_backfilled += 1;
            info!(
                tenant_id = %tenant_id,
                chunks_indexed = indexed_this_tenant,
                had_batch_failure = tenant_had_batch_failure,
                "HNSW backfill complete for tenant"
            );
            // A sparse rebuild is durable only after a tantivy commit; the
            // normal write path commits lazily (shutdown / maintenance),
            // which is exactly what a crash bypasses. Commit eagerly so a
            // healed index survives another kill.
            if sparse_cold {
                if let Some(sparse) = hybrid_searcher.and_then(|h| h.sparse_index()) {
                    if let Err(e) = sparse.commit() {
                        warn!(
                            tenant_id = %tenant_id,
                            error = %e,
                            "failed to commit rebuilt sparse index"
                        );
                    }
                }
            }
        }
    }

    Ok(stats)
}

/// Backfill `canonical_text` for any chunk row whose value is NULL.
///
/// Iterates each tenant's snapshot of active rows once, filters those
/// missing a canonical, loads the row's payload from the segment, and
/// writes back `canonicalize_for_type(text, chunk_type)` via the
/// existing `set_canonical_text` API. Errors are logged and counted as
/// skipped rather than aborting the pass — a partial backfill is more
/// useful than no backfill, and skipped rows will be revisited next
/// time the pass runs.
pub(super) fn run_canonical_text_backfill(
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
) -> CanonicalBackfillStats {
    let mut stats = CanonicalBackfillStats::default();
    let tenant_strs: Vec<String> = tenants.read().keys().cloned().collect();

    for tenant_str in tenant_strs {
        let tenant_id = match TenantId::new(&tenant_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_str,
                    error = %e,
                    "skipping tenant with invalid id during canonical_text backfill"
                );
                continue;
            }
        };

        // Snapshot once (same pattern as HNSW backfill — paging with
        // OFFSET races with concurrent writes).
        let metas = match metadata.list(&tenant_id, usize::MAX, 0) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_id,
                    error = %e,
                    "canonical_text backfill: list failed, skipping tenant"
                );
                continue;
            }
        };
        let need: Vec<_> = metas
            .into_iter()
            .filter(|m| m.canonical_text.is_none())
            .collect();
        if need.is_empty() {
            continue;
        }

        for meta in &need {
            match load_chunk_text_for_index(tenants, metadata, &tenant_id, &meta.chunk_id) {
                Ok(Some(text)) => {
                    let canonical =
                        crate::store::supersession::canonicalize_for_type(&text, meta.chunk_type);
                    match metadata.set_canonical_text(&tenant_id, &meta.chunk_id, &canonical) {
                        Ok(()) => stats.rows_backfilled += 1,
                        Err(e) => {
                            warn!(
                                tenant_id = %tenant_id,
                                chunk_id = %meta.chunk_id,
                                error = %e,
                                "canonical_text backfill: write failed"
                            );
                            stats.rows_skipped += 1;
                        }
                    }
                }
                Ok(None) => stats.rows_skipped += 1,
                Err(e) => {
                    warn!(
                        tenant_id = %tenant_id,
                        chunk_id = %meta.chunk_id,
                        error = %e,
                        "canonical_text backfill: load text failed"
                    );
                    stats.rows_skipped += 1;
                }
            }
        }
    }

    stats
}

pub(super) async fn sweep_pending_index_jobs(
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    dense_searcher: Option<&Arc<DenseSearcher>>,
    batch_size: usize,
    write_epoch: &AtomicU64,
) {
    let tenant_ids: Vec<String> = tenants.read().keys().cloned().collect();
    for tenant_id_str in tenant_ids {
        let tenant_id = match TenantId::new(&tenant_id_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(tenant_id = %tenant_id_str, error = %e, "invalid tenant id during pending-index sweep");
                continue;
            }
        };

        let pending_ids = match metadata.list_pending_index_chunk_ids(&tenant_id, batch_size) {
            Ok(ids) => ids,
            Err(e) => {
                warn!(tenant_id = %tenant_id, error = %e, "failed to list pending index chunks");
                continue;
            }
        };
        if pending_ids.is_empty() {
            continue;
        }
        // Async sweep writes (`mark_indexed` / `mark_index_failed`) bypass
        // the public `ensure_writable` entry points. Bump the same epoch
        // before any metadata write in this sweep iteration so async mode
        // does not look like an external metadata mutation.
        write_epoch.fetch_add(1, Ordering::Release);

        let mut chunk_ids = Vec::with_capacity(pending_ids.len());
        let mut index_rows = Vec::with_capacity(pending_ids.len());
        for chunk_id in pending_ids {
            match load_chunk_text_for_index(tenants, metadata, &tenant_id, &chunk_id) {
                Ok(Some(text)) => {
                    chunk_ids.push(chunk_id.clone());
                    index_rows.push((chunk_id, text));
                }
                Ok(None) => {
                    mark_index_failed_many(
                        metadata,
                        &tenant_id,
                        std::slice::from_ref(&chunk_id),
                        "pending chunk not found during index sweep",
                    );
                }
                Err(e) => {
                    mark_index_failed_many(
                        metadata,
                        &tenant_id,
                        std::slice::from_ref(&chunk_id),
                        &e.to_string(),
                    );
                }
            }
        }

        if !chunk_ids.is_empty() {
            run_async_index_job(
                metadata,
                hybrid_searcher,
                dense_searcher,
                batch_size,
                IndexJob {
                    tenant_id: tenant_id.clone(),
                    chunk_ids,
                    index_rows,
                    completion: None,
                },
                write_epoch,
            )
            .await;
        }
    }
}
