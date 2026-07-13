use super::*;

impl SqliteMetadataStore {
    /// Insert the canonical task artifact envelope plus normalized side rows.
    pub fn insert_task_artifact_bundle(
        &self,
        artifact: &TaskArtifact,
        projection_chunk_ids: &[String],
        projection_kinds: &[String],
    ) -> Result<()> {
        let canonical_json = serde_json::to_string(artifact)?;
        let summary = artifact.event_summary();
        let status = artifact.status.as_deref();
        let project_id = artifact.project_id.as_option();
        let scientific_question = artifact.scientific_question.as_deref();
        let hypothesis = artifact.hypothesis.as_deref();
        let started_at_ms =
            (artifact.artifact_kind.as_str() == "task_start").then_some(artifact.timestamp_created);
        let finished_at_ms = (artifact.artifact_kind.as_str() == "task_finish")
            .then_some(artifact.timestamp_created);

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT OR REPLACE INTO task_artifacts (
                artifact_id, tenant_id, project_id, task_id, parent_task_id,
                artifact_kind, status, artifact_role, challenge_id, thread_id, reply_to_artifact_id,
                agent_id, session_id, goal, summary, tool_name, tool_version,
                requested_action, verification_status, promotion_state, digest_key, source_updated_at_ms,
                canonical_json, timestamp_created, timestamp_observed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            rusqlite::params![
                artifact.artifact_id.as_str(),
                artifact.tenant_id.as_str(),
                project_id,
                artifact.task_id.as_str(),
                artifact.parent_task_id.as_deref(),
                artifact.artifact_kind.as_str(),
                status,
                artifact.artifact_role.as_deref(),
                artifact.challenge_id.as_deref(),
                artifact.thread_id.as_deref(),
                artifact.reply_to_artifact_id.as_deref(),
                artifact.agent_id.as_deref(),
                artifact.session_id.as_deref(),
                artifact.goal.as_deref(),
                summary.as_deref(),
                artifact.tool_name.as_deref(),
                artifact.tool_version.as_deref(),
                artifact.requested_action.as_deref(),
                artifact.verification_status.as_deref(),
                artifact.promotion_state.to_string(),
                artifact.digest_key.as_deref(),
                artifact.source_updated_at_ms,
                canonical_json,
                artifact.timestamp_created,
                artifact.timestamp_observed,
            ],
        )?;

        if artifact.artifact_kind != ArtifactKind::Digest {
            tx.execute(
                "INSERT INTO tasks (
                    task_id, tenant_id, project_id, parent_task_id, agent_id, session_id,
                    status, goal, scientific_question, hypothesis, last_artifact_id,
                    started_at_ms, finished_at_ms, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(task_id) DO UPDATE SET
                    project_id = COALESCE(excluded.project_id, tasks.project_id),
                    parent_task_id = COALESCE(excluded.parent_task_id, tasks.parent_task_id),
                    agent_id = COALESCE(excluded.agent_id, tasks.agent_id),
                    session_id = COALESCE(excluded.session_id, tasks.session_id),
                    status = COALESCE(excluded.status, tasks.status),
                    goal = COALESCE(excluded.goal, tasks.goal),
                    scientific_question = COALESCE(excluded.scientific_question, tasks.scientific_question),
                    hypothesis = COALESCE(excluded.hypothesis, tasks.hypothesis),
                    last_artifact_id = excluded.last_artifact_id,
                    started_at_ms = COALESCE(tasks.started_at_ms, excluded.started_at_ms),
                    finished_at_ms = COALESCE(excluded.finished_at_ms, tasks.finished_at_ms),
                    updated_at_ms = excluded.updated_at_ms",
                rusqlite::params![
                    artifact.task_id.as_str(),
                    artifact.tenant_id.as_str(),
                    project_id,
                    artifact.parent_task_id.as_deref(),
                    artifact.agent_id.as_deref(),
                    artifact.session_id.as_deref(),
                    status,
                    artifact.goal.as_deref(),
                    scientific_question,
                    hypothesis,
                    artifact.artifact_id.as_str(),
                    started_at_ms,
                    finished_at_ms,
                    artifact.timestamp_created,
                ],
            )?;

            tx.execute(
                "INSERT OR REPLACE INTO task_events (
                    artifact_id, tenant_id, task_id, artifact_kind, status, summary,
                    timestamp_created, timestamp_observed
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    artifact.artifact_kind.as_str(),
                    status,
                    summary.as_deref(),
                    artifact.timestamp_created,
                    artifact.timestamp_observed,
                ],
            )?;
        }

