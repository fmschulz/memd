use super::*;
use std::collections::{BTreeMap, HashSet};

impl PersistentStore {
    /// Discard stale Add records before purge removes their deletion metadata.
    pub fn checkpoint_tenant_wal(&self, tenant_id: &TenantId) -> Result<()> {
        self.ensure_writable("checkpoint_tenant_wal")?;
        let tenant = self.tenants.read().get(tenant_id.as_str()).cloned();
        if let Some(tenant) = tenant {
            tenant.ensure_wal_recovery_complete()?;
            tenant.finalize_active_segment()?;
            self.metadata.checkpoint_wal()?;
            tenant.truncate_wal()?;
        }
        Ok(())
    }
}

impl TenantStore {
    pub(super) fn open(
        tenant_id: String,
        base_dir: PathBuf,
        metadata: &SqliteMetadataStore,
        segment_max_chunks: u32,
        read_only: bool,
    ) -> Result<Self> {
        let wal_path = base_dir.join("wal.log");
        let wal_reader = WalReader::open(&wal_path)?;
        let wal_overlay = if read_only {
            build_wal_overlay(&wal_reader, &tenant_id)?
        } else {
            HashMap::new()
        };
        let wal_writer = if read_only {
            None
        } else {
            std::fs::create_dir_all(&base_dir)?;
            std::fs::create_dir_all(base_dir.join("segments"))?;
            Some(WalWriter::open_or_create(&wal_path)?)
        };

        let store = Self {
            tenant_id: tenant_id.clone(),
            base_dir,
            read_only,
            active_segment: Mutex::new(None),
            segments: RwLock::new(HashMap::new()),
            wal: Mutex::new(wal_writer),
            wal_recovery_incomplete: AtomicBool::new(false),
            wal_overlay: RwLock::new(wal_overlay),
            writes_since_checkpoint: Mutex::new(0),
            segment_max_chunks,
            segment_id_floor: metadata.max_segment_id(&TenantId::new(&tenant_id)?)? + 1,
        };

        // Load existing segments
        store.load_segments()?;

        if !read_only {
            // Recover locations before choosing which sealed segments to pack.
            store.recover_from_wal(&wal_reader, metadata)?;
            match store.rewrite_finalized_segments(metadata, &TenantId::new(&tenant_id)?) {
                Ok(result) => {
                    for warning in result.warnings {
                        warn!(tenant = %tenant_id, warning, "segment cleanup deferred");
                    }
                }
                Err(error) => {
                    // A corrupt payload or a busy metadata checkpoint must not
                    // prevent other records and tenants from opening.
                    warn!(tenant = %tenant_id, error = %error, "segment packing deferred; source segments retained");
                }
            }
        }

        Ok(store)
    }

