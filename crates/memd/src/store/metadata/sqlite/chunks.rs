use super::*;

/// Canonical column list for `chunks` SELECT statements that feed
/// `row_to_metadata`.
///
/// Kept beside the positional row mapper so extending the table has one
/// module-level source of truth. Append new columns; do not reorder existing
/// columns.
const CHUNK_COLUMNS: &str = "chunk_id, tenant_id, project_id, segment_id, ordinal, \
                             chunk_type, status, timestamp_created, hash, source_uri, \
                             tier, supersedes, superseded_by, expires_at_ms, review_after_ms, \
                             lifecycle_updated_at_ms, canonical_text, ingestion_mode";

/// Rows whose keyword indexing is not complete. The literal `index_state !=
/// 'indexed'` must match the partial index `idx_chunks_unindexed`, so that the
/// query reads only the rows still waiting, not the whole tenant.
pub(crate) const LIST_UNINDEXED_SQL: &str = "SELECT chunk_id
     FROM chunks
     WHERE tenant_id = ?1
       AND index_state != 'indexed'
       AND status NOT IN ('candidate', 'deleted', 'error', 'superseded', 'expired')
       AND (?2 IS NULL OR project_id = ?2)
     ORDER BY timestamp_created DESC
     LIMIT ?3";

const HARD_PURGE_PREDICATE: &str = "WHERE tenant_id = :tenant
    AND (:project IS NULL OR project_id = :project)
    AND (
        (status = 'deleted' AND deleted_at_ms <= :cutoff)
        OR (status IN ('superseded', 'expired', 'error')
            AND COALESCE(NULLIF(lifecycle_updated_at_ms, 0), timestamp_created) <= :cutoff)
        OR (status != 'deleted' AND tier = 'history'
            AND expires_at_ms IS NOT NULL AND expires_at_ms <= :cutoff)
    )";