        if let Some(challenge_id) = artifact.challenge_id.as_deref() {
            tx.execute(
                "INSERT INTO challenges (
                    challenge_id, tenant_id, project_id, summary, status, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(challenge_id) DO UPDATE SET
                    project_id = COALESCE(excluded.project_id, challenges.project_id),
                    summary = COALESCE(excluded.summary, challenges.summary),
                    status = COALESCE(excluded.status, challenges.status),
                    updated_at_ms = excluded.updated_at_ms",
                rusqlite::params![
                    challenge_id,
                    artifact.tenant_id.as_str(),
                    project_id,
                    summary.as_deref().or(artifact.goal.as_deref()),
                    status,
                    artifact.timestamp_created,
                ],
            )?;
        }

        for dataset in &artifact.dataset_refs {
            tx.execute(
                "INSERT OR REPLACE INTO datasets (dataset_key, dataset_name, dataset_version, description)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    dataset.key(),
                    dataset.name.as_str(),
                    dataset.version.as_deref(),
                    dataset.description.as_deref(),
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO task_datasets (
                    artifact_id, tenant_id, task_id, dataset_key, dataset_name, dataset_version
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    dataset.key(),
                    dataset.name.as_str(),
                    dataset.version.as_deref(),
                ],
            )?;
        }

        for entity in &artifact.entity_refs {
            tx.execute(
                "INSERT OR REPLACE INTO entities (entity_key, entity_name, entity_type, role)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    entity.key(),
                    entity.name.as_str(),
                    entity.entity_type.as_str(),
                    entity.role.as_deref(),
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO task_entities (
                    artifact_id, tenant_id, task_id, entity_key, entity_name, entity_type
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    entity.key(),
                    entity.name.as_str(),
                    entity.entity_type.as_str(),
                ],
            )?;
        }