    pub(super) fn load_segments(&self) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(());
        }

        let mut segments = self.segments.write();
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let dir_name = entry.file_name();
                if let Some(name) = dir_name.to_str() {
                    if name.starts_with("seg_") && entry.path().join("meta").exists() {
                        // Only load finalized segments (have meta file)
                        match SegmentReader::open(entry.path()) {
                            Ok(reader) => {
                                info!(segment_id = reader.id, "loaded segment");
                                segments.insert(reader.id, reader);
                            }
                            Err(e) => {
                                warn!(path = ?entry.path(), error = %e, "failed to load segment");
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub(super) fn read_payload_fallback(
        &self,
        chunk_id: &ChunkId,
        segment_id: u64,
        ordinal: u32,
    ) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.read_from_active_segment(segment_id, ordinal)? {
            return Ok(Some(bytes));
        }

        {
            let segments = self.segments.read();
            if let Some(reader) = segments.get(&segment_id) {
                return Ok(reader.read_chunk(ordinal)?);
            }
        }

        // In read-only mode we do not replay/truncate WAL. Metadata may
        // point at an unfinalized segment that this process deliberately
        // did not open for append, so serve the original Add payload from
        // an in-memory WAL overlay. WAL Delete records whose metadata write
        // never landed can remain visible here; read-only mode does not try
        // to repair or infer half-applied deletes.
        if self.read_only {
            return Ok(self.wal_overlay.read().get(chunk_id).cloned());
        }

        Ok(None)
    }

    pub(super) fn append_wal_records(&self, records: &[WalRecord], op: &'static str) -> Result<()> {
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore { op: op.to_string() });
        };
        wal.append_batch(records)?;
        Ok(())
    }

    pub(super) fn append_wal_add_batch(
        &self,
        tenant_id: &str,
        records: &[(String, i64, Vec<u8>)],
        op: &'static str,
    ) -> Result<()> {
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore { op: op.to_string() });
        };
        wal.append_add_batch(tenant_id, records)?;
        Ok(())
    }

    pub(super) fn append_wal_delete(
        &self,
        tenant_id: &str,
        chunk_id: &str,
        timestamp: i64,
        op: &'static str,
    ) -> Result<()> {
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore { op: op.to_string() });
        };
        wal.append_delete(tenant_id, chunk_id, timestamp)?;
        Ok(())
    }

    pub(super) fn append_wal_checkpoint(&self, tenant_id: &str, timestamp: i64) -> Result<()> {
        if self.wal_recovery_incomplete.load(Ordering::Acquire) {
            warn!(tenant = %self.tenant_id, "skipping checkpoint that would hide unresolved WAL records");
            return Ok(());
        }
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore {
                op: "append_wal_checkpoint".to_string(),
            });
        };
        wal.append_checkpoint(tenant_id, timestamp)?;
        Ok(())
    }

    pub(super) fn truncate_wal(&self) -> Result<()> {
        self.ensure_wal_recovery_complete()?;
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore {
                op: "truncate_wal".to_string(),
            });
        };
        wal.truncate()?;
        Ok(())
    }

    fn ensure_wal_recovery_complete(&self) -> Result<()> {
        if self.wal_recovery_incomplete.load(Ordering::Acquire) {
            return Err(MemdError::StorageError(format!(
                "cannot discard WAL for tenant {}: recovery has unresolved records",
                self.tenant_id
            )));
        }
        Ok(())
    }

    /// Replay each record independently, preserving SQLite lifecycle/index state.
    pub(super) fn recover_from_wal(
        &self,
        wal_reader: &WalReader,
        metadata: &SqliteMetadataStore,
    ) -> Result<()> {
        let (_, valid_bytes) = wal_reader.read_all_records()?;
        self.wal_recovery_incomplete
            .store(valid_bytes != wal_reader.len(), Ordering::Release);
        if wal_reader.is_empty() {
            return self.remove_unreferenced_segments(metadata, &TenantId::new(&self.tenant_id)?);
        }

        let records = wal_reader.records_for_recovery()?;
        if records.is_empty() {
            return self.remove_unreferenced_segments(metadata, &TenantId::new(&self.tenant_id)?);
        }

        info!(
            records = records.len(),
            tenant = %self.tenant_id,
            "WAL recovery starting"
        );

        let mut adds = 0;
        let mut deletes = 0;
        let mut task_artifacts = 0;
        let mut skipped = 0;
        let mut failed = 0;
        let tenant_id = TenantId::new(&self.tenant_id)?;
        let deleted_ids: HashSet<_> = metadata
            .get_deleted_chunk_ids(&tenant_id)?
            .into_iter()
            .map(|id| id.to_string())
            .chain(
                records
                    .iter()
                    .filter(|record| {
                        record.record_type == WalRecordType::Delete
                            && record.tenant_id == self.tenant_id
                    })
                    .map(|record| record.chunk_id.clone()),
            )
            .collect();

        for record in &records {
            // A tenant WAL must never write another tenant's metadata.
            if record.tenant_id != self.tenant_id {
                warn!(tenant = %self.tenant_id, record_tenant = %record.tenant_id, "skipping foreign tenant WAL record");
                failed += 1;
                continue;
            }
            let result = (|| -> Result<()> {
                match record.record_type {
                    WalRecordType::Add => {
                        // UUIDs identify immutable chunks. A later Delete wins even
                        // when purge has already removed its SQLite row.
                        if deleted_ids.contains(&record.chunk_id) {
                            skipped += 1;
                            return Ok(());
                        }
                        // Check if chunk already exists and is readable
                        let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                            MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                        })?;
                        let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                            MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                        })?;

                        // If metadata exists, check if segment data is readable.
                        // Keep the row for the repair path below: the WAL Add
                        // payload contains the status from initial insertion, but
                        // SQLite may hold a newer lifecycle state (candidate
                        // promoted to final, superseded, expired, or error).
                        let existing_metadata = metadata.get(&tenant_id, &chunk_id)?;
                        if let Some(existing_meta) = existing_metadata.as_ref() {
                            // Try to read from segment to verify data is intact
                            let segments = self.segments.read();
                            if let Some(reader) = segments.get(&existing_meta.segment_id) {
                                if reader
                                    .read_chunk(existing_meta.ordinal)
                                    .ok()
                                    .flatten()
                                    .is_some()
                                {
                                    // Data exists and is readable, skip
                                    skipped += 1;
                                    return Ok(());
                                }
                            }
                            // Metadata exists but segment data is missing or unreadable
                            // This is a crash recovery case - re-write the chunk
                            debug!(
                                chunk_id = %chunk_id,
                                "recovering orphan metadata - segment data missing"
                            );
                        }

                        // Validate every payload before rewriting it. SQLite still
                        // controls lifecycle and index state for an existing row.
                        let chunk: MemoryChunk =
                            serde_json::from_slice(&record.payload).map_err(|e| {
                                MemdError::StorageError(format!("deserialize WAL chunk: {}", e))
                            })?;
                        if chunk.chunk_id != chunk_id || chunk.tenant_id != tenant_id {
                            return Err(MemdError::StorageError(
                                "WAL chunk identity does not match record".into(),
                            ));
                        }

                        // Write to active segment
                        self.get_or_create_active_segment(self.segment_max_chunks)?;
                        let (segment_id, ordinal) = {
                            let mut active = self.active_segment.lock();
                            let seg = active.as_mut().ok_or_else(|| {
                                MemdError::StorageError("no active segment during recovery".into())
                            })?;
                            let ordinal = seg.writer.append_chunk(&record.payload)?;
                            seg.chunk_count += 1;
                            (seg.writer.id(), ordinal)
                        };

                        // Write to metadata
                        if existing_metadata.is_some() {
                            let updated = metadata.repair_recovered_chunk_location_pending(
                                &tenant_id,
                                &chunk_id,
                                segment_id,
                                ordinal,
                                current_time_ms(),
                            )?;
                            if updated != 1 {
                                return Err(MemdError::StorageError(format!(
                                    "WAL repair location update matched {updated} rows"
                                )));
                            }
                        } else {
                            let chunk_meta = ChunkMetadata {
                                chunk_id: chunk.chunk_id.clone(),
                                tenant_id: chunk.tenant_id.clone(),
                                project_id: chunk.project_id.as_option().map(|s| s.to_string()),
                                segment_id,
                                ordinal,
                                chunk_type: chunk.chunk_type,
                                status: chunk.status,
                                timestamp_created: chunk.timestamp_created,
                                hash: chunk.hash.clone(),
                                source_uri: chunk.source.uri.clone(),
                                lifecycle: crate::types::LifecycleMetadata::default(),
                                canonical_text: Some(
                                    crate::store::supersession::canonicalize_for_type(
                                        &chunk.text,
                                        chunk.chunk_type,
                                    ),
                                ),
                                ingestion_mode: chunk.ingestion_mode,
                            };
                            metadata.insert_pending_recovered(&chunk_meta)?;
                        }

                        adds += 1;
                    }
                    WalRecordType::Delete => {
                        // Apply delete: mark in metadata and tombstone
                        let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                            MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                        })?;
                        let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                            MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                        })?;

                        // Get metadata to find segment/ordinal
                        if let Some(meta) = metadata.get(&tenant_id, &chunk_id)? {
                            if meta.status != ChunkStatus::Deleted {
                                // Mark in metadata
                                metadata.mark_deleted(&tenant_id, &chunk_id)?;

                                // Mark the segment tombstone using the same pattern
                                // as the live delete path in `retrieval.rs`: the
                                // per-segment `Arc<RwLock<TombstoneSet>>`
                                // lets us do this under a read lock on the
                                // enclosing map.
                                let segments = self.segments.read();
                                if let Some(reader) = segments.get(&meta.segment_id) {
                                    reader.mark_deleted(meta.ordinal)?;
                                }

                                deletes += 1;
                            } else {
                                skipped += 1;
                            }
                        } else {
                            skipped += 1;
                        }
                    }
                    WalRecordType::TaskArtifact => {
                        let payload: TaskArtifactWalPayload =
                            serde_json::from_slice(&record.payload).map_err(|e| {
                                MemdError::StorageError(format!(
                                    "deserialize WAL task artifact payload: {}",
                                    e
                                ))
                            })?;
                        if payload.artifact.tenant_id != tenant_id {
                            return Err(MemdError::StorageError(
                                "WAL task artifact tenant does not match record".into(),
                            ));
                        }
                        if payload.projection_chunk_ids.len() != payload.projection_kinds.len() {
                            return Err(MemdError::StorageError(
                                "WAL task projection lengths do not match".into(),
                            ));
                        }
                        let had_projections = !payload.projection_chunk_ids.is_empty();
                        let (projection_chunk_ids, projection_kinds): (Vec<_>, Vec<_>) = payload
                            .projection_chunk_ids
                            .into_iter()
                            .zip(payload.projection_kinds)
                            .filter(|(id, _)| !deleted_ids.contains(id))
                            .unzip();
                        if had_projections && projection_chunk_ids.is_empty() {
                            skipped += 1;
                            return Ok(());
                        }
                        metadata.insert_task_artifact_bundle(
                            &payload.artifact,
                            &projection_chunk_ids,
                            &projection_kinds,
                        )?;
                        task_artifacts += 1;
                    }
                    WalRecordType::Checkpoint => {
                        // Checkpoint records are filtered out by records_for_recovery()
                        // but handle gracefully if encountered
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                failed += 1;
                warn!(tenant = %self.tenant_id, chunk_id = %record.chunk_id, record_type = ?record.record_type, error = %error, "skipping failed WAL record");
            }
        }

        info!(
            adds,
            deletes,
            task_artifacts,
            skipped,
            failed,
            tenant = %self.tenant_id,
            "WAL recovery complete"
        );
        if failed > 0 {
            self.wal_recovery_incomplete.store(true, Ordering::Release);
        }

        // Durability barrier before WAL truncation.
        //
        // Recovery above called `append_chunk` on a fresh active
        // segment, wrote metadata rows pointing at `(segment_id,
        // ordinal)`, and the original WAL still holds the source of
        // truth for those chunks. If we truncate now without first
        // finalizing the active segment, a second crash before the
        // next rotation leaves metadata pointing at an unfinalized
        // segment directory (no `meta` file, so startup skips loading
        // it) while the WAL is already empty — the chunks are lost.
        //
        // Finalize the active segment first so the recovered chunks
        // land in a real, meta-backed finalized segment. Only then is
        // WAL truncation safe: everything the WAL described is now
        // durable on disk.
        if adds > 0 {
            self.finalize_active_segment()?;
        }

        // Persist relocation commits before dropping the WAL or obsolete
        // unfinished directories. Retain the source WAL on record failures.
        if let Err(error) = metadata.checkpoint_wal() {
            warn!(tenant = %self.tenant_id, error = %error, "retaining WAL and old segments because metadata checkpoint did not finish");
            return Ok(());
        }
        // Failed records retain their WAL source, not unrelated orphan files.
        // The completed metadata checkpoint makes directory cleanup safe.
        self.remove_unreferenced_segments(metadata, &tenant_id)?;
        if failed == 0 {
            if valid_bytes == wal_reader.len() {
                self.truncate_wal()?;
            } else {
                warn!(tenant = %self.tenant_id, "retaining WAL containing corrupt bytes after recovery");
            }
        }

        Ok(())
    }

    fn remove_unreferenced_segments(
        &self,
        metadata: &SqliteMetadataStore,
        tenant_id: &TenantId,
    ) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        let mut obsolete = Vec::new();
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("seg_"))
                .and_then(|id| id.parse::<u64>().ok())
            else {
                continue;
            };
            if metadata
                .get_by_segment(tenant_id, id)?
                .iter()
                .all(|row| row.status == ChunkStatus::Deleted)
            {
                obsolete.push((id, entry.path()));
            }
        }
        if obsolete.is_empty() {
            return Ok(());
        }
        if let Err(error) = metadata.checkpoint_wal() {
            warn!(tenant = %self.tenant_id, error = %error, "retaining obsolete segments because metadata checkpoint did not finish");
            return Ok(());
        }
        for (id, path) in obsolete {
            std::fs::remove_dir_all(path)?;
            self.segments.write().remove(&id);
        }
        sync_directory_if_exists(&segments_dir)
    }

    pub(super) fn next_segment_id(&self) -> u64 {
        // The previous implementation consulted only segments that had
        // been loaded into memory (finalized dirs with a `meta` file).
        // An unfinalized `seg_N/` left behind by a mid-write crash was
        // invisible, so the next rotation reused id N and `create_dir_all`
        // + `truncate(true)` silently overwrote the prior partial
        // segment — invalidating any metadata rows that still referenced
        // it.
        //
        // Scan the filesystem for all `seg_*` directories (finalized or
        // not) and return one past the maximum id found.
        let from_loaded = {
            let segments = self.segments.read();
            segments.keys().copied().max()
        };

        let mut max_on_disk: Option<u64> = from_loaded;
        let segments_dir = self.base_dir.join("segments");
        if let Ok(entries) = std::fs::read_dir(&segments_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                // Expected format: `seg_NNNNNN` with numeric suffix.
                let Some(rest) = name.strip_prefix("seg_") else {
                    continue;
                };
                let Ok(id) = rest.parse::<u64>() else {
                    continue;
                };
                max_on_disk = Some(max_on_disk.map_or(id, |prev| prev.max(id)));
            }
        }

        max_on_disk
            .map(|id| id + 1)
            .unwrap_or(1)
            .max(self.segment_id_floor)
    }

    pub(super) fn get_or_create_active_segment(&self, max_chunks: u32) -> Result<()> {
        if self.read_only {
            return Err(MemdError::ReadOnlyStore {
                op: "get_or_create_active_segment".to_string(),
            });
        }
        let mut active = self.active_segment.lock();

        if active.is_some() {
            let seg = active.as_ref().unwrap();
            if seg.chunk_count < max_chunks {
                return Ok(());
            }
            // Need to rotate - finalize current segment
            let seg = active.take().unwrap();
            let meta = seg.writer.finalize()?;
            info!(
                segment_id = meta.id,
                chunks = meta.chunk_count,
                "segment finalized"
            );

            // Load as reader
            let segments_dir = self.base_dir.join("segments");
            let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
            let reader = SegmentReader::open(seg_dir)?;
            self.segments.write().insert(meta.id, reader);
        }

        // Create new segment
        let segment_id = self.next_segment_id();
        let segments_dir = self.base_dir.join("segments");
        let writer = SegmentWriter::create(&segments_dir, segment_id)?;

        *active = Some(ActiveSegment {
            writer,
            chunk_count: 0,
        });

        Ok(())
    }

    /// Flush and fsync the active segment's `payload.bin` without
    /// finalizing the segment.
    ///
    /// Called from the chunk/artifact write paths between
    /// `append_chunk` and the SQLite `insert_many` so that, on crash,
    /// no metadata row survives that references bytes only ever present
    /// in the in-memory `BufWriter`.
    pub(super) fn flush_active_segment_payload(&self) -> Result<()> {
        if self.read_only {
            return Err(MemdError::ReadOnlyStore {
                op: "flush_active_segment_payload".to_string(),
            });
        }
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.as_mut() {
            seg.writer.flush_payload()?;
        }
        Ok(())
    }

    /// Seal active payloads before shutdown or discarding their WAL records.
    pub(super) fn finalize_active_segment(&self) -> Result<()> {
        if self.read_only {
            tracing::debug!(
                tenant = %self.tenant_id,
                "skipping active segment finalize for read-only tenant"
            );
            return Ok(());
        }
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.take() {
            if seg.chunk_count > 0 {
                let meta = seg.writer.finalize()?;
                info!(
                    segment_id = meta.id,
                    chunks = meta.chunk_count,
                    tenant = %self.tenant_id,
                    "segment finalized on shutdown"
                );

                // Load as reader
                let segments_dir = self.base_dir.join("segments");
                let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
                let reader = SegmentReader::open(seg_dir)?;
                self.segments.write().insert(meta.id, reader);
            }
        }
        Ok(())
    }

    /// Read chunk from active segment by ordinal
    pub(super) fn read_from_active_segment(
        &self,
        segment_id: u64,
        ordinal: u32,
    ) -> Result<Option<Vec<u8>>> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.as_mut() {
            if seg.writer.id() == segment_id {
                return seg.writer.read_chunk(ordinal);
            }
        }
        Ok(None)
    }

    pub(super) fn rewrite_finalized_segments(
        &self,
        metadata: &SqliteMetadataStore,
        tenant_id: &TenantId,
    ) -> Result<SegmentRewriteResult> {
        if self.read_only {
            return Err(MemdError::ReadOnlyStore {
                op: "rewrite_finalized_segments".to_string(),
            });
        }
        // Seal the active writer first so a purge in a short-lived CLI
        // process can reclaim bytes from chunks written in the same run.
        self.finalize_active_segment()?;
        let _active_guard = self.active_segment.lock();

        let mut next_segment_id = self.next_segment_id();
        let mut segment_ids = {
            let segments = self.segments.read();
            segments.keys().copied().collect::<Vec<_>>()
        };
        segment_ids.sort_unstable();

        // Plan carries before copying. Two groups in tier k produce a group
        // in tier k+1, so each copied chunk advances a tier. Stop at half
        // capacity: joining two smaller groups cannot exceed capacity.
        let full = self.segment_max_chunks.div_ceil(2).max(1) as usize;
        let mut tiers: BTreeMap<u32, (usize, Vec<u64>)> = BTreeMap::new();
        let mut groups = Vec::new();
        for id in segment_ids {
            let mut count = metadata
                .get_by_segment(tenant_id, id)?
                .iter()
                .filter(|row| row.status != ChunkStatus::Deleted)
                .count();
            let mut ids = vec![id];
            while count > 0 && count < full {
                let tier = count.ilog2();
                if let Some((other_count, other_ids)) = tiers.remove(&tier) {
                    count += other_count;
                    ids.extend(other_ids);
                } else {
                    tiers.insert(tier, (count, ids));
                    ids = Vec::new();
                    break;
                }
            }
            if !ids.is_empty() {
                groups.push(ids);
            }
        }
        groups.extend(tiers.into_values().map(|(_, ids)| ids));

        let segments_dir = self.base_dir.join("segments");
        let mut result = SegmentRewriteResult::default();
        for ids in groups {
            let mut segments = self.segments.write();
            let mut rows = Vec::new();
            let mut old_dirs = Vec::new();
            let mut old_size = 0u64;
            let mut needs_rewrite = ids.len() > 1;
            for id in &ids {
                let reader = &segments[id];
                let live = metadata
                    .get_by_segment(tenant_id, *id)?
                    .into_iter()
                    .filter(|row| row.status != ChunkStatus::Deleted)
                    .collect::<Vec<_>>();
                needs_rewrite |= live.len() != reader.chunk_count() as usize
                    || reader.active_count() != reader.chunk_count();
                rows.extend(live);
                old_size = old_size.saturating_add(path_size(reader.dir())?);
                old_dirs.push(reader.dir().to_path_buf());
            }
            if !needs_rewrite {
                continue;
            }

            let mut new_size = 0;
            if !rows.is_empty() {
                // Validate every source before creating a replacement, so a
                // bad payload cannot leave a partial merge or relocate rows.
                let payloads = rows.iter().map(|row| {
                    segments[&row.segment_id].read_chunk(row.ordinal)?.ok_or_else(|| {
                        MemdError::StorageError(format!(
                            "live metadata row {} points at missing/tombstoned segment {} ordinal {}",
                            row.chunk_id, row.segment_id, row.ordinal
                        ))
                    })
                }).collect::<Result<Vec<_>>>()?;
                let new_id = next_segment_id;
                next_segment_id += 1;
                let mut writer = SegmentWriter::create(&segments_dir, new_id)?;
                let mut relocations = Vec::with_capacity(rows.len());
                for (row, payload) in rows.iter().zip(&payloads) {
                    relocations.push((row.chunk_id.clone(), new_id, writer.append_chunk(payload)?));
                }
                writer.finalize()?;
                // finalize syncs the files and child directory. Persist the
                // parent's new entry before committing metadata locations.
                sync_directory_if_exists(&segments_dir)?;
                let new_dir = segments_dir.join(format!("seg_{:06}", new_id));
                let new_reader = SegmentReader::open(new_dir.clone())?;
                new_size = path_size(&new_dir)?;
                // On an uncertain commit outcome retain both copies. Recovery
                // uses SQLite locations to decide which directories are old.
                segments.insert(new_id, new_reader);
                metadata.update_chunk_locations(tenant_id, &relocations)?;
            }
            // Includes the all-deleted case: deletion metadata must also be
            // durable before its source bytes can disappear.
            metadata.checkpoint_wal()?;
            for (id, dir) in ids.iter().zip(&old_dirs) {
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => {
                        segments.remove(id);
                    }
                    Err(err) => result
                        .warnings
                        .push(format!("failed to remove obsolete segment {id}: {err}")),
                }
            }
            result.segments_rewritten += ids.len();
            if rows.is_empty() {
                result.segments_removed += ids.len();
            }
            result.chunks_moved += rows.len();
            result.bytes_before = result.bytes_before.saturating_add(old_size);
            result.bytes_after = result.bytes_after.saturating_add(new_size);
            result.bytes_reclaimed = result
                .bytes_reclaimed
                .saturating_add(old_size.saturating_sub(new_size));
        }

        sync_directory_if_exists(&segments_dir)?;
        Ok(result)
    }
}

