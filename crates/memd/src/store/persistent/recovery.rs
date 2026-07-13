use super::*;

impl TenantStore {
    pub(super) fn open(
        tenant_id: String,
        base_dir: PathBuf,
        metadata: &SqliteMetadataStore,
        segment_max_chunks: u32,
        min_finalize_chunks: u32,
        wal_checkpoint_interval: u32,
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
            wal_overlay: RwLock::new(wal_overlay),
            writes_since_checkpoint: Mutex::new(0),
            segment_max_chunks,
            min_finalize_chunks,
            wal_checkpoint_interval,
        };

        // Load existing segments
        store.load_segments()?;

        if !read_only {
            // Recover from WAL - FULL IMPLEMENTATION
            store.recover_from_wal(&wal_reader, metadata)?;
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
        let mut wal = self.wal.lock();
        let Some(wal) = wal.as_mut() else {
            return Err(MemdError::ReadOnlyStore {
                op: "truncate_wal".to_string(),
            });
        };
        wal.truncate()?;
        Ok(())
    }

    /// Full WAL recovery implementation
    ///
    /// Replays Add and Delete records from WAL to restore uncommitted state.
    /// Idempotent: skips records for chunks that already exist in metadata.
    pub(super) fn recover_from_wal(
        &self,
        wal_reader: &WalReader,
        metadata: &SqliteMetadataStore,
    ) -> Result<()> {
        if wal_reader.is_empty() {
            return Ok(());
        }

        let records = wal_reader.records_for_recovery()?;
        if records.is_empty() {
            return Ok(());
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

        for record in &records {
            match record.record_type {
                WalRecordType::Add => {
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
                                continue;
                            }
                        }
                        // Metadata exists but segment data is missing or unreadable
                        // This is a crash recovery case - re-write the chunk
                        debug!(
                            chunk_id = %chunk_id,
                            "recovering orphan metadata - segment data missing"
                        );
                    }

                    // Deserialize chunk from payload
                    let chunk: MemoryChunk =
                        serde_json::from_slice(&record.payload).map_err(|e| {
                            MemdError::StorageError(format!("deserialize WAL chunk: {}", e))
                        })?;

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
                    let chunk_meta = if let Some(mut current) = existing_metadata {
                        // Repair only the physical payload coordinates. SQLite
                        // is authoritative for every lifecycle field that may
                        // have changed after the original Add record.
                        current.segment_id = segment_id;
                        current.ordinal = ordinal;
                        current
                    } else {
                        ChunkMetadata {
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
                        }
                    };
                    metadata.insert(&chunk_meta)?;

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
                    let payload: TaskArtifactWalPayload = serde_json::from_slice(&record.payload)
                        .map_err(|e| {
                        MemdError::StorageError(format!(
                            "deserialize WAL task artifact payload: {}",
                            e
                        ))
                    })?;
                    metadata.insert_task_artifact_bundle(
                        &payload.artifact,
                        &payload.projection_chunk_ids,
                        &payload.projection_kinds,
                    )?;
                    task_artifacts += 1;
                }
                WalRecordType::Checkpoint => {
                    // Checkpoint records are filtered out by records_for_recovery()
                    // but handle gracefully if encountered
                }
            }
        }

        info!(
            adds,
            deletes,
            task_artifacts,
            skipped,
            tenant = %self.tenant_id,
            "WAL recovery complete"
        );

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

        // After durable recovery, truncate WAL to start fresh.
        self.truncate_wal()?;

        Ok(())
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

        max_on_disk.map(|id| id + 1).unwrap_or(1)
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

    /// Finalize active segment for graceful shutdown.
    ///
    /// Always seals the segment when one exists (no `min_finalize_chunks`
    /// gate here). Callers that want the threshold gate must use
    /// `finalize_active_segment_if_above_threshold` instead. The WAL
    /// recovery path at the durability barrier in `recover_from_wal`
    /// must keep using this unconditional method — recovered chunks
    /// have to land in a finalized segment before WAL truncation, or a
    /// second crash strands them.
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

    /// Variant of `finalize_active_segment` that honors the
    /// `min_finalize_chunks` threshold. Used by graceful-shutdown and
    /// Drop paths so a CLI invocation that wrote 1-2 chunks does not
    /// seal its active segment; instead the segment is left in place
    /// and grown by future invocations. The chunks remain durable
    /// because the WAL has already persisted them and `recover_from_wal`
    /// replays them into the active segment on next startup.
    ///
    /// CRASH SAFETY: the gate is disabled when
    /// `wal_checkpoint_interval > 0`. With checkpointing enabled the
    /// WAL may already have been truncated past the records that would
    /// be needed to recover the unfinalized active segment, so leaving
    /// it unfinalized would lose chunks. Default config has
    /// `wal_checkpoint_interval = 0` (the v0.3.1 safety valve), so the
    /// gate is active for typical deployments.
    ///
    /// KNOWN LIMITATION: this gate is necessary but not sufficient to
    /// eliminate the segment-per-CLI-call pathology. On the next
    /// startup, `recover_from_wal` replays the WAL records: it finds
    /// metadata pointing to the unfinalized segment, the segment cache
    /// does not have it (load_segments skips dirs without `meta`), and
    /// the "missing or unreadable" branch rewrites each chunk into a
    /// freshly created active segment that the WAL durability barrier
    /// then finalizes. A full fix requires reopening the existing
    /// unfinalized segment for append (tracked as future work — needs
    /// incremental payload.idx persistence). Today this method makes
    /// the gate visible and configurable so the eventual segment-reuse
    /// path has a stable entry point.
    pub(super) fn finalize_active_segment_if_above_threshold(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        if self.wal_checkpoint_interval > 0 {
            return self.finalize_active_segment();
        }
        let active_chunks = {
            let active = self.active_segment.lock();
            active.as_ref().map(|s| s.chunk_count).unwrap_or(0)
        };
        if active_chunks < self.min_finalize_chunks {
            tracing::debug!(
                chunks = active_chunks,
                threshold = self.min_finalize_chunks,
                tenant = %self.tenant_id,
                "skipping finalize on shutdown: active segment below threshold"
            );
            return Ok(());
        }
        self.finalize_active_segment()
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

        let segments_dir = self.base_dir.join("segments");
        let mut result = SegmentRewriteResult::default();

        for old_segment_id in segment_ids {
            let rows = metadata.get_by_segment(tenant_id, old_segment_id)?;
            let live_rows = rows
                .iter()
                .filter(|row| row.status != ChunkStatus::Deleted)
                .collect::<Vec<_>>();

            let mut segments = self.segments.write();
            let Some(reader) = segments.get(&old_segment_id) else {
                continue;
            };

            let chunk_count = reader.chunk_count() as usize;
            let active_count = reader.active_count() as usize;
            if live_rows.len() == chunk_count && active_count == chunk_count {
                continue;
            }

            let old_dir = reader.dir().to_path_buf();
            let old_size = path_size(&old_dir)?;
            result.bytes_before = result.bytes_before.saturating_add(old_size);

            if live_rows.is_empty() {
                segments.remove(&old_segment_id);
                match std::fs::remove_dir_all(&old_dir) {
                    Ok(()) => {
                        result.segments_removed += 1;
                        result.segments_rewritten += 1;
                        result.bytes_reclaimed = result.bytes_reclaimed.saturating_add(old_size);
                    }
                    Err(err) => result.warnings.push(format!(
                        "failed to remove obsolete segment {}: {}",
                        old_segment_id, err
                    )),
                }
                continue;
            }

            let mut payloads = Vec::with_capacity(live_rows.len());
            for row in &live_rows {
                let payload = reader.read_chunk(row.ordinal)?.ok_or_else(|| {
                    MemdError::StorageError(format!(
                        "live metadata row {} points at missing/tombstoned segment {} ordinal {}",
                        row.chunk_id, old_segment_id, row.ordinal
                    ))
                })?;
                payloads.push((row.chunk_id.clone(), payload));
            }

            let new_segment_id = next_segment_id;
            next_segment_id += 1;
            let mut writer = SegmentWriter::create(&segments_dir, new_segment_id)?;
            let mut relocations = Vec::with_capacity(payloads.len());
            for (chunk_id, payload) in &payloads {
                let new_ordinal = writer.append_chunk(payload)?;
                relocations.push((chunk_id.clone(), new_segment_id, new_ordinal));
            }
            writer.finalize()?;
            let new_dir = segments_dir.join(format!("seg_{:06}", new_segment_id));
            let new_reader = SegmentReader::open(new_dir.clone())?;
            let new_size = path_size(&new_dir)?;

            if let Err(err) = metadata.update_chunk_locations(tenant_id, &relocations) {
                let _ = std::fs::remove_dir_all(&new_dir);
                return Err(err);
            }

            segments.remove(&old_segment_id);
            segments.insert(new_segment_id, new_reader);
            match std::fs::remove_dir_all(&old_dir) {
                Ok(()) => {}
                Err(err) => result.warnings.push(format!(
                    "failed to remove rewritten segment {}: {}",
                    old_segment_id, err
                )),
            }

            result.segments_rewritten += 1;
            result.chunks_moved += relocations.len();
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
        let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
            MemdError::StorageError(format!("invalid chunk_id in WAL overlay: {}", e))
        })?;
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
        // Best-effort finalization on drop. Same gate as the shutdown
        // path: tiny segments are kept across runs to avoid the
        // segment-per-CLI-call pathology.
        if let Err(e) = self.finalize_active_segment_if_above_threshold() {
            warn!(
                tenant = %self.tenant_id,
                error = %e,
                "failed to finalize segment on TenantStore drop"
            );
        }
    }
}