impl SqliteMetadataStore {
    /// Convert a database row to [`ChunkMetadata`].
    ///
    /// **Invariant:** the SELECT that produced `row` must project
    /// columns in the order defined by [`CHUNK_COLUMNS`]. Any mismatch
    /// will silently corrupt the parsed record.
    ///
    /// Fail-closed on unknown `status` / `tier` strings: A3 treats an
    /// unrecognized enum value as a schema-drift bug rather than
    /// silently defaulting, so bad rows surface as conversion errors
    /// instead of masquerading as `Final`/`LongTerm`.
    pub(super) fn row_to_metadata(row: &rusqlite::Row) -> rusqlite::Result<ChunkMetadata> {
        let chunk_id_str: String = row.get(0)?;
        let tenant_id_str: String = row.get(1)?;
        let project_id: Option<String> = row.get(2)?;
        let segment_id: i64 = row.get(3)?;
        let ordinal: i32 = row.get(4)?;
        let chunk_type_str: String = row.get(5)?;
        let status_str: String = row.get(6)?;
        let timestamp_created: i64 = row.get(7)?;
        let hash: String = row.get(8)?;
        let source_uri: Option<String> = row.get(9)?;
        let tier_str: String = row.get(10)?;
        let supersedes_str: Option<String> = row.get(11)?;
        let superseded_by_str: Option<String> = row.get(12)?;
        let expires_at_ms: Option<i64> = row.get(13)?;
        let review_after_ms: Option<i64> = row.get(14)?;
        let lifecycle_updated_at_ms: i64 = row.get(15)?;
        let canonical_text: Option<String> = row.get(16)?;
        let ingestion_mode_str: String = row.get(17)?;

        // Parse chunk_id
        let chunk_id = ChunkId::parse(&chunk_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse tenant_id
        let tenant_id = TenantId::new(&tenant_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse chunk_type — fail-closed via FromStr.
        let chunk_type = chunk_type_str.parse::<ChunkType>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse status — fail-closed via FromStr (A1).
        let status = status_str.parse::<ChunkStatus>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse tier — fail-closed via FromStr (A2).
        let tier = tier_str.parse::<MemoryTier>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let supersedes = supersedes_str
            .map(|s| ChunkId::parse(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    11,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let superseded_by = superseded_by_str
            .map(|s| ChunkId::parse(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

        let lifecycle = LifecycleMetadata {
            tier,
            supersedes,
            superseded_by,
            expires_at_ms,
            review_after_ms,
            lifecycle_updated_at_ms,
        };

        // Parse ingestion_mode — fail-closed via FromStr (E1).
        let ingestion_mode = ingestion_mode_str
            .parse::<crate::types::IngestionMode>()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    17,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

        Ok(ChunkMetadata {
            chunk_id,
            tenant_id,
            project_id,
            segment_id: segment_id as u64,
            ordinal: ordinal as u32,
            chunk_type,
            status,
            timestamp_created,
            hash,
            source_uri,
            lifecycle,
            canonical_text,
            ingestion_mode,
        })
    }

    /// Back-date a chunk's `timestamp_created` via direct SQL.
    ///
    /// TEST-ONLY: gives unit tests a deterministic clock when they exercise
    /// age-based behavior.
    #[cfg(test)]
    pub(crate) fn force_timestamp_created(&self, chunk_id: &ChunkId, ts_ms: i64) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "UPDATE chunks SET timestamp_created = ?1 WHERE chunk_id = ?2",
            rusqlite::params![ts_ms, chunk_id.to_string()],
        )?;
        Ok(())
    }

    /// List lifecycle-hidden rows old enough for destructive purge.
    ///
    /// This intentionally includes already-deleted rows because those
    /// rows have already gone through the soft-delete/tombstone path
    /// and only metadata/link cleanup remains. History-tier rows are
    /// eligible only after their explicit expiry has elapsed; a final
    /// history row with no retention deadline is not purged here.
    pub fn list_hard_purge_candidates(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        cutoff_ms: i64,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks {HARD_PURGE_PREDICATE}
             ORDER BY COALESCE(NULLIF(lifecycle_updated_at_ms, 0), timestamp_created) ASC
             LIMIT :limit"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":cutoff": cutoff_ms,
                ":limit": limit as i64,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Count every hidden row eligible for purge, without applying a page limit.
    pub fn count_hard_purge_candidates(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        cutoff_ms: i64,
    ) -> Result<usize> {
        let conn = self.pool.get();
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM chunks {HARD_PURGE_PREDICATE}"),
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":cutoff": cutoff_ms,
            },
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Physically remove metadata/link rows for chunks that have already
    /// passed through the soft-delete path.
    ///
    /// The status guard prevents this method from orphaning live rows.
    /// Callers that start with superseded/expired/history candidates
    /// must first route them through `PersistentStore::delete_chunk`
    /// so WAL, tombstone, sparse/hybrid index, and cache state are
    /// updated before metadata disappears.
    pub fn hard_delete_chunks(&self, tenant_id: &TenantId, chunk_ids: &[ChunkId]) -> Result<usize> {
        if chunk_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        let mut deleted = 0usize;
        {
            let mut feedback_stmt =
                tx.prepare("DELETE FROM feedback WHERE tenant_id = ?1 AND chunk_id = ?2")?;
            let mut link_stmt = tx.prepare("DELETE FROM artifact_links WHERE chunk_id = ?1")?;
            let mut chunk_stmt = tx.prepare(
                "DELETE FROM chunks
                 WHERE tenant_id = ?1 AND chunk_id = ?2 AND status = 'deleted'",
            )?;
            for chunk_id in chunk_ids {
                let chunk_id_str = chunk_id.to_string();
                feedback_stmt.execute(rusqlite::params![tenant_id.as_str(), chunk_id_str])?;
                link_stmt.execute(rusqlite::params![chunk_id.to_string()])?;
                deleted += chunk_stmt
                    .execute(rusqlite::params![tenant_id.as_str(), chunk_id.to_string()])?;
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// Repair recovered segment coordinates and invalidate stale index state atomically.
    pub fn repair_recovered_chunk_location_pending(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        segment_id: u64,
        ordinal: u32,
        now_ms: i64,
    ) -> Result<usize> {
        let conn = self.pool.get();
        Ok(conn.execute(
            "UPDATE chunks
             SET segment_id = ?1, ordinal = ?2,
                 index_state = 'pending', index_attempts = 0,
                 index_last_error = NULL, indexed_at_ms = NULL,
                 index_updated_at_ms = ?3
             WHERE tenant_id = ?4 AND chunk_id = ?5 AND status != 'deleted'",
            rusqlite::params![
                segment_id as i64,
                ordinal as i64,
                now_ms,
                tenant_id.as_str(),
                chunk_id.to_string(),
            ],
        )?)
    }

    /// Move live metadata rows to rewritten segment coordinates.
    ///
    /// Segment rewrite creates new immutable segment directories rather
    /// than editing old payload files in place. This transaction flips
    /// affected rows to their new `(segment_id, ordinal)` pairs after
    /// the replacement segment has been finalized on disk.
    pub fn update_chunk_locations(
        &self,
        tenant_id: &TenantId,
        relocations: &[(ChunkId, u64, u32)],
    ) -> Result<usize> {
        if relocations.is_empty() {
            return Ok(0);
        }

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        let mut updated = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE chunks
                 SET segment_id = ?1, ordinal = ?2
                 WHERE tenant_id = ?3 AND chunk_id = ?4 AND status != 'deleted'",
            )?;
            for (chunk_id, segment_id, ordinal) in relocations {
                updated += stmt.execute(rusqlite::params![
                    *segment_id as i64,
                    *ordinal as i64,
                    tenant_id.as_str(),
                    chunk_id.to_string(),
                ])?;
            }
        }
        if updated != relocations.len() {
            return Err(MemdError::StorageError(format!(
                "segment rewrite location update matched {} of {} rows",
                updated,
                relocations.len()
            )));
        }
        tx.commit()?;
        Ok(updated)
    }

    pub(crate) fn max_segment_id(&self, tenant_id: &TenantId) -> Result<u64> {
        let conn = self.pool.get();
        let id: i64 = conn.query_row(
            "SELECT COALESCE(MAX(segment_id), 0) FROM chunks WHERE tenant_id = ?1",
            [tenant_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(id as u64)
    }
}

pub(super) fn lengths_for_scope(
    conn: &Connection,
    table: &str,
    column: &str,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<usize>> {
    let mut sql = format!(
        "SELECT LENGTH({column}) FROM {table}
         WHERE tenant_id = ?1 AND {column} IS NOT NULL"
    );
    let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
    if let Some(project_id) = project_id {
        sql.push_str(" AND project_id = ?2");
        params.push(rusqlite::types::Value::Text(project_id.to_string()));
    }
    sql.push_str(&format!(" ORDER BY LENGTH({column}) ASC"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok(row.get::<usize, i64>(0)? as usize)
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub(super) fn percentile_usize(sorted: &[usize], p: usize) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (p * (sorted.len() - 1) / 100).min(sorted.len() - 1);
    sorted[idx]
}

pub(super) fn compact_text_preview(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = 0;
    for (idx, _) in text.char_indices() {
        if idx > max_chars {
            break;
        }
        end = idx;
    }
    text[..end].trim_end().to_string()
}

impl SqliteMetadataStore {
    /// Recover metadata with pending indexing in the same SQLite transaction.
    pub fn insert_pending_recovered(&self, metadata: &ChunkMetadata) -> Result<()> {
        self.insert_many_with_index_state(std::slice::from_ref(metadata), true)
    }

    fn insert_many_with_index_state(
        &self,
        metadata: &[ChunkMetadata],
        pending: bool,
    ) -> Result<()> {
        self.insert_with_supersession(metadata, pending, &[])
    }

    /// Commit an import's metadata, pending index state and graph together.
    pub(crate) fn insert_with_supersession(
        &self,
        metadata: &[ChunkMetadata],
        pending: bool,
        edges: &[(TenantId, ChunkId, ChunkId)],
    ) -> Result<()> {
        if metadata.is_empty() && edges.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO chunks (
                    chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri,
                    tier, supersedes, superseded_by, expires_at_ms, review_after_ms,
                    lifecycle_updated_at_ms, canonical_text, ingestion_mode, index_state
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            )?;
            for row in metadata {
                stmt.execute(rusqlite::params![
                    row.chunk_id.to_string(),
                    row.tenant_id.as_str(),
                    row.project_id.as_deref(),
                    row.segment_id as i64,
                    row.ordinal as i32,
                    row.chunk_type.to_string(),
                    row.status.to_string(),
                    row.timestamp_created,
                    &row.hash,
                    row.source_uri.as_deref(),
                    row.lifecycle.tier.to_string(),
                    row.lifecycle.supersedes.as_ref().map(|c| c.to_string()),
                    row.lifecycle.superseded_by.as_ref().map(|c| c.to_string()),
                    row.lifecycle.expires_at_ms,
                    row.lifecycle.review_after_ms,
                    row.lifecycle.lifecycle_updated_at_ms,
                    row.canonical_text.as_deref(),
                    row.ingestion_mode.to_string(),
                    if pending { "pending" } else { "indexed" },
                ])?;
            }
        }

        for (tenant, old_id, new_id) in edges {
            let conflicts: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chunks WHERE tenant_id = ?1 AND chunk_id = ?2
                    AND supersedes IS NOT NULL AND supersedes != ?3)",
                rusqlite::params![tenant.as_str(), new_id.to_string(), old_id.to_string()],
                |row| row.get(0),
            )?;
            if conflicts {
                return Err(MemdError::ValidationError(format!(
                    "import target {new_id} already supersedes a different chunk"
                )));
            }
            // Exact replay of an already imported edge is a no-op. A different
            // successor still goes through the head-only guard and rolls back.
            let linked: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chunks old JOIN chunks new
                    ON new.tenant_id = old.tenant_id
                    WHERE old.tenant_id = ?1 AND old.chunk_id = ?2 AND new.chunk_id = ?3
                    AND old.superseded_by = ?3 AND new.supersedes = ?2
                    AND old.status != 'deleted' AND new.status != 'deleted')",
                rusqlite::params![tenant.as_str(), old_id.to_string(), new_id.to_string()],
                |row| row.get(0),
            )?;
            if !linked {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                Self::supersede_on_connection(&tx, tenant, old_id, new_id, now_ms)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

impl MetadataStore for SqliteMetadataStore {
    fn insert(&self, metadata: &ChunkMetadata) -> Result<()> {
        self.insert_many(std::slice::from_ref(metadata))
    }

    fn insert_many(&self, metadata: &[ChunkMetadata]) -> Result<()> {
        self.insert_many_with_index_state(metadata, false)
    }

    fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<ChunkMetadata>> {
        let conn = self.pool.get();

        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'"
        );
        let mut stmt = conn.prepare(&sql)?;

        let result = stmt.query_row(
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string()],
            Self::row_to_metadata,
        );

        match result {
            Ok(metadata) => Ok(Some(metadata)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();

        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = ?1 AND status NOT IN ('candidate', 'deleted')
             ORDER BY timestamp_created DESC
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = conn.prepare(&sql)?;

        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), limit as i64, offset as i64],
            Self::row_to_metadata,
        )?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn list_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = :tenant
               AND status NOT IN ('candidate', 'deleted')
               AND (:project IS NULL OR project_id = :project)
             ORDER BY timestamp_created DESC, chunk_id ASC
             LIMIT :limit OFFSET :offset"
        );
        let mut stmt = conn.prepare(&sql)?;

        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":limit": limit as i64,
                ":offset": offset as i64,
            },
            Self::row_to_metadata,
        )?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn mark_deleted(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        let conn = self.pool.get();

        let rows_affected = conn.execute(
            "UPDATE chunks SET status = 'deleted',
                 deleted_at_ms = CAST(ROUND(unixepoch('subsec') * 1000) AS INTEGER),
                 lifecycle_updated_at_ms = CAST(ROUND(unixepoch('subsec') * 1000) AS INTEGER)
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'",
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string()],
        )?;

        Ok(rows_affected > 0)
    }

    fn get_by_segment(&self, tenant_id: &TenantId, segment_id: u64) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();

        // Tenant-scoped because (segment_id, ordinal) is no longer
        // globally unique after Item 2's UNIQUE constraint migration.
        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = ?1 AND segment_id = ?2
             ORDER BY ordinal ASC"
        );
        let mut stmt = conn.prepare(&sql)?;

        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), segment_id as i64],
            Self::row_to_metadata,
        )?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn count_by_status(&self, tenant_id: &TenantId) -> Result<(usize, usize, usize)> {
        let conn = self.pool.get();

        let mut stmt = conn.prepare(
            "SELECT status, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1
             GROUP BY status",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count as usize))
        })?;