fn build_wal_overlay(wal_reader: &WalReader, tenant_id: &str) -> Result<HashMap<ChunkId, Vec<u8>>> {
    let mut overlay = HashMap::new();
    if wal_reader.is_empty() {
        return Ok(overlay);
    }

    for record in wal_reader.records_for_recovery()? {
        if record.record_type != WalRecordType::Add || record.tenant_id != tenant_id {
            continue;
        }
        let chunk_id = match ChunkId::parse(&record.chunk_id) {
            Ok(id) => id,
            Err(error) => {
                warn!(tenant_id, error = %error, "skipping invalid chunk id in read-only WAL overlay");
                continue;
            }
        };
        overlay.insert(chunk_id, record.payload);
    }

    if !overlay.is_empty() {
        debug!(
            tenant_id = tenant_id,
            pending = overlay.len(),
            "built read-only WAL payload overlay"
        );
    }

    Ok(overlay)
}

fn path_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return path.metadata().map(|m| m.len()).map_err(MemdError::IoError);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).map_err(MemdError::IoError)? {
        let entry = entry.map_err(MemdError::IoError)?;
        total = total.saturating_add(path_size(&entry.path())?);
    }
    Ok(total)
}

fn sync_directory_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let dir = std::fs::File::open(path).map_err(MemdError::IoError)?;
    dir.sync_all().map_err(MemdError::IoError)
}

