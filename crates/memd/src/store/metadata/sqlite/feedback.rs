use super::*;

fn relevance_to_int(relevance: RelevanceLabel) -> i64 {
    match relevance {
        RelevanceLabel::Relevant => 1,
        RelevanceLabel::Irrelevant => -1,
    }
}

fn int_to_relevance(value: i64) -> RelevanceLabel {
    if value < 0 {
        RelevanceLabel::Irrelevant
    } else {
        RelevanceLabel::Relevant
    }
}

impl SqliteMetadataStore {
    /// Insert one feedback event.
    pub fn insert_feedback(&self, feedback: &FeedbackEntry) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "INSERT INTO feedback (
                 tenant_id, project_id, query_hash, chunk_id, relevance, timestamp_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                feedback.tenant_id.as_str(),
                feedback.project_id.as_deref(),
                &feedback.query_hash,
                feedback.chunk_id.to_string(),
                relevance_to_int(feedback.relevance),
                feedback.timestamp_ms,
            ],
        )?;
        Ok(())
    }

    /// Fetch feedback events for one tenant/query.
    pub fn list_feedback_for_query(
        &self,
        tenant_id: &TenantId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        let normalized = normalize_query(query);
        if normalized.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let query_hash = crate::store::stable_query_hash(&normalized);

        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT project_id, query_hash, chunk_id, relevance, timestamp_ms
             FROM feedback
             WHERE tenant_id = ?1 AND query_hash = ?2
             ORDER BY timestamp_ms DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), query_hash, limit as i64],
            |row| {
                let project_id: Option<String> = row.get(0)?;
                let query_hash: String = row.get(1)?;
                let chunk_id: String = row.get(2)?;
                let relevance: i64 = row.get(3)?;
                let timestamp_ms: i64 = row.get(4)?;
                Ok((project_id, query_hash, chunk_id, relevance, timestamp_ms))
            },
        )?;

        let mut feedback = Vec::new();
        for row in rows {
            let (project_id, query_hash, chunk_id_str, relevance_raw, timestamp_ms) = row?;
            let Ok(chunk_id) = ChunkId::parse(&chunk_id_str) else {
                continue;
            };
            let relevance = int_to_relevance(relevance_raw);
            feedback.push(FeedbackEntry {
                tenant_id: tenant_id.clone(),
                project_id,
                query_hash,
                chunk_id,
                relevance,
                timestamp_ms,
            });
        }
        Ok(feedback)
    }

    /// Insert one usage ledger event.
    pub fn insert_usage_event(&self, ts_unix_ms: i64, event: &UsageEvent) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "INSERT INTO usage_events (
                ts_unix_ms, op, tenant, project, outcome, chunk_count, bytes, detail
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                ts_unix_ms,
                event.op.as_str(),
                event.tenant.as_deref(),
                event.project.as_deref(),
                event.outcome.as_str(),
                event.chunk_count,
                event.bytes,
                event.detail.as_deref(),
            ],
        )?;
        Ok(())
    }

    pub fn usage_events_since(
        &self,
        since_ms: i64,
        tenant: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<UsageEventRecord>> {
        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT ts_unix_ms, op, outcome, chunk_count, bytes, detail
             FROM usage_events
             WHERE ts_unix_ms >= ?1",
        );
        let mut params = vec![rusqlite::types::Value::Integer(since_ms)];
        if let Some(tenant) = tenant {
            sql.push_str(" AND tenant = ?");
            params.push(rusqlite::types::Value::Text(tenant.to_string()));
        }
        if let Some(project) = project {
            sql.push_str(" AND project = ?");
            params.push(rusqlite::types::Value::Text(project.to_string()));
        }
        sql.push_str(" ORDER BY ts_unix_ms");

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(UsageEventRecord {
                ts_unix_ms: row.get(0)?,
                op: row.get(1)?,
                outcome: row.get(2)?,
                chunk_count: row.get(3)?,
                bytes: row.get(4)?,
                detail: row.get(5)?,
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn usage_ledger_stats(&self) -> Result<(i64, Option<i64>)> {
        let conn = self.pool.get();
        let (row_count, min_ts) = conn.query_row(
            "SELECT COUNT(*), MIN(ts_unix_ms) FROM usage_events",
            [],
            |row| Ok((row.get::<usize, i64>(0)?, row.get::<usize, Option<i64>>(1)?)),
        )?;
        Ok((row_count, min_ts))
    }

    pub fn lifecycle_status_counts_since(
        &self,
        since_ms: i64,
        tenant: Option<&str>,
        project: Option<&str>,
    ) -> Result<BTreeMap<String, usize>> {
        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT status, COUNT(*)
             FROM chunks
             WHERE lifecycle_updated_at_ms >= ?1
               AND status IN ('expired', 'superseded')",
        );
        let mut params = vec![rusqlite::types::Value::Integer(since_ms)];
        if let Some(tenant) = tenant {
            sql.push_str(" AND tenant_id = ?");
            params.push(rusqlite::types::Value::Text(tenant.to_string()));
        }
        if let Some(project) = project {
            sql.push_str(" AND project_id = ?");
            params.push(rusqlite::types::Value::Text(project.to_string()));
        }
        sql.push_str(" GROUP BY status");

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count.max(0) as usize))
        })?;

        let mut counts = BTreeMap::new();
        for row in rows {
            let (status, count) = row?;
            counts.insert(status, count);
        }
        Ok(counts)
    }

    /// Delete usage ledger events older than `cutoff_ms`.
    pub fn sweep_usage_events_before(&self, cutoff_ms: i64) -> Result<usize> {
        let conn = self.pool.get();
        Ok(conn.execute(
            "DELETE FROM usage_events WHERE ts_unix_ms < ?1",
            rusqlite::params![cutoff_ms],
        )?)
    }
}