        let mut active = 0usize;
        let mut deleted = 0usize;
        let mut candidates = 0usize;

        for row in rows {
            let (status, count) = row?;
            if status == "deleted" {
                deleted = count;
            } else if status == "candidate" {
                candidates = count;
            } else {
                active += count;
            }
        }

        Ok((active, deleted, candidates))
    }

    /// Count chunk types without paging through `list`.
    ///
    /// Returns `(active, deleted, all)` maps. "Active" here means every
    /// non-deleted row; lifecycle-hidden rows such as superseded, expired,
    /// or history-tier chunks remain counted as active storage rows.
    fn count_chunk_types_by_status(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
    ) -> Result<(
        HashMap<String, usize>,
        HashMap<String, usize>,
        HashMap<String, usize>,
    )> {
        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT chunk_type, status, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1",
        );
        let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            sql.push_str(" AND project_id = ?2");
            params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        sql.push_str(" GROUP BY chunk_type, status");

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<usize, String>(0)?,
                row.get::<usize, String>(1)?,
                row.get::<usize, i64>(2)? as usize,
            ))
        })?;

        let mut active = HashMap::new();
        let mut deleted = HashMap::new();
        let mut all = HashMap::new();
        for row in rows {
            let (chunk_type, status, count) = row?;
            *all.entry(chunk_type.clone()).or_insert(0) += count;
            if status == "deleted" {
                *deleted.entry(chunk_type).or_insert(0) += count;
            } else if status != "candidate" {
                *active.entry(chunk_type).or_insert(0) += count;
            }
        }

        Ok((active, deleted, all))
    }

    fn health_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        duplicate_limit: usize,
    ) -> Result<StoreHealthSnapshot> {
        let (chunk_types_active, _chunk_types_deleted, chunk_types_all) =
            self.count_chunk_types_by_status(tenant_id, project_id)?;
        let conn = self.pool.get();

        let mut status_sql = String::from(
            "SELECT status, tier, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1",
        );
        let mut status_params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            status_sql.push_str(" AND project_id = ?2");
            status_params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        status_sql.push_str(" GROUP BY status, tier");
        let mut stmt = conn.prepare(&status_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(status_params), |row| {
            Ok((
                row.get::<usize, String>(0)?,
                row.get::<usize, String>(1)?,
                row.get::<usize, i64>(2)? as usize,
            ))
        })?;

        let mut counts = HealthCounts::default();
        for row in rows {
            let (status, tier, count) = row?;
            counts.total_chunks += count;
            if status == "candidate" {
                counts.candidate_chunks += count;
            } else if status == "deleted" {
                counts.deleted_chunks += count;
            } else {
                counts.active_chunks += count;
            }
            if status == "expired" {
                counts.expired_chunks += count;
            }
            if status == "superseded" {
                counts.superseded_chunks += count;
            }
            if tier == "history" {
                counts.history_chunks += count;
            }
        }

        let mut index_sql = String::from(
            "SELECT index_state, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1 AND status NOT IN ('candidate', 'deleted')",
        );
        let mut index_params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            index_sql.push_str(" AND project_id = ?2");
            index_params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        index_sql.push_str(" GROUP BY index_state");
        let mut stmt = conn.prepare(&index_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(index_params), |row| {
            Ok((
                row.get::<usize, String>(0)?,
                row.get::<usize, i64>(1)? as usize,
            ))
        })?;
        let mut index_coverage = IndexCoverageHealth::default();
        for row in rows {
            let (state, count) = row?;
            match state.as_str() {
                "pending" => index_coverage.pending = count,
                "indexed" => index_coverage.indexed = count,
                "failed" => index_coverage.failed = count,
                _ => {}
            }
        }
        let index_total = index_coverage.pending + index_coverage.indexed + index_coverage.failed;
        if index_total > 0 {
            index_coverage.indexed_percentage =
                (index_coverage.indexed as f64 / index_total as f64) * 100.0;
        }

        let mut visible_count_sql = String::from(
            "SELECT COUNT(*)
             FROM chunks
             WHERE tenant_id = ?1
               AND status NOT IN ('candidate', 'deleted', 'superseded', 'expired', 'error')
               AND tier != 'history'",
        );
        let mut visible_count_params =
            vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            visible_count_sql.push_str(" AND project_id = ?2");
            visible_count_params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        let visible_chunks: usize = conn.query_row(
            &visible_count_sql,
            rusqlite::params_from_iter(visible_count_params),
            |row| Ok(row.get::<usize, i64>(0)? as usize),
        )?;

        let mut dup_summary_sql = String::from(
            "SELECT COUNT(DISTINCT canonical_text),
                    COUNT(canonical_text),
                    COALESCE(SUM(LENGTH(canonical_text)), 0)
             FROM chunks
             WHERE tenant_id = ?1
               AND status NOT IN ('candidate', 'deleted', 'superseded', 'expired', 'error')
               AND tier != 'history'
               AND canonical_text IS NOT NULL",
        );
        let mut dup_params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            dup_summary_sql.push_str(" AND project_id = ?2");
            dup_params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        let (unique_text_count, _text_rows, total_text_bytes): (usize, usize, usize) = conn
            .query_row(
                &dup_summary_sql,
                rusqlite::params_from_iter(dup_params),
                |row| {
                    Ok((
                        row.get::<usize, i64>(0)? as usize,
                        row.get::<usize, i64>(1)? as usize,
                        row.get::<usize, i64>(2)? as usize,
                    ))
                },
            )?;

        let mut dup_aggregate_sql = String::from(
            "SELECT COUNT(*),
                    COALESCE(SUM(cnt - 1), 0),
                    COALESCE(SUM((cnt - 1) * bytes), 0)
             FROM (
                 SELECT COUNT(*) as cnt, LENGTH(canonical_text) as bytes
                 FROM chunks
                 WHERE tenant_id = ?1
                   AND status NOT IN ('candidate', 'deleted', 'superseded', 'expired', 'error')
                   AND tier != 'history'
                   AND canonical_text IS NOT NULL",
        );
        let mut dup_aggregate_params =
            vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            dup_aggregate_sql.push_str(" AND project_id = ?2");
            dup_aggregate_params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        dup_aggregate_sql.push_str(
            " GROUP BY canonical_text
              HAVING COUNT(*) > 1
             )",
        );
        let (duplicate_group_count, duplicate_row_count, duplicate_byte_count): (
            usize,
            usize,
            usize,
        ) = conn.query_row(
            &dup_aggregate_sql,
            rusqlite::params_from_iter(dup_aggregate_params),
            |row| {
                Ok((
                    row.get::<usize, i64>(0)? as usize,
                    row.get::<usize, i64>(1)? as usize,
                    row.get::<usize, i64>(2)? as usize,
                ))
            },
        )?;

        let mut duplicates = DuplicateHealth {
            unique_text_count,
            exact_duplicate_group_count: duplicate_group_count,
            duplicate_row_count,
            ..Default::default()
        };

        if visible_chunks > 0 {
            duplicates.duplicate_row_ratio =
                duplicates.duplicate_row_count as f64 / visible_chunks as f64;
        }
        if total_text_bytes > 0 {
            duplicates.duplicate_byte_ratio = duplicate_byte_count as f64 / total_text_bytes as f64;
        }

        if duplicate_limit > 0 {
            let mut dup_groups_sql = String::from(
                "SELECT canonical_text, COUNT(*) as cnt, LENGTH(canonical_text) as bytes
             FROM chunks
             WHERE tenant_id = ?1
               AND status NOT IN ('candidate', 'deleted', 'superseded', 'expired', 'error')
               AND tier != 'history'
               AND canonical_text IS NOT NULL",
            );
            let mut dup_group_params =
                vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
            if let Some(project_id) = project_id {
                dup_groups_sql.push_str(" AND project_id = ?2");
                dup_group_params.push(rusqlite::types::Value::Text(project_id.to_string()));
            }
            dup_groups_sql.push_str(
                " GROUP BY canonical_text
              HAVING COUNT(*) > 1
              ORDER BY COUNT(*) DESC, LENGTH(canonical_text) DESC",
            );
            dup_groups_sql.push_str(&format!(" LIMIT {}", duplicate_limit));

            let mut stmt = conn.prepare(&dup_groups_sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(dup_group_params), |row| {
                Ok((
                    row.get::<usize, String>(0)?,
                    row.get::<usize, i64>(1)? as usize,
                    row.get::<usize, i64>(2)? as usize,
                ))
            })?;
            for row in rows {
                let (canonical_text, count, bytes) = row?;
                let preview = compact_text_preview(&canonical_text, 160);
                duplicates.examples.push(DuplicateExample {
                    canonical_text_preview: preview,
                    count,
                    byte_count: count.saturating_mul(bytes),
                });
            }
        }

        let canonical_lengths =
            lengths_for_scope(&conn, "chunks", "canonical_text", tenant_id, project_id)?;
        let artifact_lengths = lengths_for_scope(
            &conn,
            "task_artifacts",
            "canonical_json",
            tenant_id,
            project_id,
        )?;
        let payload = PayloadHealth {
            p50_canonical_text_bytes: percentile_usize(&canonical_lengths, 50),
            p95_canonical_text_bytes: percentile_usize(&canonical_lengths, 95),
            max_canonical_text_bytes: canonical_lengths.last().copied().unwrap_or(0),
            p95_artifact_json_bytes: percentile_usize(&artifact_lengths, 95),
        };

        Ok(StoreHealthSnapshot {
            counts,
            chunk_types_active,
            chunk_types_all,
            duplicates,
            index_coverage,
            payload,
        })
    }

    fn get_deleted_chunk_ids(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();

        let mut stmt = conn
            .prepare("SELECT chunk_id FROM chunks WHERE tenant_id = ?1 AND status = 'deleted'")?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let chunk_id_str: String = row.get(0)?;
            Ok(chunk_id_str)
        })?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            let chunk_id_str = row?;
            if let Ok(chunk_id) = ChunkId::parse(&chunk_id_str) {
                chunk_ids.push(chunk_id);
            }
        }

        Ok(chunk_ids)
    }

    fn mark_index_pending(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
        now_ms: i64,
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE chunks
                 SET index_state = ?3,
                     index_attempts = 0,
                     index_last_error = NULL,
                     indexed_at_ms = NULL,
                     index_updated_at_ms = ?4
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
            )?;
            for chunk_id in chunk_ids {
                stmt.execute(rusqlite::params![
                    tenant_id.as_str(),
                    chunk_id.to_string(),
                    IndexState::Pending.as_str(),
                    now_ms,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_indexed(&self, tenant_id: &TenantId, chunk_ids: &[ChunkId], now_ms: i64) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE chunks
                 SET index_state = ?3,
                     index_last_error = NULL,
                     indexed_at_ms = ?4,
                     index_updated_at_ms = ?4
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
            )?;
            for chunk_id in chunk_ids {
                stmt.execute(rusqlite::params![
                    tenant_id.as_str(),
                    chunk_id.to_string(),
                    IndexState::Indexed.as_str(),
                    now_ms,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_index_failed(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        error: &str,
        now_ms: i64,
    ) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "UPDATE chunks
             SET index_state = ?3,
                 index_attempts = index_attempts + 1,
                 index_last_error = ?4,
                 index_updated_at_ms = ?5
             WHERE tenant_id = ?1 AND chunk_id = ?2",
            rusqlite::params![
                tenant_id.as_str(),
                chunk_id.to_string(),
                IndexState::Failed.as_str(),
                error,
                now_ms,
            ],
        )?;
        Ok(())
    }

    fn list_unindexed_chunk_ids(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut stmt = conn.prepare_cached(LIST_UNINDEXED_SQL)?;
        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), project_id, limit as i64],
            |row| row.get::<usize, String>(0),
        )?;
        let mut chunk_ids = Vec::new();
        for row in rows {
            if let Ok(chunk_id) = ChunkId::parse(&row?) {
                chunk_ids.push(chunk_id);
            }
        }
        Ok(chunk_ids)
    }

    fn list_pending_index_chunk_ids(
        &self,
        tenant_id: &TenantId,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT chunk_id
             FROM chunks
             WHERE tenant_id = ?1
               AND status NOT IN ('candidate', 'deleted', 'error')
               AND index_state = ?2
             ORDER BY timestamp_created ASC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![
                tenant_id.as_str(),
                IndexState::Pending.as_str(),
                limit as i64
            ],
            |row| row.get::<usize, String>(0),
        )?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            if let Ok(chunk_id) = ChunkId::parse(&row?) {
                chunk_ids.push(chunk_id);
            }
        }
        Ok(chunk_ids)
    }

    fn count_by_index_state(&self, tenant_id: &TenantId) -> Result<(usize, usize, usize)> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT index_state, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1 AND status NOT IN ('candidate', 'deleted', 'error')
             GROUP BY index_state",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let state: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((state, count as usize))
        })?;

        let mut pending = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        for row in rows {
            let (state, count) = row?;
            if state == IndexState::Pending.as_str() {
                pending = count;
            } else if state == IndexState::Indexed.as_str() {
                indexed = count;
            } else if state == IndexState::Failed.as_str() {
                failed = count;
            }
        }
        Ok((pending, indexed, failed))
    }

    fn update_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<()> {
        // Forward to the counted variant and drop the rowcount so
        // existing callers keep their Result<()> signature. Callers
        // that need the rowcount (e.g. memory.set_expiry's
        // atomic "did the row actually exist?" check) use
        // `update_lifecycle_counted` directly on the concrete store.
        let _ = self.update_lifecycle_counted(tenant_id, chunk_id, delta)?;
        Ok(())
    }

    fn atomic_supersede(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_id: &ChunkId,
        now_ms: i64,
    ) -> Result<()> {
        self.atomic_supersede_lifecycle(tenant_id, old_id, new_id, now_ms)
    }

    fn list_expired_before(&self, tenant_id: &TenantId, now_ms: i64) -> Result<Vec<ChunkId>> {
        self.list_expired_before_lifecycle(tenant_id, now_ms)
    }

    fn mark_expired_if_final(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        now_ms: i64,
    ) -> Result<bool> {
        self.mark_expired_if_final_lifecycle(tenant_id, chunk_id, now_ms)
    }

    fn promote_to_history_if_stale(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        older_than_ms: i64,
        now_ms: i64,
    ) -> Result<bool> {
        self.promote_to_history_if_stale_lifecycle(tenant_id, chunk_id, older_than_ms, now_ms)
    }

    fn list_stale_superseded(
        &self,
        tenant_id: &TenantId,
        older_than_ms: i64,
    ) -> Result<Vec<ChunkId>> {
        self.list_stale_superseded_lifecycle(tenant_id, older_than_ms)
    }

    fn list_lifecycle_hidden(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>> {
        self.list_lifecycle_hidden_impl(tenant_id)
    }

    fn list_by_canonical_text(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        canonical: &str,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
               AND canonical_text = :canonical"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":canonical": canonical,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_live_by_content_hash(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        chunk_type: ChunkType,
        hash: &str,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND ((:project IS NULL AND project_id IS NULL) OR project_id = :project)
               AND chunk_type = :chunk_type
               AND hash = :hash
               AND status = 'final'
               AND tier != 'history'
               AND superseded_by IS NULL
             ORDER BY timestamp_created DESC
             LIMIT :limit"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":chunk_type": chunk_type.to_string(),
                ":hash": hash,
                ":limit": limit as i64,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_recent_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
             ORDER BY timestamp_created DESC
             LIMIT :limit"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":limit": limit as i64,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_recent_with_null_project(
        &self,
        tenant_id: &TenantId,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND project_id IS NULL
             ORDER BY timestamp_created DESC
             LIMIT :limit"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":limit": limit as i64,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_for_export(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        include_history: bool,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
               AND (:include_history = 1 OR tier != 'history')
               AND status NOT IN ('candidate', 'deleted', 'error')
             ORDER BY timestamp_created ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":include_history": i64::from(include_history),
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_digest_rows(
        &self,
        tenant_id: &TenantId,
        rows: DigestRows<'_>,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        let scope = digest_scope_clause(rows, &mut params);
        params.push(rusqlite::types::Value::Integer(limit as i64));
        let limit_index = params.len();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = ?1
               AND status NOT IN {NOT_VISIBLE_STATUSES}
               AND {scope}
               AND NOT {EXPERIENCE_PROJECTION}
               AND NOT {GENERATED_DIGEST_PROJECTION}
             ORDER BY timestamp_created DESC, chunk_id ASC
             LIMIT ?{limit_index}"
        );
        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter()),
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn count_digest_rows(
        &self,
        tenant_id: &TenantId,
        scope: &DigestScope,
    ) -> Result<DigestScopeCounts> {
        let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        let in_scope = digest_scope_clause(DigestRows::InScope(scope), &mut params);
        // One scan: classify each row once (the CASE stops at the first match,
        // so the projection lookups run only for rows with a visible status),
        // then group by scope side.
        let sql = format!(
            "SELECT in_scope,
                    COALESCE(SUM(kind = 'status'), 0),
                    COALESCE(SUM(kind = 'experience'), 0),
                    COALESCE(SUM(kind = 'generated'), 0),
                    COALESCE(SUM(kind = 'visible'), 0)
             FROM (
                 SELECT {in_scope} AS in_scope,
                        CASE WHEN status IN {NOT_VISIBLE_STATUSES} THEN 'status'
                             WHEN {EXPERIENCE_PROJECTION} THEN 'experience'
                             WHEN {GENERATED_DIGEST_PROJECTION} THEN 'generated'
                             ELSE 'visible' END AS kind
                 FROM chunks
                 WHERE tenant_id = ?1
             )
             GROUP BY in_scope"
        );
        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                DigestCounts {
                    not_visible: row.get::<_, i64>(1)? as usize,
                    experience_projections: row.get::<_, i64>(2)? as usize,
                    generated_digests: row.get::<_, i64>(3)? as usize,
                    visible: row.get::<_, i64>(4)? as usize,
                },
            ))
        })?;
        let mut counts = DigestScopeCounts::default();
        for row in rows {
            let (in_scope, side_counts) = row?;
            if in_scope == 1 {
                counts.inside = side_counts;
            } else {
                counts.outside = side_counts;
            }
        }
        Ok(counts)
    }
}