impl Drop for TenantStore {
    fn drop(&mut self) {
        if let Err(e) = self.finalize_active_segment() {
            warn!(
                tenant = %self.tenant_id,
                error = %e,
                "failed to finalize segment on TenantStore drop"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, LifecycleMetadata};
    use tempfile::tempdir;

    fn chunk() -> MemoryChunk {
        MemoryChunk::new(
            TenantId::new("recovery").unwrap(),
            "recover me",
            ChunkType::Doc,
        )
    }

    fn add_record(chunk: &MemoryChunk) -> WalRecord {
        WalRecord::add(
            chunk.tenant_id.to_string(),
            chunk.chunk_id.to_string(),
            chunk.timestamp_created,
            serde_json::to_vec(chunk).unwrap(),
        )
    }

    fn row(chunk: &MemoryChunk) -> ChunkMetadata {
        ChunkMetadata {
            chunk_id: chunk.chunk_id.clone(),
            tenant_id: chunk.tenant_id.clone(),
            project_id: None,
            segment_id: 1,
            ordinal: 0,
            chunk_type: chunk.chunk_type,
            status: chunk.status,
            timestamp_created: chunk.timestamp_created,
            hash: chunk.hash.clone(),
            source_uri: None,
            lifecycle: LifecycleMetadata::default(),
            canonical_text: None,
            ingestion_mode: chunk.ingestion_mode,
        }
    }

    fn write_records(base: &Path, records: &[WalRecord]) {
        std::fs::create_dir_all(base).unwrap();
        WalWriter::open_or_create(&base.join("wal.log"))
            .unwrap()
            .append_batch(records)
            .unwrap();
    }

    fn open(base: &Path, metadata: &SqliteMetadataStore) -> Result<TenantStore> {
        TenantStore::open(
            "recovery".to_string(),
            base.to_path_buf(),
            metadata,
            100,
            false,
        )
    }

    #[test]
    fn wal_repair_preserves_pending_index_state() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        metadata.insert(&row(&chunk)).unwrap();
        metadata
            .mark_index_pending(&chunk.tenant_id, std::slice::from_ref(&chunk.chunk_id), 123)
            .unwrap();
        write_records(dir.path(), &[add_record(&chunk)]);
        let _store = open(dir.path(), &metadata).unwrap();
        assert_eq!(
            metadata.count_by_index_state(&chunk.tenant_id).unwrap(),
            (1, 0, 0)
        );
    }

    #[test]
    fn wal_new_chunk_remains_pending_until_indexed() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        write_records(dir.path(), &[add_record(&chunk)]);
        let _store = open(dir.path(), &metadata).unwrap();
        assert_eq!(
            metadata.count_by_index_state(&chunk.tenant_id).unwrap(),
            (1, 0, 0)
        );
    }

