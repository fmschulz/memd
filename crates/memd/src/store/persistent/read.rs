use super::*;

impl PersistentStore {
    pub(super) async fn get_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> Result<Option<MemoryChunk>> {
        // Query metadata first
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(None),
        };

        // Load from segment
        let tenant_str = tenant_id.to_string();
        let tenant = match self.tenants.read().get(&tenant_str) {
            Some(t) => Arc::clone(t),
            None => return Ok(None),
        };

        if let Some(bytes) =
            tenant.read_payload_fallback(chunk_id, meta.segment_id, meta.ordinal)?
        {
            let mut chunk: MemoryChunk = serde_json::from_slice(&bytes)
                .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
            chunk.status = meta.status;
            return Ok(Some(chunk));
        }

        // Cache miss. Metadata says the chunk exists at (segment_id, ordinal)
        // but the segment reader is not in `tenant.segments`. This should not
        // happen — startup `load_segments()` and every rollover insert into
        // the map. The previous implementation returned `Ok(None)` here,
        // which surfaces as a silent "chunk not found" and masks the real
        // inconsistency. Instead, try to open the segment on demand so the
        // read still succeeds, and log loudly so the drift is observable.
        //
        // On the explicit `memory.get` path the returned error is surfaced to
        // the caller (handlers.rs swallows only in `get_chunk_for_retrieval`),
        // so a truly missing/corrupt segment produces a real error instead of
        // a false "not found".
        let seg_dir = tenant
            .base_dir
            .join("segments")
            .join(format!("seg_{:06}", meta.segment_id));
        let reader = SegmentReader::open(seg_dir).map_err(|e| {
            warn!(
                tenant_id = %tenant_id,
                chunk_id = %chunk_id,
                segment_id = meta.segment_id,
                ordinal = meta.ordinal,
                error = %e,
                "segment reader missing from cache and on-demand open failed"
            );
            MemdError::StorageError(format!(
                "segment {} missing from cache; on-demand open failed: {}",
                meta.segment_id, e
            ))
        })?;

        warn!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            segment_id = meta.segment_id,
            ordinal = meta.ordinal,
            "segment reader missing from cache; opened on demand (cache drift)"
        );
        let payload = reader.read_chunk(meta.ordinal)?;

        // Repopulate the cache so subsequent reads take the fast path. Use
        // `entry(...).or_insert(...)` instead of unconditional `insert` so we
        // don't overwrite a fresher reader that a concurrent thread (e.g. a
        // rollover or delete that materialized tombstone state) may have
        // installed between our read lock and this write lock.
        tenant
            .segments
            .write()
            .entry(meta.segment_id)
            .or_insert(reader);

        Ok(match payload {
            Some(bytes) => {
                let mut chunk: MemoryChunk = serde_json::from_slice(&bytes)
                    .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
                chunk.status = meta.status;
                Some(chunk)
            }
            None => None,
        })
    }

    pub(crate) async fn get_chunk_for_retrieval(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        operation: &'static str,
    ) -> Result<Option<MemoryChunk>> {
        let metadata = self.metadata.get(tenant_id, chunk_id)?;
        if matches!(
            metadata.as_ref().map(|row| row.status),
            None | Some(ChunkStatus::Candidate | ChunkStatus::Deleted | ChunkStatus::Error)
        ) {
            return Ok(None);
        }
        match self.get_chunk(tenant_id, chunk_id).await {
            Ok(chunk) => Ok(chunk),
            Err(MemdError::StorageError(error)) => {
                warn!(
                    tenant_id = %tenant_id,
                    chunk_id = %chunk_id,
                    operation,
                    error = %error,
                    "skipping unreadable chunk during retrieval"
                );
                Ok(None)
            }
            Err(MemdError::IoError(error)) => {
                warn!(
                    tenant_id = %tenant_id,
                    chunk_id = %chunk_id,
                    operation,
                    error = %error,
                    "skipping unreadable chunk during retrieval"
                );
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}
