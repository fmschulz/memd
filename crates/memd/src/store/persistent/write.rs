use super::*;

impl PersistentStore {
    pub(super) fn expand_chunks_for_add(
        &self,
        chunks: Vec<MemoryChunk>,
    ) -> Result<(Vec<MemoryChunk>, Vec<usize>)> {
        if chunks.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut expanded = Vec::new();
        let mut primary_positions = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let parts = super::super::split_for_add(chunk);
            if parts.is_empty() {
                return Err(MemdError::StorageError(
                    "split_for_add produced no chunks".into(),
                ));
            }
            primary_positions.push(expanded.len());
            expanded.extend(parts);
        }
        Ok((expanded, primary_positions))
    }

    pub(super) fn prepare_pending_chunks(
        &self,
        chunks: Vec<MemoryChunk>,
        preserve_chunk_ids: bool,
    ) -> Result<Vec<PendingChunkAdd>> {
        let mut pending = Vec::with_capacity(chunks.len());
        for mut chunk in chunks {
            let chunk_id = if preserve_chunk_ids {
                if self
                    .metadata
                    .get(&chunk.tenant_id, &chunk.chunk_id)?
                    .is_some()
                {
                    return Err(MemdError::ValidationError(format!(
                        "preallocated chunk id {} already exists",
                        chunk.chunk_id
                    )));
                }
                chunk.chunk_id.clone()
            } else {
                ChunkId::new()
            };
            chunk.chunk_id = chunk_id.clone();
            chunk.hash = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(chunk.text.as_bytes());
                format!("{:x}", hasher.finalize())
            };
            let payload = serde_json::to_vec(&chunk)
                .map_err(|e| MemdError::StorageError(format!("serialize chunk: {}", e)))?;
            pending.push(PendingChunkAdd {
                chunk,
                chunk_id,
                payload,
            });
        }
        Ok(pending)
    }

    pub(super) fn checkpoint_after_batch(
        &self,
        tenant: &TenantStore,
        tenant_id: &str,
        writes: u32,
    ) -> Result<()> {
        let interval = self.config.wal_checkpoint_interval;
        if interval == 0 || writes == 0 {
            return Ok(());
        }

        let checkpoints = {
            let mut count = tenant.writes_since_checkpoint.lock();
            *count += writes;
            let checkpoints = *count / interval;
            *count %= interval;
            checkpoints
        };
        if checkpoints == 0 {
            return Ok(());
        }

        // A checkpoint asserts "every record at or before me is durable in
        // finalized segments": recovery drops those records and then
        // truncates the WAL. Appending one while the active segment is
        // still unfinalized would discard the only durable copy of those
        // adds on the next open (the unfinalized segment has no `meta`
        // file, so `load_segments` skips it). Finalize first so the
        // checkpoint's claim is true before it is written.
        tenant.finalize_active_segment()?;

        let timestamp = current_time_ms();
        for _ in 0..checkpoints {
            tenant.append_wal_checkpoint(tenant_id, timestamp)?;
        }
        Ok(())
    }

    pub(super) async fn add_chunks_internal(
        &self,
        chunks: Vec<MemoryChunk>,
    ) -> Result<Vec<ChunkId>> {
        self.add_chunks_internal_with_ids(chunks, false).await
    }

    pub(super) async fn add_chunks_internal_with_ids(
        &self,
        chunks: Vec<MemoryChunk>,
        preserve_chunk_ids: bool,
    ) -> Result<Vec<ChunkId>> {
        self.add_chunks_internal_with_ids_and_hook(chunks, preserve_chunk_ids, |_| Ok(()))
            .await
    }

    pub(super) async fn add_chunks_internal_with_ids_and_hook<F>(
        &self,
        chunks: Vec<MemoryChunk>,
        preserve_chunk_ids: bool,
        mut hook: F,
    ) -> Result<Vec<ChunkId>>
    where
        F: FnMut(CandidatePersistenceStage) -> Result<()>,
    {
        self.ensure_writable("add_chunks_internal")?;
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let (expanded_chunks, primary_positions) = if preserve_chunk_ids {
            let positions = (0..chunks.len()).collect();
            (chunks, positions)
        } else {
            self.expand_chunks_for_add(chunks)?
        };
        let pending = self.prepare_pending_chunks(expanded_chunks, preserve_chunk_ids)?;

        let mut tenant_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, row) in pending.iter().enumerate() {
            tenant_groups
                .entry(row.chunk.tenant_id.to_string())
                .or_default()
                .push(idx);
        }

        for (tenant_id_str, indices) in tenant_groups {
            let tenant = self.get_or_create_tenant(&tenant_id_str)?;
            let tenant_id = pending[indices[0]].chunk.tenant_id.clone();

            let wal_rows: Vec<(String, i64, Vec<u8>)> = indices
                .iter()
                .map(|&idx| {
                    (
                        pending[idx].chunk_id.to_string(),
                        pending[idx].chunk.timestamp_created,
                        pending[idx].payload.clone(),
                    )
                })
                .collect();
            tenant.append_wal_add_batch(&tenant_id_str, &wal_rows, "add_batch")?;
            hook(CandidatePersistenceStage::WalAppended)?;

            let mut metadata_rows = Vec::with_capacity(indices.len());
            let mut index_rows = Vec::with_capacity(indices.len());
            for idx in &indices {
                let row = &pending[*idx];
                tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
                let (segment_id, ordinal) = {
                    let mut active = tenant.active_segment.lock();
                    let seg = active
                        .as_mut()
                        .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
                    let ordinal = seg.writer.append_chunk(&row.payload)?;
                    seg.chunk_count += 1;
                    (seg.writer.id(), ordinal)
                };

                metadata_rows.push(ChunkMetadata {
                    chunk_id: row.chunk_id.clone(),
                    tenant_id: row.chunk.tenant_id.clone(),
                    project_id: row.chunk.project_id.as_option().map(|s| s.to_string()),
                    segment_id,
                    ordinal,
                    chunk_type: row.chunk.chunk_type,
                    status: row.chunk.status,
                    timestamp_created: row.chunk.timestamp_created,
                    hash: row.chunk.hash.clone(),
                    source_uri: row.chunk.source.uri.clone(),
                    // A8: writer will populate lifecycle overlay directly once update_lifecycle is wired in.
                    lifecycle: crate::types::LifecycleMetadata::default(),
                    // D2: populate canonical_text at INSERT time so the
                    // `idx_chunks_canonical` index covers every memory.add
                    // / memory.add_batch write — not just lifecycle-bearing
                    // ones routed through `add_chunk_with_lifecycle`.
                    canonical_text: Some(crate::store::supersession::canonicalize_for_type(
                        &row.chunk.text,
                        row.chunk.chunk_type,
                    )),
                    // E1: mirror the chunk's ingestion_mode label.
                    ingestion_mode: row.chunk.ingestion_mode,
                });
                index_rows.push((row.chunk_id.clone(), row.chunk.text.clone()));
            }
            // Durability ordering: flush + fsync payload bytes before
            // the metadata commit. See the sibling call in
            // `add_task_artifact` for rationale.
            tenant.flush_active_segment_payload()?;
            self.metadata.insert_many(&metadata_rows)?;
            hook(CandidatePersistenceStage::MetadataInserted)?;
            let chunk_ids_for_state: Vec<ChunkId> = metadata_rows
                .iter()
                .map(|row| row.chunk_id.clone())
                .collect();
            self.metadata.mark_index_pending(
                &tenant_id,
                &chunk_ids_for_state,
                current_time_ms(),
            )?;

            if self.async_indexing_enabled() {
                if let Some(indexer) = self.async_indexer.as_ref() {
                    let (ack_tx, ack_rx) = oneshot::channel();
                    let job = IndexJob {
                        tenant_id: tenant_id.clone(),
                        chunk_ids: chunk_ids_for_state.clone(),
                        index_rows,
                        completion: Some(ack_tx),
                    };
                    if indexer.job_tx.send(job).is_err() {
                        let error_message = "async indexer queue is closed";
                        warn!(tenant_id = %tenant_id, error = error_message, "failed to enqueue async index job");
                        mark_index_failed_many(
                            self.metadata.as_ref(),
                            &tenant_id,
                            &chunk_ids_for_state,
                            error_message,
                        );
                    } else {
                        // Hold the acknowledgement until the chunks are
                        // searchable — "add returned" must keep implying
                        // "search finds it" (the 1.3.0 async default broke
                        // that: bulk loads ack'd early and searches read a
                        // half-built index). The await yields; it does not
                        // park the caller's thread.
                        await_index_ack(&tenant_id, ack_rx).await;
                    }
                } else {
                    let error_message = "async indexing enabled but worker unavailable";
                    warn!(tenant_id = %tenant_id, error = error_message, "cannot enqueue async index job");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        error_message,
                    );
                }
            } else {
                let index_result = if let Some(ref hybrid) = self.hybrid_searcher {
                    hybrid.index_batch(&tenant_id, &index_rows).await
                } else if let Some(ref searcher) = self.dense_searcher {
                    searcher.index_batch(&tenant_id, &index_rows).await
                } else {
                    Ok(())
                };

                match index_result {
                    Ok(()) => {
                        self.metadata.mark_indexed(
                            &tenant_id,
                            &chunk_ids_for_state,
                            current_time_ms(),
                        )?;
                    }
                    Err(e) => {
                        warn!(tenant_id = %tenant_id, error = %e, "sync index batch failed");
                        mark_index_failed_many(
                            self.metadata.as_ref(),
                            &tenant_id,
                            &chunk_ids_for_state,
                            &e.to_string(),
                        );
                    }
                }
            }

            self.checkpoint_after_batch(&tenant, &tenant_id_str, indices.len() as u32)?;
        }

        let expanded_ids: Vec<ChunkId> = pending.iter().map(|row| row.chunk_id.clone()).collect();
        let mut primary_ids = Vec::with_capacity(primary_positions.len());
        for pos in primary_positions {
            let chunk_id = expanded_ids
                .get(pos)
                .ok_or_else(|| MemdError::StorageError("missing primary chunk id".into()))?;
            primary_ids.push(chunk_id.clone());
        }
        Ok(primary_ids)
    }

    pub(super) async fn add_task_artifact_internal(
        &self,
        artifact: TaskArtifact,
        projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        self.ensure_writable("add_task_artifact")?;
        let projection_kinds = projections
            .iter()
            .map(|projection| projection.kind.as_str().to_string())
            .collect::<Vec<_>>();

        if projections.is_empty() {
            return Err(MemdError::StorageError(
                "task artifact requires at least one projection".into(),
            ));
        }

        let tenant_id = artifact.tenant_id.clone();
        let tenant_id_str = tenant_id.to_string();
        let projection_chunks = projections
            .into_iter()
            .map(|projection| projection.chunk)
            .collect::<Vec<_>>();
        if projection_chunks
            .iter()
            .any(|chunk| chunk.tenant_id != tenant_id)
        {
            return Err(MemdError::StorageError(
                "task projections must belong to the same tenant as the artifact".into(),
            ));
        }

        let (expanded_chunks, primary_positions) = self.expand_chunks_for_add(projection_chunks)?;
        let pending = self.prepare_pending_chunks(expanded_chunks, false)?;
        let tenant = self.get_or_create_tenant(&tenant_id_str)?;

        let expanded_ids: Vec<ChunkId> = pending.iter().map(|row| row.chunk_id.clone()).collect();
        let mut projection_chunk_ids = Vec::with_capacity(primary_positions.len());
        for pos in &primary_positions {
            let chunk_id = expanded_ids.get(*pos).ok_or_else(|| {
                MemdError::StorageError("missing primary projection chunk id".into())
            })?;
            projection_chunk_ids.push(chunk_id.to_string());
        }

        let task_wal_payload = serde_json::to_vec(&TaskArtifactWalPayload {
            artifact: artifact.clone(),
            projection_chunk_ids: projection_chunk_ids.clone(),
            projection_kinds: projection_kinds.clone(),
        })
        .map_err(|e| {
            MemdError::StorageError(format!("serialize task artifact WAL payload: {}", e))
        })?;

        let mut wal_records = pending
            .iter()
            .map(|row| {
                WalRecord::add(
                    tenant_id_str.clone(),
                    row.chunk_id.to_string(),
                    row.chunk.timestamp_created,
                    row.payload.clone(),
                )
            })
            .collect::<Vec<_>>();
        wal_records.push(WalRecord::task_artifact(
            tenant_id_str.clone(),
            artifact.artifact_id.clone(),
            artifact.timestamp_created,
            task_wal_payload,
        ));
        {
            tenant.append_wal_records(&wal_records, "add_task_artifact")?;
        }

        let mut metadata_rows = Vec::with_capacity(pending.len());
        let mut index_rows = Vec::with_capacity(pending.len());
        for row in &pending {
            tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
            let (segment_id, ordinal) = {
                let mut active = tenant.active_segment.lock();
                let seg = active
                    .as_mut()
                    .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
                let ordinal = seg.writer.append_chunk(&row.payload)?;
                seg.chunk_count += 1;
                (seg.writer.id(), ordinal)
            };

            metadata_rows.push(ChunkMetadata {
                chunk_id: row.chunk_id.clone(),
                tenant_id: row.chunk.tenant_id.clone(),
                project_id: row.chunk.project_id.as_option().map(|s| s.to_string()),
                segment_id,
                ordinal,
                chunk_type: row.chunk.chunk_type,
                status: row.chunk.status,
                timestamp_created: row.chunk.timestamp_created,
                hash: row.chunk.hash.clone(),
                source_uri: row.chunk.source.uri.clone(),
                // A8: writer will populate lifecycle overlay directly once update_lifecycle is wired in.
                lifecycle: crate::types::LifecycleMetadata::default(),
                // D2 round-2: task artifacts are still chunks; populating
                // canonical_text here keeps `idx_chunks_canonical`
                // coverage repo-wide so new task artifacts written after
                // startup are not silently absent from the dedup index.
                // (Codex round-2 D2 MEDIUM finding.)
                canonical_text: Some(crate::store::supersession::canonicalize_for_type(
                    &row.chunk.text,
                    row.chunk.chunk_type,
                )),
                // E1: mirror the chunk's ingestion_mode label. Task
                // artifacts are typically Document but the writer can
                // override.
                ingestion_mode: row.chunk.ingestion_mode,
            });
            index_rows.push((row.chunk_id.clone(), row.chunk.text.clone()));
        }
        // Persist the active segment's payload bytes before the SQLite
        // commit. Without this, a crash between `insert_many` and a
        // later `finalize_active_segment()` leaves metadata rows
        // pointing at `(segment_id, ordinal)` tuples whose bytes are
        // still sitting in the unflushed `BufWriter` and are lost.
        tenant.flush_active_segment_payload()?;
        self.metadata.insert_many(&metadata_rows)?;
        let chunk_ids_for_state: Vec<ChunkId> = metadata_rows
            .iter()
            .map(|row| row.chunk_id.clone())
            .collect();
        self.metadata
            .mark_index_pending(&tenant_id, &chunk_ids_for_state, current_time_ms())?;
        self.metadata.insert_task_artifact_bundle(
            &artifact,
            &projection_chunk_ids,
            &projection_kinds,
        )?;

        if self.async_indexing_enabled() {
            if let Some(indexer) = self.async_indexer.as_ref() {
                let (ack_tx, ack_rx) = oneshot::channel();
                let job = IndexJob {
                    tenant_id: tenant_id.clone(),
                    chunk_ids: chunk_ids_for_state.clone(),
                    index_rows,
                    completion: Some(ack_tx),
                };
                if indexer.job_tx.send(job).is_err() {
                    let error_message = "async indexer queue is closed";
                    warn!(tenant_id = %tenant_id, error = error_message, "failed to enqueue async index job");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        error_message,
                    );
                } else {
                    // Same ack-after-index contract as the add lane.
                    await_index_ack(&tenant_id, ack_rx).await;
                }
            } else {
                let error_message = "async indexing enabled but worker unavailable";
                warn!(tenant_id = %tenant_id, error = error_message, "cannot enqueue async index job");
                mark_index_failed_many(
                    self.metadata.as_ref(),
                    &tenant_id,
                    &chunk_ids_for_state,
                    error_message,
                );
            }
        } else {
            let index_result = if let Some(ref hybrid) = self.hybrid_searcher {
                hybrid.index_batch(&tenant_id, &index_rows).await
            } else if let Some(ref searcher) = self.dense_searcher {
                searcher.index_batch(&tenant_id, &index_rows).await
            } else {
                Ok(())
            };

            match index_result {
                Ok(()) => {
                    self.metadata.mark_indexed(
                        &tenant_id,
                        &chunk_ids_for_state,
                        current_time_ms(),
                    )?;
                }
                Err(e) => {
                    warn!(tenant_id = %tenant_id, error = %e, "sync index batch failed");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        &e.to_string(),
                    );
                }
            }
        }

        self.checkpoint_after_batch(&tenant, &tenant_id_str, (pending.len() + 1) as u32)?;

        Ok(TaskArtifactWriteResult {
            task_id: artifact.task_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            projection_chunk_ids,
        })
    }
}
