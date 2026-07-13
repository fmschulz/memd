use super::*;

pub(super) const CONSOLIDATION_RUN_COLUMNS: &str =
    "run_id, tenant_id, project_id, input_hash, state, consolidator, \
     prompt_hash, response_hash, validation_result, error, sparse_cleanup_done, \
     consolidator_command, consolidator_model, consolidator_version, audit_artifact_path, \
     promotion_requested, created_at_ms, updated_at_ms";

pub(super) fn row_to_consolidation_run(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ConsolidationRun> {
    let run_id = ConsolidationRunId::parse(&row.get::<_, String>(0)?)
        .map_err(|error| sql_decode_error(0, error))?;
    let tenant_id =
        TenantId::new(row.get::<_, String>(1)?).map_err(|error| sql_decode_error(1, error))?;
    let state = row
        .get::<_, String>(4)?
        .parse::<ConsolidationState>()
        .map_err(|error| sql_decode_error(4, error))?;
    Ok(ConsolidationRun {
        run_id,
        tenant_id,
        project_id: row.get(2)?,
        input_hash: row.get(3)?,
        state,
        consolidator: row.get(5)?,
        prompt_hash: row.get(6)?,
        response_hash: row.get(7)?,
        validation_result: row.get(8)?,
        error: row.get(9)?,
        sparse_cleanup_done: row.get::<_, i64>(10)? != 0,
        consolidator_command: row.get(11)?,
        consolidator_model: row.get(12)?,
        consolidator_version: row.get(13)?,
        audit_artifact_path: row.get(14)?,
        promotion_requested: row.get::<_, i64>(15)? != 0,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
    })
}

pub(super) fn row_to_consolidation_entry(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ConsolidationEntryRecord> {
    let run_id = ConsolidationRunId::parse(&row.get::<_, String>(0)?)
        .map_err(|error| sql_decode_error(0, error))?;
    let candidate = row
        .get::<_, Option<String>>(2)?
        .map(|value| ChunkId::parse(&value).map_err(|error| sql_decode_error(2, error)))
        .transpose()?;
    let entry_index = usize::try_from(row.get::<_, i64>(1)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    let state = row
        .get::<_, String>(4)?
        .parse::<ConsolidationState>()
        .map_err(|error| sql_decode_error(4, error))?;
    Ok(ConsolidationEntryRecord {
        run_id,
        entry_index,
        candidate_chunk_id: candidate,
        source_set_hash: row.get(3)?,
        state,
        validation_error: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

pub(super) fn row_to_memory_lineage(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryLineage> {
    let run_id = ConsolidationRunId::parse(&row.get::<_, String>(0)?)
        .map_err(|error| sql_decode_error(0, error))?;
    let tenant_id =
        TenantId::new(row.get::<_, String>(1)?).map_err(|error| sql_decode_error(1, error))?;
    let source_chunk_id =
        ChunkId::parse(&row.get::<_, String>(3)?).map_err(|error| sql_decode_error(3, error))?;
    let result_chunk_id =
        ChunkId::parse(&row.get::<_, String>(4)?).map_err(|error| sql_decode_error(4, error))?;
    let relation = row
        .get::<_, String>(5)?
        .parse::<LineageRelation>()
        .map_err(|error| sql_decode_error(5, error))?;
    Ok(MemoryLineage {
        run_id,
        tenant_id,
        project_id: row.get(2)?,
        source_chunk_id,
        result_chunk_id,
        relation,
        created_at_ms: row.get(6)?,
    })
}

pub(super) fn query_consolidation_entries(
    conn: &Connection,
    run_id: &ConsolidationRunId,
) -> Result<Vec<ConsolidationEntryRecord>> {
    let mut statement = conn.prepare(
        "SELECT run_id, entry_index, candidate_chunk_id, source_set_hash,
                state, validation_error, created_at_ms, updated_at_ms
         FROM consolidation_entries WHERE run_id = ?1 ORDER BY entry_index ASC",
    )?;
    let rows = statement.query_map([run_id.to_string()], row_to_consolidation_entry)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

pub(super) fn query_memory_lineage(
    conn: &Connection,
    run_id: &ConsolidationRunId,
) -> Result<Vec<MemoryLineage>> {
    let mut statement = conn.prepare(
        "SELECT run_id, tenant_id, project_id, source_chunk_id,
                result_chunk_id, relation, created_at_ms
         FROM memory_lineage WHERE run_id = ?1
         ORDER BY result_chunk_id ASC, source_chunk_id ASC",
    )?;
    let rows = statement.query_map([run_id.to_string()], row_to_memory_lineage)?;
    let mut lineage = Vec::new();
    for row in rows {
        lineage.push(row?);
    }
    Ok(lineage)
}

pub(super) fn validate_consolidation_plan(
    run: &ConsolidationRun,
    entries: &[ConsolidationEntryRecord],
    lineage: &[MemoryLineage],
) -> Result<()> {
    if run.state != ConsolidationState::Planned || run.sparse_cleanup_done {
        return Err(MemdError::ValidationError(
            "new consolidation runs must start in planned state".to_string(),
        ));
    }
    if run.input_hash.trim().is_empty() || entries.is_empty() || lineage.is_empty() {
        return Err(MemdError::ValidationError(
            "a consolidation plan requires an input hash, entries, and lineage".to_string(),
        ));
    }
    let mut expected_relation = None;
    let mut indexes = std::collections::HashSet::new();
    let mut candidates = std::collections::HashSet::new();
    for entry in entries {
        let candidate = entry.candidate_chunk_id.as_ref().ok_or_else(|| {
            MemdError::ValidationError("planned entries require candidate chunk ids".to_string())
        })?;
        if entry.run_id != run.run_id
            || entry.state != ConsolidationState::Planned
            || entry.source_set_hash.trim().is_empty()
            || !indexes.insert(entry.entry_index)
            || !candidates.insert(candidate.clone())
        {
            return Err(MemdError::ValidationError(
                "consolidation entries do not form a unique planned run".to_string(),
            ));
        }
    }
    let mut sources = std::collections::HashSet::new();
    for edge in lineage {
        let run_relation = *expected_relation.get_or_insert(edge.relation);
        if edge.run_id != run.run_id
            || edge.tenant_id != run.tenant_id
            || edge.project_id != run.project_id
            || edge.relation != run_relation
            || !candidates.contains(&edge.result_chunk_id)
            || edge.source_chunk_id == edge.result_chunk_id
            || !sources.insert(edge.source_chunk_id.clone())
        {
            return Err(MemdError::ValidationError(
                "consolidation lineage does not match its run scope".to_string(),
            ));
        }
    }
    if candidates.iter().any(|candidate| {
        !lineage
            .iter()
            .any(|edge| edge.result_chunk_id == *candidate)
    }) {
        return Err(MemdError::ValidationError(
            "every consolidation candidate requires lineage".to_string(),
        ));
    }
    Ok(())
}

impl SqliteMetadataStore {
    /// Create a consolidation journal and all of its planned entries and
    /// lineage edges in one transaction. Repeating an exact tenant/scope/input
    /// returns the first run without duplicating entries.
    pub fn begin_consolidation_run(
        &self,
        run: &ConsolidationRun,
        entries: &[ConsolidationEntryRecord],
        lineage: &[MemoryLineage],
    ) -> Result<ConsolidationRun> {
        validate_consolidation_plan(run, entries, lineage)?;

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO consolidation_runs (
                 run_id, tenant_id, project_id, input_hash, state, consolidator,
                 prompt_hash, response_hash, validation_result, error,
                 sparse_cleanup_done, consolidator_command, consolidator_model,
                 consolidator_version, audit_artifact_path, promotion_requested,
                 created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                run.run_id.to_string(),
                run.tenant_id.as_str(),
                run.project_id.as_deref(),
                run.input_hash,
                run.state.as_str(),
                run.consolidator,
                run.prompt_hash.as_deref(),
                run.response_hash.as_deref(),
                run.validation_result.as_deref(),
                run.error.as_deref(),
                i64::from(run.sparse_cleanup_done),
                run.consolidator_command.as_deref(),
                run.consolidator_model.as_deref(),
                run.consolidator_version.as_deref(),
                run.audit_artifact_path.as_deref(),
                i64::from(run.promotion_requested),
                run.created_at_ms,
                run.updated_at_ms,
            ],
        )?;

        if inserted == 0 {
            let existing = tx.query_row(
                &format!(
                    "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs
                     WHERE tenant_id = ?1 AND project_id IS ?2 AND input_hash = ?3
                       AND state NOT IN ('rejected', 'rolled_back')"
                ),
                rusqlite::params![
                    run.tenant_id.as_str(),
                    run.project_id.as_deref(),
                    run.input_hash
                ],
                row_to_consolidation_run,
            )?;
            tx.commit()?;
            return Ok(existing);
        }

        for entry in entries {
            tx.execute(
                "INSERT INTO consolidation_entries (
                     run_id, entry_index, candidate_chunk_id, source_set_hash,
                     state, validation_error, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    entry.run_id.to_string(),
                    entry.entry_index as i64,
                    entry.candidate_chunk_id.as_ref().map(ToString::to_string),
                    entry.source_set_hash,
                    entry.state.as_str(),
                    entry.validation_error.as_deref(),
                    entry.created_at_ms,
                    entry.updated_at_ms,
                ],
            )?;
        }

        for edge in lineage {
            tx.execute(
                "INSERT INTO memory_lineage (
                     run_id, tenant_id, project_id, source_chunk_id,
                     result_chunk_id, relation, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    edge.run_id.to_string(),
                    edge.tenant_id.as_str(),
                    edge.project_id.as_deref(),
                    edge.source_chunk_id.to_string(),
                    edge.result_chunk_id.to_string(),
                    edge.relation.as_str(),
                    edge.created_at_ms,
                ],
            )?;
        }

        tx.commit()?;
        Ok(run.clone())
    }

    pub fn get_consolidation_run(
        &self,
        run_id: &ConsolidationRunId,
    ) -> Result<Option<ConsolidationRun>> {
        let conn = self.pool.get();
        let mut statement = conn.prepare(&format!(
            "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs WHERE run_id = ?1"
        ))?;
        let mut rows = statement.query([run_id.to_string()])?;
        Ok(rows.next()?.map(row_to_consolidation_run).transpose()?)
    }

    pub fn find_consolidation_run_by_input(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        input_hash: &str,
    ) -> Result<Option<ConsolidationRun>> {
        let conn = self.pool.get();
        let mut statement = conn.prepare(&format!(
            "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs
             WHERE tenant_id = ?1 AND project_id IS ?2 AND input_hash = ?3
               AND state NOT IN ('rejected', 'rolled_back')"
        ))?;
        let mut rows = statement.query(rusqlite::params![
            tenant_id.as_str(),
            project_id,
            input_hash
        ])?;
        Ok(rows.next()?.map(row_to_consolidation_run).transpose()?)
    }

    pub fn list_recoverable_consolidation_runs(
        &self,
        limit: usize,
    ) -> Result<Vec<ConsolidationRun>> {
        self.list_recoverable_consolidation_runs_before(limit, i64::MAX)
    }

    /// List validated candidates awaiting an explicit promotion/rejection
    /// decision. These runs are intentionally absent from automatic recovery.
    pub fn list_staged_consolidation_runs(&self, limit: usize) -> Result<Vec<ConsolidationRun>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut statement = conn.prepare(&format!(
            "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs
             WHERE state = 'validated' AND promotion_requested = 0
             ORDER BY updated_at_ms ASC, run_id ASC LIMIT ?1"
        ))?;
        let rows = statement.query_map([limit as i64], row_to_consolidation_run)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// List nonterminal runs whose last journal update is old enough for
    /// recovery. The cutoff is what prevents a startup/context request in
    /// the same process from rolling back a consolidation that is still
    /// writing its candidate payloads.
    pub fn list_recoverable_consolidation_runs_before(
        &self,
        limit: usize,
        updated_before_ms: i64,
    ) -> Result<Vec<ConsolidationRun>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut statement = conn.prepare(&format!(
            "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs
             WHERE (state IN ('planned', 'candidate_written', 'failed_recoverable')
                    OR (state = 'validated' AND promotion_requested = 1))
               AND updated_at_ms <= ?1
             ORDER BY updated_at_ms ASC, run_id ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            rusqlite::params![updated_before_ms, limit as i64],
            row_to_consolidation_run,
        )?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    pub fn list_consolidation_runs_pending_sparse_cleanup(
        &self,
        limit: usize,
    ) -> Result<Vec<ConsolidationRun>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut statement = conn.prepare(&format!(
            "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs
             WHERE state = 'committed' AND sparse_cleanup_done = 0
             ORDER BY updated_at_ms ASC, run_id ASC LIMIT ?1"
        ))?;
        let rows = statement.query_map([limit as i64], row_to_consolidation_run)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    pub fn mark_consolidation_sparse_cleanup_done(
        &self,
        run_id: &ConsolidationRunId,
        now_ms: i64,
    ) -> Result<bool> {
        let conn = self.pool.get();
        let rows = conn.execute(
            "UPDATE consolidation_runs
                SET sparse_cleanup_done = 1, updated_at_ms = ?1
              WHERE run_id = ?2 AND state = 'committed' AND sparse_cleanup_done = 0",
            rusqlite::params![now_ms, run_id.to_string()],
        )?;
        Ok(rows == 1)
    }

    /// Record a per-run recovery error without changing the protocol state.
    /// Updating the clock rotates a poisoned run behind other recoverable
    /// work instead of letting it remain the oldest row forever.
    pub fn record_consolidation_recovery_error(
        &self,
        run_id: &ConsolidationRunId,
        now_ms: i64,
        error: &str,
    ) -> Result<bool> {
        let conn = self.pool.get();
        let rows = conn.execute(
            "UPDATE consolidation_runs
                SET error = ?1, updated_at_ms = ?2
              WHERE run_id = ?3
                AND state NOT IN ('committed', 'rejected', 'rolled_back')",
            rusqlite::params![error, now_ms, run_id.to_string()],
        )?;
        Ok(rows == 1)
    }

    /// Persist explicit promotion intent before any validated candidate can
    /// become visible or supersede a source. Recovery consults this same bit.
    pub fn request_consolidation_promotion(
        &self,
        run_id: &ConsolidationRunId,
        now_ms: i64,
    ) -> Result<bool> {
        let conn = self.pool.get();
        let rows = conn.execute(
            "UPDATE consolidation_runs
                SET promotion_requested = 1, updated_at_ms = ?1
              WHERE run_id = ?2
                AND state IN ('planned', 'candidate_written', 'validated', 'failed_recoverable')",
            rusqlite::params![now_ms, run_id.to_string()],
        )?;
        Ok(rows == 1)
    }

    /// Hide any candidate payloads still attached to a terminal run. This
    /// reconciles the narrow race where a writer persists a candidate after
    /// another task has already rolled its Planned run back.
    pub fn hide_terminal_consolidation_candidates(&self, now_ms: i64) -> Result<usize> {
        let conn = self.pool.get();
        Ok(conn.execute(
            "UPDATE chunks
                SET status = 'error', lifecycle_updated_at_ms = ?1
              WHERE status = 'candidate' AND chunk_id IN (
                    SELECT entry.candidate_chunk_id
                      FROM consolidation_entries AS entry
                      JOIN consolidation_runs AS run ON run.run_id = entry.run_id
                     WHERE run.state IN ('committed', 'rejected', 'rolled_back')
              )",
            [now_ms],
        )?)
    }

    /// Hide every still-staged payload for one run without changing its
    /// journal state. Used when a guarded transition discovers that another
    /// recovery task already made the run terminal.
    pub fn hide_consolidation_candidates(
        &self,
        run_id: &ConsolidationRunId,
        now_ms: i64,
    ) -> Result<usize> {
        let conn = self.pool.get();
        Ok(conn.execute(
            "UPDATE chunks
                SET status = 'error', lifecycle_updated_at_ms = ?1
              WHERE status = 'candidate' AND chunk_id IN (
                    SELECT candidate_chunk_id
                      FROM consolidation_entries
                     WHERE run_id = ?2
              )",
            rusqlite::params![now_ms, run_id.to_string()],
        )?)
    }

    pub fn get_consolidation_entries(
        &self,
        run_id: &ConsolidationRunId,
    ) -> Result<Vec<ConsolidationEntryRecord>> {
        let conn = self.pool.get();
        query_consolidation_entries(&conn, run_id)
    }

    pub fn get_memory_lineage(&self, run_id: &ConsolidationRunId) -> Result<Vec<MemoryLineage>> {
        let conn = self.pool.get();
        query_memory_lineage(&conn, run_id)
    }

    /// Move a run and all of its entries through one guarded state transition.
    pub fn transition_consolidation_run(
        &self,
        run_id: &ConsolidationRunId,
        expected: ConsolidationState,
        next: ConsolidationState,
        now_ms: i64,
        validation_result: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        let mut conn = self.pool.get();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run_rows = tx.execute(
            "UPDATE consolidation_runs
                SET state = ?1, validation_result = COALESCE(?2, validation_result),
                    error = ?3, updated_at_ms = ?4
              WHERE run_id = ?5 AND state = ?6",
            rusqlite::params![
                next.as_str(),
                validation_result,
                error,
                now_ms,
                run_id.to_string(),
                expected.as_str(),
            ],
        )?;
        if run_rows == 0 {
            return Ok(false);
        }
        let entry_rows = tx.execute(
            "UPDATE consolidation_entries
                SET state = ?1, validation_error = ?2, updated_at_ms = ?3
              WHERE run_id = ?4 AND state = ?5",
            rusqlite::params![
                next.as_str(),
                error,
                now_ms,
                run_id.to_string(),
                expected.as_str(),
            ],
        )?;
        let total_entries: i64 = tx.query_row(
            "SELECT COUNT(*) FROM consolidation_entries WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )?;
        if entry_rows != total_entries as usize {
            return Err(MemdError::StorageError(format!(
                "consolidation run {run_id} has entries outside expected state {expected}"
            )));
        }
        tx.commit()?;
        Ok(true)
    }

    /// Reject or roll back a run while keeping any already-written candidate
    /// payloads durably hidden. Candidate metadata is moved to `error` in the
    /// same transaction as the terminal journal state.
    pub fn terminate_consolidation_run(
        &self,
        run_id: &ConsolidationRunId,
        expected: ConsolidationState,
        terminal: ConsolidationState,
        now_ms: i64,
        error: &str,
    ) -> Result<bool> {
        if !matches!(
            terminal,
            ConsolidationState::Rejected | ConsolidationState::RolledBack
        ) {
            return Err(MemdError::ValidationError(
                "consolidation termination requires rejected or rolled_back state".to_string(),
            ));
        }
        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        let run_rows = tx.execute(
            "UPDATE consolidation_runs
                SET state = ?1, error = ?2, updated_at_ms = ?3
              WHERE run_id = ?4 AND state = ?5",
            rusqlite::params![
                terminal.as_str(),
                error,
                now_ms,
                run_id.to_string(),
                expected.as_str(),
            ],
        )?;
        if run_rows == 0 {
            return Ok(false);
        }
        let entry_rows = tx.execute(
            "UPDATE consolidation_entries
                SET state = ?1, validation_error = ?2, updated_at_ms = ?3
              WHERE run_id = ?4 AND state = ?5",
            rusqlite::params![
                terminal.as_str(),
                error,
                now_ms,
                run_id.to_string(),
                expected.as_str(),
            ],
        )?;
        let total_entries: i64 = tx.query_row(
            "SELECT COUNT(*) FROM consolidation_entries WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )?;
        if entry_rows != total_entries as usize {
            return Err(MemdError::StorageError(format!(
                "consolidation run {run_id} has entries outside expected state {expected}"
            )));
        }
        tx.execute(
            "UPDATE chunks
                SET status = 'error', lifecycle_updated_at_ms = ?1
              WHERE status = 'candidate' AND chunk_id IN (
                    SELECT candidate_chunk_id FROM consolidation_entries WHERE run_id = ?2
              )",
            rusqlite::params![now_ms, run_id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Promote all validated candidates and, for project-scoped runs, their
    /// sources in one SQLite transaction. Tenant-wide derivations never
    /// tombstone or supersede project-owned sources.
    pub fn atomic_promote_consolidation_run(
        &self,
        run_id: &ConsolidationRunId,
        now_ms: i64,
    ) -> Result<PromotionOutcome> {
        let mut conn = self.pool.get();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = tx.query_row(
            &format!(
                "SELECT {CONSOLIDATION_RUN_COLUMNS} FROM consolidation_runs WHERE run_id = ?1"
            ),
            [run_id.to_string()],
            row_to_consolidation_run,
        )?;
        if run.state == ConsolidationState::Committed {
            return Ok(PromotionOutcome::AlreadyCommitted);
        }
        if run.state != ConsolidationState::Validated {
            return Err(MemdError::StorageError(format!(
                "consolidation run {run_id} cannot promote from state {}",
                run.state
            )));
        }
        if !run.promotion_requested {
            return Err(MemdError::ValidationError(format!(
                "consolidation run {run_id} has no durable promotion request"
            )));
        }

        let entries = query_consolidation_entries(&tx, run_id)?;
        let lineage = query_memory_lineage(&tx, run_id)?;
        if entries.is_empty() || lineage.is_empty() {
            return Err(MemdError::StorageError(format!(
                "consolidation run {run_id} has no entries or lineage"
            )));
        }

        let candidate_ids = entries
            .iter()
            .map(|entry| {
                if entry.state != ConsolidationState::Validated {
                    return Err(MemdError::StorageError(format!(
                        "consolidation entry {} is not validated",
                        entry.entry_index
                    )));
                }
                entry.candidate_chunk_id.clone().ok_or_else(|| {
                    MemdError::StorageError(format!(
                        "consolidation entry {} has no candidate chunk",
                        entry.entry_index
                    ))
                })
            })
            .collect::<Result<std::collections::HashSet<_>>>()?;

        for candidate_id in &candidate_ids {
            let (status, project_id): (String, Option<String>) = tx.query_row(
                "SELECT status, project_id FROM chunks
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
                rusqlite::params![run.tenant_id.as_str(), candidate_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if status != ChunkStatus::Candidate.to_string() || project_id != run.project_id {
                return Err(MemdError::StorageError(format!(
                    "candidate {candidate_id} drifted outside run {run_id}"
                )));
            }
            if !lineage
                .iter()
                .any(|edge| edge.result_chunk_id == *candidate_id)
            {
                return Err(MemdError::StorageError(format!(
                    "candidate {candidate_id} has no lineage in run {run_id}"
                )));
            }
        }

        for edge in &lineage {
            if edge.tenant_id != run.tenant_id
                || edge.project_id != run.project_id
                || !candidate_ids.contains(&edge.result_chunk_id)
            {
                return Err(MemdError::StorageError(format!(
                    "lineage edge escaped consolidation run {run_id} scope"
                )));
            }
            let expected_relation = lineage[0].relation;
            if edge.relation != expected_relation {
                return Err(MemdError::StorageError(format!(
                    "lineage relation {} is invalid for run {run_id} scope",
                    edge.relation
                )));
            }

            let (source_status, source_project, superseded_by, tier, expires_at_ms): (
                String,
                Option<String>,
                Option<String>,
                String,
                Option<i64>,
            ) = tx.query_row(
                "SELECT status, project_id, superseded_by, tier, expires_at_ms FROM chunks
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
                rusqlite::params![run.tenant_id.as_str(), edge.source_chunk_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
            if source_status != ChunkStatus::Final.to_string()
                || superseded_by.is_some()
                || tier == MemoryTier::History.to_string()
                || expires_at_ms.is_some_and(|expiry| expiry <= now_ms)
            {
                return Err(MemdError::StorageError(format!(
                    "source {} is no longer a visible final head for run {run_id}",
                    edge.source_chunk_id
                )));
            }
            if expected_relation == LineageRelation::Supersedes && source_project != run.project_id
            {
                return Err(MemdError::StorageError(format!(
                    "source {} escaped project scope for run {run_id}",
                    edge.source_chunk_id
                )));
            }
        }

        for candidate_id in &candidate_ids {
            let rows = tx.execute(
                "UPDATE chunks
                    SET status = 'final', lifecycle_updated_at_ms = ?1
                  WHERE tenant_id = ?2 AND chunk_id = ?3 AND status = 'candidate'",
                rusqlite::params![now_ms, run.tenant_id.as_str(), candidate_id.to_string()],
            )?;
            if rows != 1 {
                return Err(MemdError::StorageError(format!(
                    "candidate {candidate_id} could not be promoted for run {run_id}"
                )));
            }
        }

        if lineage[0].relation == LineageRelation::Supersedes {
            for edge in &lineage {
                let rows = tx.execute(
                    "UPDATE chunks
                        SET status = 'superseded', superseded_by = ?1,
                            lifecycle_updated_at_ms = ?2
                      WHERE tenant_id = ?3 AND chunk_id = ?4 AND status = 'final'
                        AND project_id IS ?5 AND superseded_by IS NULL
                        AND tier != 'history'
                        AND (expires_at_ms IS NULL OR expires_at_ms > ?6)",
                    rusqlite::params![
                        edge.result_chunk_id.to_string(),
                        now_ms,
                        run.tenant_id.as_str(),
                        edge.source_chunk_id.to_string(),
                        run.project_id.as_deref(),
                        now_ms,
                    ],
                )?;
                if rows != 1 {
                    return Err(MemdError::StorageError(format!(
                        "source {} could not be superseded for run {run_id}",
                        edge.source_chunk_id
                    )));
                }
            }
        }

        let entry_rows = tx.execute(
            "UPDATE consolidation_entries
                SET state = 'committed', updated_at_ms = ?1
              WHERE run_id = ?2 AND state = 'validated'",
            rusqlite::params![now_ms, run_id.to_string()],
        )?;
        if entry_rows != entries.len() {
            return Err(MemdError::StorageError(format!(
                "not all entries committed for run {run_id}"
            )));
        }
        let run_rows = tx.execute(
            "UPDATE consolidation_runs
                SET state = 'committed', error = NULL, updated_at_ms = ?1
              WHERE run_id = ?2 AND state = 'validated'",
            rusqlite::params![now_ms, run_id.to_string()],
        )?;
        if run_rows != 1 {
            return Err(MemdError::StorageError(format!(
                "run {run_id} lost its validated promotion guard"
            )));
        }
        tx.commit()?;
        Ok(PromotionOutcome::Committed)
    }
}