        let old_projection_chunk_ids = if artifact.artifact_kind == ArtifactKind::Digest {
            let mut stmt = tx.prepare(
                "SELECT chunk_id FROM artifact_links
                 WHERE artifact_id = ?1 AND link_kind = 'retrieval_projection'",
            )?;
            let rows = stmt.query_map(rusqlite::params![artifact.artifact_id.as_str()], |row| {
                row.get::<usize, String>(0)
            })?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row?);
            }
            ids
        } else {
            Vec::new()
        };

        if artifact.artifact_kind == ArtifactKind::Digest {
            let current_ids = projection_chunk_ids
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            for old_chunk_id in old_projection_chunk_ids {
                if current_ids.contains(&old_chunk_id) {
                    continue;
                }
                tx.execute(
                    "UPDATE chunks
                        SET status = 'superseded',
                            tier = 'history',
                            lifecycle_updated_at_ms = ?3
                      WHERE tenant_id = ?1
                        AND chunk_id = ?2
                        AND status NOT IN ('candidate', 'deleted')",
                    rusqlite::params![
                        artifact.tenant_id.as_str(),
                        old_chunk_id,
                        artifact.timestamp_created,
                    ],
                )?;
            }

            tx.execute(
                "DELETE FROM artifact_links
                 WHERE artifact_id = ?1 AND link_kind = 'retrieval_projection'",
                rusqlite::params![artifact.artifact_id.as_str()],
            )?;
        }

        for (idx, chunk_id) in projection_chunk_ids.iter().enumerate() {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_links (
                    artifact_id, chunk_id, link_kind, projection_kind
                ) VALUES (?1, ?2, 'retrieval_projection', ?3)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    chunk_id,
                    projection_kinds.get(idx).map(|kind| kind.as_str()),
                ],
            )?;
        }

        if artifact.artifact_kind == ArtifactKind::Digest {
            for chunk_id in projection_chunk_ids {
                tx.execute(
                    "UPDATE chunks
                        SET status = 'superseded',
                            tier = 'history',
                            lifecycle_updated_at_ms = ?4
                      WHERE tenant_id = ?1
                        AND chunk_id != ?2
                        AND chunk_id NOT IN (
                            SELECT chunk_id FROM artifact_links
                             WHERE artifact_id = ?5
                               AND link_kind = 'retrieval_projection'
                        )
                        AND status NOT IN ('candidate', 'deleted')
                        AND ((?3 IS NULL AND project_id IS NULL) OR project_id = ?3)
                        AND canonical_text IS NOT NULL
                        AND canonical_text = (
                            SELECT canonical_text FROM chunks
                             WHERE tenant_id = ?1 AND chunk_id = ?2
                        )",
                    rusqlite::params![
                        artifact.tenant_id.as_str(),
                        chunk_id,
                        project_id,
                        artifact.timestamp_created,
                        artifact.artifact_id.as_str(),
                    ],
                )?;
            }
        }

        if let Some(reply_to_artifact_id) = artifact.reply_to_artifact_id.as_deref() {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_relations (
                    artifact_id, tenant_id, related_artifact_id, relation_kind
                ) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    reply_to_artifact_id,
                    artifact.relation_kind.as_deref().unwrap_or("reply_to"),
                ],
            )?;
        }

        for related_artifact_id in &artifact.related_artifact_ids {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_relations (
                    artifact_id, tenant_id, related_artifact_id, relation_kind
                ) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    related_artifact_id.as_str(),
                    artifact.relation_kind.as_deref().unwrap_or("related"),
                ],
            )?;
        }

        for contributor in &artifact.contributors {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_contributors (
                    artifact_id, tenant_id, contributor_id, display_name, role, contribution
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    contributor.contributor_id.as_str(),
                    contributor.display_name.as_deref(),
                    contributor.role.as_deref(),
                    contributor.contribution.as_deref(),
                ],
            )?;
        }

        match artifact.artifact_kind {
            crate::task_memory::ArtifactKind::RunStart
            | crate::task_memory::ArtifactKind::RunFinish => {
                tx.execute(
                    "INSERT OR REPLACE INTO runs (
                        artifact_id, tenant_id, task_id, tool_name, status, command, metrics_json, timestamp_created
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        artifact.artifact_id.as_str(),
                        artifact.tenant_id.as_str(),
                        artifact.task_id.as_str(),
                        artifact.tool_name.as_deref(),
                        status,
                        artifact.command.as_deref(),
                        artifact.metrics.as_ref().map(|metrics| metrics.to_string()),
                        artifact.timestamp_created,
                    ],
                )?;
            }
            crate::task_memory::ArtifactKind::Evidence => {
                tx.execute(
                    "INSERT OR REPLACE INTO evidence (
                        artifact_id, tenant_id, task_id, summary, supports_claim, metrics_json, timestamp_created
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        artifact.artifact_id.as_str(),
                        artifact.tenant_id.as_str(),
                        artifact.task_id.as_str(),
                        summary.as_deref(),
                        artifact.supports_claim.map(|value| if value { 1 } else { 0 }),
                        artifact.metrics.as_ref().map(|metrics| metrics.to_string()),
                        artifact.timestamp_created,
                    ],
                )?;
            }
            _ => {}
        }

        tx.commit()?;
        Ok(())
    }

    /// Fetch the canonical task artifact envelope by ID.
    pub fn get_task_artifact(
        &self,
        tenant_id: &TenantId,
        artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1 AND artifact_id = ?2",
        )?;

        let result = stmt.query_row(rusqlite::params![tenant_id.as_str(), artifact_id], |row| {
            row.get::<usize, String>(0)
        });

        match result {
            Ok(canonical_json) => Ok(Some(serde_json::from_str(&canonical_json)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List canonical task artifacts for one logical task ordered by creation time.
    pub fn list_task_artifacts(
        &self,
        tenant_id: &TenantId,
        task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1 AND task_id = ?2
             ORDER BY timestamp_created ASC, artifact_id ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), task_id], |row| {
            row.get::<usize, String>(0)
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(serde_json::from_str(&row?)?);
        }
        Ok(artifacts)
    }

    /// List canonical task artifacts for a thread ordered by creation time.
    pub fn list_thread_artifacts(
        &self,
        tenant_id: &TenantId,
        thread_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1
               AND (thread_id = ?2 OR (thread_id IS NULL AND task_id = ?2))
             ORDER BY timestamp_created ASC, artifact_id ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), thread_id], |row| {
            row.get::<usize, String>(0)
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(serde_json::from_str(&row?)?);
        }
        Ok(artifacts)
    }

    pub fn list_tasks(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT task_id, tenant_id, project_id, status, goal, scientific_question,
                    hypothesis, last_artifact_id, started_at_ms, finished_at_ms, updated_at_ms
             FROM tasks
             WHERE tenant_id = ?1",
        );
        let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            sql.push_str(" AND project_id = ?2");
            params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        sql.push_str(" ORDER BY updated_at_ms DESC");
        sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Integer(limit as i64));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(TaskRecord {
                task_id: row.get(0)?,
                tenant_id: TenantId::new(row.get::<usize, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                project_id: crate::types::ProjectId::from(row.get::<usize, Option<String>>(2)?),
                status: row.get(3)?,
                goal: row.get(4)?,
                scientific_question: row.get(5)?,
                hypothesis: row.get(6)?,
                last_artifact_id: row.get(7)?,
                started_at_ms: row.get(8)?,
                finished_at_ms: row.get(9)?,
                updated_at_ms: row.get(10)?,
            })
        })?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// Resolve projection chunk IDs for exact task filters.
    pub fn search_task_projection_chunk_ids(
        &self,
        tenant_id: &TenantId,
        filters: &TaskSearchFilters,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        use rusqlite::types::Value as SqlValue;

        let mut sql = String::from(
            "SELECT DISTINCT l.chunk_id
             FROM task_artifacts a
             JOIN artifact_links l
               ON l.artifact_id = a.artifact_id
              AND l.link_kind = 'retrieval_projection'
             WHERE a.tenant_id = ?1",
        );
        let mut params = vec![SqlValue::Text(tenant_id.as_str().to_string())];
        let mut idx = 2usize;

        if let Some(task_id) = filters.task_id.as_deref() {
            sql.push_str(&format!(" AND a.task_id = ?{}", idx));
            params.push(SqlValue::Text(task_id.to_string()));
            idx += 1;
        }
        if let Some(artifact_kind) = filters.artifact_kind {
            sql.push_str(&format!(" AND a.artifact_kind = ?{}", idx));
            params.push(SqlValue::Text(artifact_kind.as_str().to_string()));
            idx += 1;
        }
        if let Some(status) = filters.status.as_deref() {
            sql.push_str(&format!(" AND a.status = ?{}", idx));
            params.push(SqlValue::Text(status.to_string()));
            idx += 1;
        }
        if let Some(challenge_id) = filters.challenge_id.as_deref() {
            sql.push_str(&format!(" AND a.challenge_id = ?{}", idx));
            params.push(SqlValue::Text(challenge_id.to_string()));
            idx += 1;
        }
        if let Some(thread_id) = filters.thread_id.as_deref() {
            sql.push_str(&format!(
                " AND (a.thread_id = ?{} OR (a.thread_id IS NULL AND a.task_id = ?{}))",
                idx, idx
            ));
            params.push(SqlValue::Text(thread_id.to_string()));
            idx += 1;
        }
        if let Some(reply_to_artifact_id) = filters.reply_to_artifact_id.as_deref() {
            sql.push_str(&format!(" AND a.reply_to_artifact_id = ?{}", idx));
            params.push(SqlValue::Text(reply_to_artifact_id.to_string()));
            idx += 1;
        }
        if let Some(artifact_role) = filters.artifact_role.as_deref() {
            sql.push_str(&format!(" AND a.artifact_role = ?{}", idx));
            params.push(SqlValue::Text(artifact_role.to_string()));
            idx += 1;
        }
        if let Some(project_id) = filters.project_id.as_deref() {
            sql.push_str(&format!(" AND a.project_id = ?{}", idx));
            params.push(SqlValue::Text(project_id.to_string()));
            idx += 1;
        }
        if let Some(agent_id) = filters.agent_id.as_deref() {
            sql.push_str(&format!(" AND a.agent_id = ?{}", idx));
            params.push(SqlValue::Text(agent_id.to_string()));
            idx += 1;
        }
        if let Some(session_id) = filters.session_id.as_deref() {
            sql.push_str(&format!(" AND a.session_id = ?{}", idx));
            params.push(SqlValue::Text(session_id.to_string()));
            idx += 1;
        }
        if let Some(tool_name) = filters.tool_name.as_deref() {
            sql.push_str(&format!(" AND a.tool_name = ?{}", idx));
            params.push(SqlValue::Text(tool_name.to_string()));
            idx += 1;
        }
        if let Some(requested_action) = filters.requested_action.as_deref() {
            sql.push_str(&format!(" AND a.requested_action = ?{}", idx));
            params.push(SqlValue::Text(requested_action.to_string()));
            idx += 1;
        }
        if let Some(verification_status) = filters.verification_status.as_deref() {
            sql.push_str(&format!(" AND a.verification_status = ?{}", idx));
            params.push(SqlValue::Text(verification_status.to_string()));
            idx += 1;
        }
        if let Some(relation_kind) = filters.relation_kind.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM artifact_relations ar
                    WHERE ar.artifact_id = a.artifact_id
                      AND ar.relation_kind = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(relation_kind.to_string()));
            idx += 1;
        }
        if let Some(dataset_name) = filters.dataset_name.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_datasets d
                    WHERE d.artifact_id = a.artifact_id
                      AND d.dataset_name = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(dataset_name.to_string()));
            idx += 1;
            if let Some(dataset_version) = filters.dataset_version.as_deref() {
                sql.push_str(&format!(
                    " AND EXISTS (
                        SELECT 1
                        FROM task_datasets dv
                        WHERE dv.artifact_id = a.artifact_id
                          AND dv.dataset_name = ?{}
                          AND dv.dataset_version = ?{}
                    )",
                    idx,
                    idx + 1
                ));
                params.push(SqlValue::Text(dataset_name.to_string()));
                params.push(SqlValue::Text(dataset_version.to_string()));
                idx += 2;
            }
        } else if let Some(dataset_version) = filters.dataset_version.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_datasets d
                    WHERE d.artifact_id = a.artifact_id
                      AND d.dataset_version = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(dataset_version.to_string()));
            idx += 1;
        }
        if let Some(entity_name) = filters.entity_name.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_entities e
                    WHERE e.artifact_id = a.artifact_id
                      AND e.entity_name = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(entity_name.to_string()));
            idx += 1;
            if let Some(entity_type) = filters.entity_type.as_deref() {
                sql.push_str(&format!(
                    " AND EXISTS (
                        SELECT 1
                        FROM task_entities et
                        WHERE et.artifact_id = a.artifact_id
                          AND et.entity_name = ?{}
                          AND et.entity_type = ?{}
                    )",
                    idx,
                    idx + 1
                ));
                params.push(SqlValue::Text(entity_name.to_string()));
                params.push(SqlValue::Text(entity_type.to_string()));
                idx += 2;
            }
        } else if let Some(entity_type) = filters.entity_type.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_entities e
                    WHERE e.artifact_id = a.artifact_id
                      AND e.entity_type = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(entity_type.to_string()));
            idx += 1;
        }

        sql.push_str(" ORDER BY a.timestamp_created DESC");
        sql.push_str(&format!(" LIMIT ?{}", idx));
        params.push(SqlValue::Integer(limit as i64));

        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<usize, String>(0)
        })?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            if let Ok(chunk_id) = ChunkId::parse(&row?) {
                chunk_ids.push(chunk_id);
            }
        }
        Ok(chunk_ids)
    }

    /// Resolve canonical artifacts for retrieval projection chunk IDs.
    pub fn resolve_artifacts_for_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<HashMap<String, TaskArtifact>> {
        let mut resolved = HashMap::new();
        if chunk_ids.is_empty() {
            return Ok(resolved);
        }

        let placeholders = (0..chunk_ids.len())
            .map(|idx| format!("?{}", idx + 2))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "SELECT l.chunk_id, a.canonical_json
             FROM artifact_links l
             JOIN task_artifacts a
               ON a.artifact_id = l.artifact_id
             WHERE a.tenant_id = ?1
               AND l.link_kind = 'retrieval_projection'
               AND l.chunk_id IN ({})",
            placeholders
        );

        let mut params = Vec::with_capacity(chunk_ids.len() + 1);
        params.push(rusqlite::types::Value::Text(tenant_id.as_str().to_string()));
        for chunk_id in chunk_ids {
            params.push(rusqlite::types::Value::Text(chunk_id.to_string()));
        }

        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok((row.get::<usize, String>(0)?, row.get::<usize, String>(1)?))
        })?;

        for row in rows {
            let (chunk_id, canonical_json) = row?;
            resolved.insert(chunk_id, serde_json::from_str(&canonical_json)?);
        }

        Ok(resolved)
    }
}
