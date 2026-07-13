use super::*;

impl SqliteMetadataStore {
    /// Apply a lifecycle delta and return the number of rows affected.
    ///
    /// Same semantics as `MetadataStore::update_lifecycle` but the
    /// concrete rowcount is exposed so callers can fail closed when
    /// the UPDATE matched zero rows (non-existent chunk_id OR
    /// cross-tenant access — the WHERE filter silently drops both
    /// otherwise). `memory.set_expiry` uses this to make
    /// `{"updated": true}` a load-bearing claim.
    pub fn update_lifecycle_counted(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<usize> {
        let conn = self.pool.get();
        // `status != 'deleted'` guard: once a row is tombstoned no
        // caller should be mutating its overlay. Before this guard,
        // `memory.set_expiry` could flip `expires_at_ms` on a deleted
        // row (and bump the cache) even though the row was invisible
        // to every retrieval surface. The guarded UPDATE matches zero
        // rows for deleted chunks, so the atomic
        // `update_lifecycle_if_exists` path returns `updated=false`
        // end-to-end.
        let rows = conn.execute(
            "UPDATE chunks SET
                status                  = COALESCE(:status, status),
                tier                    = COALESCE(:tier, tier),
                supersedes              = COALESCE(:supersedes, supersedes),
                superseded_by           = COALESCE(:superseded_by, superseded_by),
                expires_at_ms           = CASE WHEN :set_expires = 1 THEN :expires_at ELSE expires_at_ms END,
                review_after_ms         = CASE WHEN :set_review  = 1 THEN :review_at  ELSE review_after_ms END,
                lifecycle_updated_at_ms = COALESCE(:lifecycle_at, lifecycle_updated_at_ms)
             WHERE tenant_id = :tenant
               AND chunk_id = :chunk
               AND status != 'deleted'",
            rusqlite::named_params! {
                ":status":        delta.status.map(|s| s.to_string()),
                ":tier":          delta.tier.map(|t| t.to_string()),
                ":supersedes":    delta.supersedes.as_ref().map(|c| c.to_string()),
                ":superseded_by": delta.superseded_by.as_ref().map(|c| c.to_string()),
                ":set_expires":   i64::from(delta.expires_at_ms.is_some()),
                ":expires_at":    delta.expires_at_ms.flatten(),
                ":set_review":    i64::from(delta.review_after_ms.is_some()),
                ":review_at":     delta.review_after_ms.flatten(),
                ":lifecycle_at":  delta.lifecycle_updated_at_ms,
                ":tenant":        tenant_id.as_str(),
                ":chunk":         chunk_id.to_string(),
            },
        )?;
        Ok(rows)
    }

    pub(super) fn atomic_supersede_lifecycle(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_id: &ChunkId,
        now_ms: i64,
    ) -> Result<()> {
        let mut conn = self.pool.get();
        let tx = conn.transaction()?;

        // Both UPDATEs must touch exactly one row; otherwise one side of
        // the supersession edge would dangle. Returning an error here
        // drops the `Transaction` without commit, which rolls the
        // partial update back on SQLite's side.
        //
        // `superseded_by IS NULL` in the WHERE clause enforces head-only
        // semantics at the SQL layer: an old chunk that already points
        // to a successor won't match, rows=0 triggers the guard below,
        // and the transaction rolls back. Without this, a double-
        // supersede on the same old chunk would silently overwrite
        // `superseded_by` and fork the graph.
        let old_rows = tx.execute(
            "UPDATE chunks SET status = 'superseded', superseded_by = :new,
                lifecycle_updated_at_ms = :now
             WHERE tenant_id = :tenant AND chunk_id = :old
               AND superseded_by IS NULL",
            rusqlite::named_params! {
                ":new": new_id.to_string(),
                ":now": now_ms,
                ":tenant": tenant_id.as_str(),
                ":old": old_id.to_string(),
            },
        )?;
        if old_rows != 1 {
            return Err(crate::error::MemdError::ValidationError(format!(
                "atomic_supersede: old chunk {old_id} is not current head or missing in tenant {tenant_id} (rows={old_rows})"
            )));
        }

        let new_rows = tx.execute(
            "UPDATE chunks SET supersedes = :old, lifecycle_updated_at_ms = :now
             WHERE tenant_id = :tenant AND chunk_id = :new",
            rusqlite::named_params! {
                ":old": old_id.to_string(),
                ":now": now_ms,
                ":tenant": tenant_id.as_str(),
                ":new": new_id.to_string(),
            },
        )?;
        if new_rows != 1 {
            return Err(crate::error::MemdError::StorageError(format!(
                "atomic_supersede: new chunk {new_id} not found in tenant {tenant_id} (rows={new_rows})"
            )));
        }

        tx.commit()?;
        Ok(())
    }

    pub(super) fn list_expired_before_lifecycle(
        &self,
        tenant_id: &TenantId,
        now_ms: i64,
    ) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        // Use `<=` so this matches `VisibilityPolicy::is_visible_at`
        // (which hides any row with `expires_at_ms <= now_ms`); otherwise
        // a row expiring exactly at `now_ms` is hidden by retrieval but
        // never materialised to status=Expired by the sweep until the
        // next invocation, which breaks the sweep→promote pipeline.
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND expires_at_ms IS NOT NULL
               AND expires_at_ms <= ?2
               AND status NOT IN ('candidate', 'deleted', 'expired', 'superseded', 'error')",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), now_ms], |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub(super) fn mark_expired_if_final_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        now_ms: i64,
    ) -> Result<bool> {
        // Guarded UPDATE: only promote rows that are *still* eligible
        // right now. Two predicates together:
        //
        // 1. `status = 'final'` — any other status (deleted,
        //    superseded, expired, error) is treated as "raced out from
        //    under the sweep" and skipped so we never overwrite a
        //    newer lifecycle transition.
        // 2. `expires_at_ms IS NOT NULL AND expires_at_ms <= :now_ms` —
        //    re-checks the retention predicate at UPDATE time so a
        //    concurrent writer that cleared `expires_at_ms` or pushed
        //    it forward (while leaving status=final) does not get its
        //    row reclassified as Expired by a sweep that selected the
        //    row before that write landed.
        let conn = self.pool.get();
        let rows = conn.execute(
            "UPDATE chunks
                SET status = 'expired',
                    lifecycle_updated_at_ms = ?3
              WHERE tenant_id = ?1
                AND chunk_id = ?2
                AND status = 'final'
                AND expires_at_ms IS NOT NULL
                AND expires_at_ms <= ?3",
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string(), now_ms],
        )?;
        Ok(rows == 1)
    }

    pub(super) fn promote_to_history_if_stale_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        older_than_ms: i64,
        now_ms: i64,
    ) -> Result<bool> {
        // Guarded UPDATE: re-check status, tier, and the overlay-idle
        // clock at UPDATE time so a concurrent writer that refreshed
        // lifecycle_updated_at_ms or flipped the row back off
        // superseded/expired is not clobbered by a stale selection.
        let conn = self.pool.get();
        let rows = conn.execute(
            "UPDATE chunks
                SET tier = 'history',
                    lifecycle_updated_at_ms = ?4
              WHERE tenant_id = ?1
                AND chunk_id = ?2
                AND status IN ('superseded', 'expired')
                AND tier != 'history'
                AND lifecycle_updated_at_ms < ?3",
            rusqlite::params![
                tenant_id.as_str(),
                chunk_id.to_string(),
                older_than_ms,
                now_ms
            ],
        )?;
        Ok(rows == 1)
    }

    pub(super) fn list_stale_superseded_lifecycle(
        &self,
        tenant_id: &TenantId,
        older_than_ms: i64,
    ) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND status IN ('superseded', 'expired')
               AND tier != 'history'
               AND lifecycle_updated_at_ms < ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), older_than_ms],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub(super) fn list_lifecycle_hidden_impl(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        // Must mirror `VisibilityPolicy::is_visible` (types.rs). Any status
        // or tier that `is_visible` hides regardless of the include_*
        // flags — Candidate, Superseded, Expired, Error, plus
        // MemoryTier::History —
        // must be in this set so the HNSW rebuild excluded set matches
        // retrieval-side hiding. Missing a category means the rebuilt
        // HNSW carries graph weight for chunks that the handler will
        // never surface.
        //
        // `Deleted` is intentionally NOT here: it comes from
        // `get_deleted_chunk_ids` instead (tombstone path with its own
        // metrics), and the runner unions the two sets before passing
        // to `rebuild_hnsw_for_tenant`.
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND (status IN ('candidate', 'superseded', 'expired', 'error') OR tier = 'history')",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }
}