    #[test]
    fn wal_physical_repair_marks_previously_indexed_chunk_pending() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        metadata.insert(&row(&chunk)).unwrap();
        assert_eq!(
            metadata.count_by_index_state(&chunk.tenant_id).unwrap(),
            (0, 1, 0)
        );
        write_records(dir.path(), &[add_record(&chunk)]);
        let _store = open(dir.path(), &metadata).unwrap();
        assert_eq!(
            metadata.count_by_index_state(&chunk.tenant_id).unwrap(),
            (1, 0, 0),
            "physical repair must queue indexes that may have skipped the missing payload"
        );
    }

    #[test]
    fn wal_add_does_not_resurrect_soft_deleted_chunk() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        metadata.insert(&row(&chunk)).unwrap();
        metadata
            .mark_deleted(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap();
        write_records(dir.path(), &[add_record(&chunk)]);
        let store = open(dir.path(), &metadata).unwrap();
        assert!(metadata
            .get(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap()
            .is_none());
        assert!(
            store.segments.read().is_empty(),
            "deleted bytes must not be replayed"
        );
    }

    #[test]
    fn wal_add_delete_does_not_recreate_purged_row() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        metadata.insert(&row(&chunk)).unwrap();
        metadata
            .mark_deleted(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap();
        metadata
            .hard_delete_chunks(&chunk.tenant_id, std::slice::from_ref(&chunk.chunk_id))
            .unwrap();
        write_records(
            dir.path(),
            &[
                add_record(&chunk),
                WalRecord::delete(chunk.tenant_id.to_string(), chunk.chunk_id.to_string(), 456),
            ],
        );
        let store = open(dir.path(), &metadata).unwrap();
        assert!(
            metadata
                .get_deleted_chunk_ids(&chunk.tenant_id)
                .unwrap()
                .is_empty(),
            "purged row was recreated"
        );
        assert!(
            store.segments.read().is_empty(),
            "purged bytes must not be replayed"
        );
    }

    #[test]
    fn wal_bad_record_does_not_block_later_valid_chunk() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        let bad = WalRecord::add(
            "recovery".to_string(),
            ChunkId::new().to_string(),
            1,
            b"invalid json".to_vec(),
        );
        write_records(dir.path(), &[bad, add_record(&chunk)]);
        let store =
            open(dir.path(), &metadata).expect("one invalid payload must not block store open");
        let meta = metadata
            .get(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap()
            .unwrap();
        assert!(store
            .read_payload_fallback(&chunk.chunk_id, meta.segment_id, meta.ordinal)
            .unwrap()
            .is_some());
    }

    #[test]
    fn wal_task_artifact_does_not_restore_purged_projection_link() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        let artifact = TaskArtifact::new_task_start(chunk.tenant_id.clone());
        let payload = TaskArtifactWalPayload {
            artifact: artifact.clone(),
            projection_chunk_ids: vec![chunk.chunk_id.to_string()],
            projection_kinds: vec!["task".to_string()],
        };
        metadata.insert(&row(&chunk)).unwrap();
        metadata
            .insert_task_artifact_bundle(
                &artifact,
                &payload.projection_chunk_ids,
                &payload.projection_kinds,
            )
            .unwrap();
        metadata
            .mark_deleted(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap();
        metadata
            .hard_delete_chunks(&chunk.tenant_id, std::slice::from_ref(&chunk.chunk_id))
            .unwrap();
        write_records(
            dir.path(),
            &[
                add_record(&chunk),
                WalRecord::task_artifact(
                    chunk.tenant_id.to_string(),
                    artifact.artifact_id.clone(),
                    123,
                    serde_json::to_vec(&payload).unwrap(),
                ),
                WalRecord::delete(chunk.tenant_id.to_string(), chunk.chunk_id.to_string(), 456),
            ],
        );
        let _store = open(dir.path(), &metadata).unwrap();
        assert!(metadata
            .search_task_projection_chunk_ids(&chunk.tenant_id, &TaskSearchFilters::default(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn wal_purge_barrier_refuses_incomplete_recovery_without_losing_source() {
        let dir = tempdir().unwrap();
        let tenant = TenantId::new("recovery").unwrap();
        let base = dir.path().join("tenants").join(tenant.as_str());
        let bad = WalRecord::add(
            tenant.to_string(),
            ChunkId::new().to_string(),
            1,
            b"invalid payload".to_vec(),
        );
        write_records(&base, &[bad]);
        let before = std::fs::read(base.join("wal.log")).unwrap();
        let store = PersistentStore::open(PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        })
        .unwrap();
        assert!(
            store.checkpoint_tenant_wal(&tenant).is_err(),
            "purge must refuse a WAL retained for failed recovery"
        );
        let tenant_store = store.get_or_create_tenant(tenant.as_str()).unwrap();
        assert!(tenant_store.truncate_wal().is_err());
        tenant_store
            .append_wal_checkpoint(tenant.as_str(), 456)
            .unwrap();
        assert_eq!(std::fs::read(base.join("wal.log")).unwrap(), before);
    }

    #[test]
    fn wal_existing_row_repair_rejects_mismatched_payload_identity() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let original = chunk();
        let different = chunk();
        metadata.insert(&row(&original)).unwrap();
        let bad = WalRecord::add(
            original.tenant_id.to_string(),
            original.chunk_id.to_string(),
            original.timestamp_created,
            serde_json::to_vec(&different).unwrap(),
        );
        write_records(dir.path(), &[bad]);
        let store = open(dir.path(), &metadata).unwrap();
        assert!(
            store.segments.read().is_empty(),
            "repair wrote another chunk's payload at the existing row's coordinates"
        );
        assert!(!std::fs::read(dir.path().join("wal.log"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn wal_reopen_does_not_rewrite_small_segment_or_leave_orphan() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        let store = open(dir.path(), &metadata).unwrap();
        store
            .append_wal_records(&[add_record(&chunk)], "test")
            .unwrap();
        store.get_or_create_active_segment(100).unwrap();
        store
            .active_segment
            .lock()
            .as_mut()
            .unwrap()
            .writer
            .append_chunk(&serde_json::to_vec(&chunk).unwrap())
            .unwrap();
        store.active_segment.lock().as_mut().unwrap().chunk_count += 1;
        metadata.insert(&row(&chunk)).unwrap();
        drop(store);
        let _store = open(dir.path(), &metadata).unwrap();
        let meta = metadata
            .get(&chunk.tenant_id, &chunk.chunk_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            meta.segment_id, 1,
            "gracefully closed payload must retain its coordinates"
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join("segments"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn wal_recovery_removes_unreferenced_unfinished_directory() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let chunk = chunk();
        let segments = dir.path().join("segments");
        let mut writer = SegmentWriter::create(&segments, 1).unwrap();
        writer
            .append_chunk(&serde_json::to_vec(&chunk).unwrap())
            .unwrap();
        writer.flush_payload().unwrap();
        drop(writer);
        metadata.insert(&row(&chunk)).unwrap();
        write_records(dir.path(), &[add_record(&chunk)]);
        let _store = open(dir.path(), &metadata).unwrap();
        assert!(
            !segments.join("seg_000001").exists(),
            "recovery left obsolete unfinished payload bytes on disk"
        );
        assert_eq!(std::fs::read_dir(segments).unwrap().count(), 1);
    }

    #[test]
    fn wal_empty_startup_removes_old_unreferenced_unfinished_directory() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let segments = dir.path().join("segments");
        let mut writer = SegmentWriter::create(&segments, 1).unwrap();
        writer.append_chunk(b"obsolete payload").unwrap();
        writer.flush_payload().unwrap();
        drop(writer);
        let _store = open(dir.path(), &metadata).unwrap();
        assert!(!segments.join("seg_000001").exists());
    }

    #[test]
    fn packing_preserves_pending_and_unresolved_wal() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let tenant = TenantId::new("recovery").unwrap();
        // A crash can leave unrelated unfinished bytes next to a bad WAL.
        // Retaining that WAL must not also retain unreferenced payload files.
        let orphan = dir.path().join("segments/seg_009999");
        let mut unfinished = SegmentWriter::create(&dir.path().join("segments"), 9999).unwrap();
        unfinished.append_chunk(b"purged orphan plaintext").unwrap();
        unfinished.flush_payload().unwrap();
        drop(unfinished);
        let mut chunks = Vec::new();
        for id in 1..=5 {
            let chunk = chunk();
            let mut writer = SegmentWriter::create(&dir.path().join("segments"), id).unwrap();
            writer
                .append_chunk(&serde_json::to_vec(&chunk).unwrap())
                .unwrap();
            writer.finalize().unwrap();
            let mut meta = row(&chunk);
            meta.segment_id = id;
            metadata.insert(&meta).unwrap();
            metadata
                .mark_index_pending(&tenant, std::slice::from_ref(&chunk.chunk_id), 123)
                .unwrap();
            chunks.push(chunk);
        }
        metadata.mark_deleted(&tenant, &chunks[3].chunk_id).unwrap();
        metadata.mark_deleted(&tenant, &chunks[4].chunk_id).unwrap();
        metadata
            .hard_delete_chunks(&tenant, std::slice::from_ref(&chunks[4].chunk_id))
            .unwrap();
        let bad = WalRecord::add(
            tenant.to_string(),
            ChunkId::new().to_string(),
            1,
            b"bad json".to_vec(),
        );
        write_records(
            dir.path(),
            &[
                bad,
                add_record(&chunks[3]),
                add_record(&chunks[4]),
                WalRecord::delete(tenant.to_string(), chunks[4].chunk_id.to_string(), 456),
            ],
        );
        let before = std::fs::read(dir.path().join("wal.log")).unwrap();
        let store = open(dir.path(), &metadata).unwrap();
        assert_eq!(metadata.count_by_index_state(&tenant).unwrap(), (3, 0, 0));
        assert_eq!(
            store.segments.read().len(),
            2,
            "three live one-chunk segments must pack"
        );
        for chunk in &chunks[..3] {
            let meta = metadata.get(&tenant, &chunk.chunk_id).unwrap().unwrap();
            assert_eq!(
                store
                    .read_payload_fallback(&chunk.chunk_id, meta.segment_id, meta.ordinal)
                    .unwrap()
                    .unwrap(),
                serde_json::to_vec(chunk).unwrap()
            );
        }
        for chunk in &chunks[3..] {
            assert!(metadata.get(&tenant, &chunk.chunk_id).unwrap().is_none());
        }
        assert_eq!(
            metadata.get_deleted_chunk_ids(&tenant).unwrap(),
            vec![chunks[3].chunk_id.clone()]
        );
        assert!(store.truncate_wal().is_err());
        assert_eq!(std::fs::read(dir.path().join("wal.log")).unwrap(), before);
        assert!(
            !orphan.exists(),
            "unresolved WAL retained an unrelated orphan"
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join("segments"))
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn packing_leaves_full_segments_immutable() {
        let dir = tempdir().unwrap();
        let metadata = SqliteMetadataStore::open_in_memory().unwrap();
        let tenant = TenantId::new("recovery").unwrap();
        let segments_dir = dir.path().join("segments");
        for (id, count) in [(1, 4), (2, 2), (3, 1), (4, 1)] {
            let mut writer = SegmentWriter::create(&segments_dir, id).unwrap();
            for ordinal in 0..count {
                let chunk = chunk();
                writer
                    .append_chunk(&serde_json::to_vec(&chunk).unwrap())
                    .unwrap();
                let mut meta = row(&chunk);
                meta.segment_id = id;
                meta.ordinal = ordinal;
                metadata.insert(&meta).unwrap();
            }
            writer.finalize().unwrap();
        }
        let full = segments_dir.join("seg_000001/payload.bin");
        let before = (
            full.metadata().unwrap().modified().unwrap(),
            std::fs::read(&full).unwrap(),
        );
        let store = TenantStore::open(
            tenant.to_string(),
            dir.path().to_path_buf(),
            &metadata,
            8,
            false,
        )
        .unwrap();
        assert_eq!(store.segments.read().len(), 2);
        assert!(store
            .segments
            .read()
            .values()
            .all(|reader| reader.chunk_count() == 4));
        assert_eq!(
            (
                full.metadata().unwrap().modified().unwrap(),
                std::fs::read(&full).unwrap()
            ),
            before
        );
        assert_eq!(std::fs::read_dir(segments_dir).unwrap().count(), 2);
    }
}