/// Statuses the digest never shows.
const NOT_VISIBLE_STATUSES: &str = "('candidate', 'deleted', 'error', 'superseded', 'expired')";

/// A row that retrieval-projects an experience case (problem, attempt, check,
/// lesson or correction). The lookup runs on `idx_artifact_links_chunk` and
/// the `task_artifacts` primary key.
const EXPERIENCE_PROJECTION: &str = "EXISTS (
    SELECT 1
    FROM artifact_links l
    JOIN task_artifacts a ON a.artifact_id = l.artifact_id
    WHERE l.chunk_id = chunks.chunk_id
      AND l.link_kind IN ('retrieval_projection', 'retrieval_projection_part')
      AND substr(a.artifact_role, 1, 11) = 'experience_'
)";

/// A row that retrieval-projects a generated digest (task resume, failure or
/// decision library, project brief, highlight library). This is the SQL form
/// of `is_generated_digest_takeaway`: `base_projection_tags` writes
/// `task:status:<status>`, `task:role:<role>` and `task:digest:<key>` from
/// these columns, and the digest writers store the status as exactly
/// `generated`. The tag rule sanitizes the status (`Generated!` also yields
/// `generated`), the SQL compares the raw value; the two differ only for such
/// non-canonical statuses.
const GENERATED_DIGEST_PROJECTION: &str = "EXISTS (
    SELECT 1
    FROM artifact_links l
    JOIN task_artifacts a ON a.artifact_id = l.artifact_id
    WHERE l.chunk_id = chunks.chunk_id
      AND l.link_kind IN ('retrieval_projection', 'retrieval_projection_part')
      AND a.status = 'generated'
      AND (COALESCE(trim(a.artifact_role), '') <> ''
           OR COALESCE(trim(a.digest_key), '') <> '')
)";

/// SQL condition for the project scope of one digest selection. Bound
/// project ids go to `params`. The condition is never NULL, so `NOT` is safe.
fn digest_scope_clause(rows: DigestRows<'_>, params: &mut Vec<rusqlite::types::Value>) -> String {
    let (scope, outside) = match rows {
        DigestRows::InScope(scope) => (scope, false),
        DigestRows::OutOfScope(scope) => (scope, true),
    };
    let mut parts = Vec::new();
    let project_ids = scope.project_ids.iter().collect::<BTreeSet<_>>();
    if !project_ids.is_empty() {
        let placeholders = project_ids
            .into_iter()
            .map(|project_id| {
                params.push(rusqlite::types::Value::Text(project_id.clone()));
                format!("?{}", params.len())
            })
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "(project_id IS NOT NULL AND project_id IN ({placeholders}))"
        ));
    }
    if scope.include_unscoped {
        parts.push("project_id IS NULL".to_string());
    }
    let inside = if parts.is_empty() {
        "0".to_string()
    } else {
        format!("({})", parts.join(" OR "))
    };
    if outside {
        format!("(NOT {inside})")
    } else {
        inside
    }
}
