use super::schema::{detect_chunks_unique_shape, ChunksUniqueShape};
use super::*;
use crate::task_memory::{ArtifactKind, DatasetRef, TaskArtifact, TaskSearchFilters};
use crate::types::ProjectId;
use rusqlite::Connection;
use tempfile::tempdir;

fn create_test_metadata(tenant: &str, chunk_id: &ChunkId) -> ChunkMetadata {
    ChunkMetadata {
        chunk_id: chunk_id.clone(),
        tenant_id: TenantId::new(tenant).unwrap(),
        project_id: None,
        segment_id: 1,
        ordinal: 0,
        chunk_type: ChunkType::Doc,
        status: ChunkStatus::Final,
        timestamp_created: 1234567890,
        hash: "abc123".to_string(),
        source_uri: None,
        lifecycle: LifecycleMetadata::default(),
        canonical_text: None,
        ingestion_mode: crate::types::IngestionMode::Document,
    }
}

#[test]
fn insert_and_get() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let chunk_id = ChunkId::new();
    let tenant_id = TenantId::new("tenant_a").unwrap();
    let metadata = create_test_metadata("tenant_a", &chunk_id);

    store.insert(&metadata).unwrap();

    let retrieved = store.get(&tenant_id, &chunk_id).unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.chunk_id, chunk_id);
    assert_eq!(retrieved.tenant_id, tenant_id);
    assert_eq!(retrieved.hash, "abc123");
}

#[test]
fn insert_many_round_trip() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant_id = TenantId::new("tenant_batch").unwrap();

    let mut rows = Vec::new();
    for i in 0..3u32 {
        let chunk_id = ChunkId::new();
        let mut row = create_test_metadata("tenant_batch", &chunk_id);
        row.segment_id = 100 + i as u64;
        row.ordinal = i;
        row.timestamp_created = 2000 + i as i64;
        rows.push(row);
    }

    store.insert_many(&rows).unwrap();

    let listed = store.list(&tenant_id, 10, 0).unwrap();
    assert_eq!(listed.len(), 3);
}

#[test]
fn insert_many_empty_is_noop() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant_id = TenantId::new("tenant_empty").unwrap();

    store.insert_many(&[]).unwrap();
    let listed = store.list(&tenant_id, 10, 0).unwrap();
    assert!(listed.is_empty());
}

#[test]
fn tenant_isolation() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let chunk_id = ChunkId::new();
    let metadata = create_test_metadata("tenant_a", &chunk_id);
    store.insert(&metadata).unwrap();

    // Tenant A can see their own chunk
    let tenant_a = TenantId::new("tenant_a").unwrap();
    let result_a = store.get(&tenant_a, &chunk_id).unwrap();
    assert!(result_a.is_some());

    // Tenant B cannot see Tenant A's chunk
    let tenant_b = TenantId::new("tenant_b").unwrap();
    let result_b = store.get(&tenant_b, &chunk_id).unwrap();
    assert!(result_b.is_none());
}

#[test]
fn list_pagination() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let tenant_id = TenantId::new("tenant_a").unwrap();

    // Insert 10 chunks with different timestamps
    for i in 0..10 {
        let chunk_id = ChunkId::new();
        let mut metadata = create_test_metadata("tenant_a", &chunk_id);
        metadata.timestamp_created = 1000 + i;
        metadata.ordinal = i as u32;
        // Different segments to avoid UNIQUE constraint on (segment_id, ordinal)
        metadata.segment_id = i as u64;
        store.insert(&metadata).unwrap();
    }

    // List first 5
    let first_page = store.list(&tenant_id, 5, 0).unwrap();
    assert_eq!(first_page.len(), 5);
    // Should be ordered by timestamp DESC
    assert!(first_page[0].timestamp_created > first_page[4].timestamp_created);

    // List next 5
    let second_page = store.list(&tenant_id, 5, 5).unwrap();
    assert_eq!(second_page.len(), 5);

    // First item of second page should be older than last item of first page
    assert!(second_page[0].timestamp_created < first_page[4].timestamp_created);
}

#[test]
fn soft_delete() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let chunk_id = ChunkId::new();
    let tenant_id = TenantId::new("tenant_a").unwrap();
    let metadata = create_test_metadata("tenant_a", &chunk_id);

    store.insert(&metadata).unwrap();

    // Chunk exists before delete
    let before = store.get(&tenant_id, &chunk_id).unwrap();
    assert!(before.is_some());

    // Delete the chunk
    let deleted = store.mark_deleted(&tenant_id, &chunk_id).unwrap();
    assert!(deleted);

    // Chunk not visible after delete
    let after = store.get(&tenant_id, &chunk_id).unwrap();
    assert!(after.is_none());

    // Deleting again returns false
    let deleted_again = store.mark_deleted(&tenant_id, &chunk_id).unwrap();
    assert!(!deleted_again);
}

#[test]
fn count_by_status() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let tenant_id = TenantId::new("tenant_a").unwrap();

    // Insert 5 chunks
    let mut chunk_ids = Vec::new();
    for i in 0..5 {
        let chunk_id = ChunkId::new();
        chunk_ids.push(chunk_id.clone());
        let mut metadata = create_test_metadata("tenant_a", &chunk_id);
        metadata.ordinal = i;
        metadata.segment_id = i as u64;
        store.insert(&metadata).unwrap();
    }

    // Before deletion: 5 active, 0 deleted
    let (active, deleted, candidates) = store.count_by_status(&tenant_id).unwrap();
    assert_eq!(active, 5);
    assert_eq!(deleted, 0);
    assert_eq!(candidates, 0);

    // Delete 2 chunks
    store.mark_deleted(&tenant_id, &chunk_ids[0]).unwrap();
    store.mark_deleted(&tenant_id, &chunk_ids[1]).unwrap();

    // After deletion: 3 active, 2 deleted
    let (active, deleted, candidates) = store.count_by_status(&tenant_id).unwrap();
    assert_eq!(active, 3);
    assert_eq!(deleted, 2);
    assert_eq!(candidates, 0);
}

#[test]
fn health_duplicate_limit_only_limits_examples() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("health.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant_id = TenantId::new("tenant_health").unwrap();

    let texts = [
        "alpha duplicate",
        "alpha duplicate",
        "beta duplicate",
        "beta duplicate",
        "gamma duplicate",
        "gamma duplicate",
        "delta duplicate",
        "delta duplicate",
        "unique text",
    ];
    let mut rows = Vec::new();
    for (idx, text) in texts.iter().enumerate() {
        let chunk_id = ChunkId::new();
        let mut metadata = create_test_metadata("tenant_health", &chunk_id);
        metadata.segment_id = idx as u64 + 1;
        metadata.timestamp_created = 1000 + idx as i64;
        metadata.hash = format!("hash-{idx}");
        metadata.canonical_text = Some((*text).to_string());
        rows.push(metadata);
    }
    store.insert_many(&rows).unwrap();

    let snapshot = store.health_snapshot(&tenant_id, None, 2).unwrap();
    assert_eq!(snapshot.duplicates.unique_text_count, 5);
    assert_eq!(snapshot.duplicates.exact_duplicate_group_count, 4);
    assert_eq!(snapshot.duplicates.duplicate_row_count, 4);
    assert_eq!(snapshot.duplicates.examples.len(), 2);
    assert!((snapshot.duplicates.duplicate_row_ratio - (4.0 / 9.0)).abs() < f64::EPSILON);
    assert!(snapshot.duplicates.duplicate_byte_ratio > 0.0);
}

#[test]
fn get_by_segment() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant_id = TenantId::new("tenant_a").unwrap();

    // Insert chunks in different segments
    for seg in 0..3u64 {
        for ord in 0..3u32 {
            let chunk_id = ChunkId::new();
            let mut metadata = create_test_metadata("tenant_a", &chunk_id);
            metadata.segment_id = seg;
            metadata.ordinal = ord;
            store.insert(&metadata).unwrap();
        }
    }

    // Get chunks from segment 1 (scoped to tenant_a).
    let segment_1_chunks = store.get_by_segment(&tenant_id, 1).unwrap();
    assert_eq!(segment_1_chunks.len(), 3);

    // Verify ordinal ordering
    for (i, chunk) in segment_1_chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal, i as u32);
        assert_eq!(chunk.segment_id, 1);
    }
}

#[test]
fn wal_mode_enabled() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    // Insert something to trigger WAL creation
    let chunk_id = ChunkId::new();
    let metadata = create_test_metadata("test", &chunk_id);
    store.insert(&metadata).unwrap();

    // Database file should exist
    assert!(db_path.exists());

    // Check WAL mode via PRAGMA
    let conn = store.pool.get();
    let journal_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_lowercase(), "wal");
}

#[test]
fn feedback_insert_and_query_roundtrip() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant = TenantId::new("tenant_feedback").unwrap();
    let chunk_id = ChunkId::new();
    let feedback = FeedbackEntry::new(
        tenant.clone(),
        "Find parse config",
        chunk_id.clone(),
        RelevanceLabel::Relevant,
        123456789,
    );

    store.insert_feedback(&feedback).unwrap();

    let loaded = store
        .list_feedback_for_query(&tenant, " find   parse  config ", 10)
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].chunk_id, chunk_id);
    assert_eq!(loaded[0].relevance, RelevanceLabel::Relevant);
}

#[test]
fn index_state_roundtrip_and_counts() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let tenant_id = TenantId::new("tenant_index").unwrap();
    let chunk_id = ChunkId::new();
    let metadata = create_test_metadata("tenant_index", &chunk_id);
    store.insert(&metadata).unwrap();

    let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
    assert_eq!((pending, indexed, failed), (0, 1, 0));

    store
        .mark_index_pending(&tenant_id, std::slice::from_ref(&chunk_id), 101)
        .unwrap();
    let pending_ids = store.list_pending_index_chunk_ids(&tenant_id, 10).unwrap();
    assert_eq!(pending_ids, vec![chunk_id.clone()]);
    let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
    assert_eq!((pending, indexed, failed), (1, 0, 0));

    store
        .mark_index_failed(&tenant_id, &chunk_id, "boom", 102)
        .unwrap();
    let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
    assert_eq!((pending, indexed, failed), (0, 0, 1));

    store
        .mark_indexed(&tenant_id, std::slice::from_ref(&chunk_id), 103)
        .unwrap();
    let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
    assert_eq!((pending, indexed, failed), (0, 1, 0));
}

#[test]
fn task_artifact_bundle_roundtrip() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("task.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    let tenant_id = TenantId::new("tenant_task").unwrap();
    let mut artifact = TaskArtifact::new_task_start(tenant_id.clone());
    artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
    artifact.goal = Some("Profile the perturbation response".to_string());
    artifact.motivation = Some("The regulator remains unresolved".to_string());
    artifact.hypothesis = Some("RpoS drives the induced genes".to_string());
    artifact.scientific_question = Some("Which genes increase after the perturbation?".to_string());
    artifact.dataset_refs = vec![DatasetRef {
        name: "rna_seq".to_string(),
        version: Some("v1".to_string()),
        description: Some("Count matrix".to_string()),
    }];

    store
        .insert_task_artifact_bundle(
            &artifact,
            &["chunk-1".to_string(), "chunk-2".to_string()],
            &["task_goal".to_string(), "task_summary".to_string()],
        )
        .unwrap();

    let loaded = store
        .get_task_artifact(&tenant_id, &artifact.artifact_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.task_id, artifact.task_id);
    assert_eq!(loaded.goal, artifact.goal);
    assert_eq!(loaded.dataset_refs, artifact.dataset_refs);

    let conn = store.pool.get();
    let link_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM artifact_links WHERE artifact_id = ?1",
            rusqlite::params![artifact.artifact_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(link_count, 2);
}

#[test]
fn task_projection_search_filters_by_exact_dimensions() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("task_search.db");
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let tenant_id = TenantId::new("tenant_task").unwrap();
    let chunk_a = ChunkId::new().to_string();
    let chunk_b = ChunkId::new().to_string();

    let mut run_artifact = TaskArtifact::new_run_start(tenant_id.clone(), "task-a");
    run_artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
    run_artifact.status = Some("started".to_string());
    run_artifact.tool_name = Some("mmseqs".to_string());
    run_artifact.dataset_refs = vec![DatasetRef {
        name: "rna_seq".to_string(),
        version: Some("v1".to_string()),
        description: None,
    }];
    store
        .insert_task_artifact_bundle(
            &run_artifact,
            std::slice::from_ref(&chunk_a),
            &["run".to_string()],
        )
        .unwrap();

    let mut other_artifact = TaskArtifact::new_run_start(tenant_id.clone(), "task-b");
    other_artifact.project_id = ProjectId::new(Some("project_beta".to_string()));
    other_artifact.status = Some("started".to_string());
    other_artifact.tool_name = Some("blast".to_string());
    other_artifact.dataset_refs = vec![DatasetRef {
        name: "proteomics".to_string(),
        version: Some("v2".to_string()),
        description: None,
    }];
    store
        .insert_task_artifact_bundle(&other_artifact, &[chunk_b], &["run".to_string()])
        .unwrap();

    let chunk_ids = store
        .search_task_projection_chunk_ids(
            &tenant_id,
            &TaskSearchFilters {
                task_id: Some("task-a".to_string()),
                artifact_kind: Some(ArtifactKind::RunStart),
                status: Some("started".to_string()),
                challenge_id: None,
                thread_id: None,
                reply_to_artifact_id: None,
                artifact_role: None,
                dataset_name: Some("rna_seq".to_string()),
                dataset_version: Some("v1".to_string()),
                entity_name: None,
                entity_type: None,
                tool_name: Some("mmseqs".to_string()),
                project_id: Some("project_alpha".to_string()),
                agent_id: None,
                session_id: None,
                requested_action: None,
                verification_status: None,
                relation_kind: None,
            },
            20,
        )
        .unwrap();

    assert_eq!(chunk_ids.len(), 1);
    assert_eq!(chunk_ids[0].to_string(), chunk_a);
}

#[test]
fn open_migrates_legacy_chunks_schema_with_index_columns() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE chunks (
            chunk_id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            project_id TEXT,
            segment_id INTEGER NOT NULL,
            ordinal INTEGER NOT NULL,
            chunk_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'final',
            timestamp_created INTEGER NOT NULL,
            hash TEXT NOT NULL,
            source_uri TEXT,
            UNIQUE(segment_id, ordinal)
        )",
        [],
    )
    .unwrap();
    drop(conn);

    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let conn = store.pool.get();
    let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
    let rows = stmt
        .query_map([], |row| row.get::<usize, String>(1))
        .unwrap();
    let mut names = std::collections::HashSet::new();
    for row in rows {
        names.insert(row.unwrap());
    }
    assert!(names.contains("index_state"));
    assert!(names.contains("index_attempts"));
    assert!(names.contains("index_last_error"));
    assert!(names.contains("indexed_at_ms"));
    assert!(names.contains("index_updated_at_ms"));
}

#[test]
fn chunks_table_has_lifecycle_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let conn = store.pool.get();
    let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
    let rows = stmt.query_map([], |r| r.get::<usize, String>(1)).unwrap();
    let mut cols: Vec<String> = Vec::new();
    for row in rows {
        cols.push(row.unwrap());
    }
    for c in &[
        "tier",
        "supersedes",
        "superseded_by",
        "expires_at_ms",
        "review_after_ms",
        "lifecycle_updated_at_ms",
        "canonical_text",
    ] {
        assert!(cols.iter().any(|x| x == c), "missing column: {c}");
    }
}

#[test]
fn row_to_metadata_fails_closed_on_unknown_status() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let conn = store.pool.get();
    conn.execute(
        "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status,
                             timestamp_created, hash, tier, lifecycle_updated_at_ms)
         VALUES (?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            "019d0000-0000-7000-8000-000000000001",
            "t",
            0_i64,
            0_i32,
            "doc",
            "bogus_status",
            1_i64,
            "h",
            "long_term",
            0_i64
        ],
    )
    .unwrap();
    let result = store.get(
        &TenantId::new("t").unwrap(),
        &ChunkId::parse("019d0000-0000-7000-8000-000000000001").unwrap(),
    );
    assert!(result.is_err(), "expected error on unknown status");
}

#[test]
fn legacy_db_gains_lifecycle_columns_on_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("m.db");

    // Simulate a DB created before A3 by opening a raw rusqlite connection and
    // creating chunks WITHOUT the 7 new columns.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT
            )",
            [],
        )
        .unwrap();
        // Insert a row with the pre-A3 column set only.
        conn.execute(
            "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status, timestamp_created, hash)
             VALUES (?1, ?2, 0, 0, 'doc', 'final', 1, 'h')",
            rusqlite::params![
                "019d0000-0000-7000-8000-000000000010",
                "t",
            ],
        )
        .unwrap();
    }

    // Now open via SqliteMetadataStore — this should trigger
    // ensure_index_columns to backfill the 7 new columns and
    // indexes.
    let store = SqliteMetadataStore::open(&db_path).unwrap();

    // Verify the columns now exist.
    let conn = store.pool.get();
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
        let rows = stmt.query_map([], |r| r.get::<usize, String>(1)).unwrap();
        let mut names = Vec::new();
        for row in rows {
            names.push(row.unwrap());
        }
        names
    };
    for c in &[
        "tier",
        "supersedes",
        "superseded_by",
        "expires_at_ms",
        "review_after_ms",
        "lifecycle_updated_at_ms",
        "canonical_text",
    ] {
        assert!(
            cols.iter().any(|x| x == c),
            "missing column after migration: {c}"
        );
    }
    drop(conn);

    // Verify the pre-existing row is still readable AND gains default lifecycle.
    let meta = store
        .get(
            &TenantId::new("t").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-000000000010").unwrap(),
        )
        .unwrap()
        .expect("row survives migration");
    assert_eq!(meta.status, ChunkStatus::Final);
    assert_eq!(meta.lifecycle.tier, crate::types::MemoryTier::LongTerm);
    assert_eq!(meta.lifecycle.lifecycle_updated_at_ms, 0);
    assert!(meta.lifecycle.supersedes.is_none());
    assert!(meta.lifecycle.superseded_by.is_none());
    assert!(meta.lifecycle.expires_at_ms.is_none());
    assert!(meta.lifecycle.review_after_ms.is_none());
    assert!(meta.canonical_text.is_none());
}

#[test]
fn legacy_db_gains_consolidation_schema_and_reopens_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pre_consolidation.db");

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks (
                chunk_id, tenant_id, segment_id, ordinal, chunk_type,
                status, timestamp_created, hash
             ) VALUES (?1, 'legacy_tenant', 0, 0, 'doc', 'final', 1, 'h')",
            ["019d0000-0000-7000-8000-000000000011"],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE consolidation_runs (
                 run_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 project_id TEXT,
                 input_hash TEXT NOT NULL,
                 state TEXT NOT NULL,
                 consolidator TEXT NOT NULL,
                 prompt_hash TEXT,
                 response_hash TEXT,
                 validation_result TEXT,
                 error TEXT,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );",
        )
        .unwrap();
    }

    for reopen in 0..2 {
        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let conn = store.pool.get();
        for table in [
            "consolidation_runs",
            "consolidation_entries",
            "memory_lineage",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing {table} after reopen {reopen}");
        }
        for column in [
            "sparse_cleanup_done",
            "consolidator_command",
            "consolidator_model",
            "consolidator_version",
            "audit_artifact_path",
            "promotion_requested",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('consolidation_runs')
                     WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {column} after reopen {reopen}");
        }
        drop(conn);
        drop(store);
    }

    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let legacy = store
        .get(
            &TenantId::new("legacy_tenant").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-000000000011").unwrap(),
        )
        .unwrap()
        .expect("legacy row survives consolidation schema migration");
    assert_eq!(legacy.status, ChunkStatus::Final);
}

// --- Item 2: chunks UNIQUE constraint migrated to tenant-scoped ---

#[test]
fn fresh_db_uses_tenant_scoped_unique_constraint() {
    // Pins the new CREATE TABLE in init_schema: fresh databases
    // are created with UNIQUE(tenant_id, segment_id, ordinal).
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("fresh.db")).unwrap();
    let conn = store.pool.get();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='chunks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("UNIQUE(tenant_id, segment_id, ordinal)"),
        "fresh DB must have tenant-scoped UNIQUE; got: {sql}"
    );
    assert!(
        !sql.contains("UNIQUE(segment_id, ordinal)")
            || sql.contains("UNIQUE(tenant_id, segment_id, ordinal)"),
        "fresh DB must not have the legacy global UNIQUE",
    );
}

#[test]
fn legacy_db_migrates_unique_constraint_to_tenant_scoped() {
    // Simulate a pre-Item-2 database and verify that re-opening it
    // via SqliteMetadataStore rebuilds the chunks table with the
    // tenant-scoped UNIQUE. Also verifies data survives the
    // rebuild.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("legacy_unique.db");

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Legacy shape with the global UNIQUE(segment_id, ordinal).
        conn.execute(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                UNIQUE(segment_id, ordinal)
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status, timestamp_created, hash)
             VALUES (?1, ?2, ?3, ?4, 'doc', 'final', 1, 'h')",
            rusqlite::params![
                "019d0000-0000-7000-8000-0000000000aa",
                "tenant_a",
                1_i64,
                0_i32,
            ],
        )
        .unwrap();
    }

    // Opening via SqliteMetadataStore runs ensure_index_columns,
    // which drives the rebuild migration.
    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let conn = store.pool.get();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='chunks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("UNIQUE(tenant_id, segment_id, ordinal)"),
        "legacy DB must be rebuilt with tenant-scoped UNIQUE; got: {sql}"
    );

    drop(conn);

    // Existing row survives the rebuild.
    let meta = store
        .get(
            &TenantId::new("tenant_a").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-0000000000aa").unwrap(),
        )
        .unwrap()
        .expect("pre-migration row must survive rebuild");
    assert_eq!(meta.segment_id, 1);
    assert_eq!(meta.ordinal, 0);
}

// Item 2 NIT regression — PRAGMA-based migration detection.
//
// The substring check was brittle on DDL variations it couldn't
// easily normalise: `UNIQUE (segment_id, ordinal)` with a space,
// `CONSTRAINT name UNIQUE (...)`, upper-case / mixed-case keyword,
// etc. These tests feed SQLite legacy tables whose CREATE TABLE
// text does not literally contain `UNIQUE(segment_id, ordinal)`,
// verify `detect_chunks_unique_shape` still classifies them as
// `Legacy`, and that the migration fires.
#[test]
fn detect_chunks_unique_shape_recognises_legacy_with_spaced_ddl() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("spaced_legacy.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Note the space between UNIQUE and (.
        conn.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                UNIQUE (segment_id, ordinal)
            );",
        )
        .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&conn).unwrap(),
        ChunksUniqueShape::Legacy,
        "substring match would miss the space; PRAGMA should not"
    );
}

#[test]
fn detect_chunks_unique_shape_recognises_legacy_with_named_constraint() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("named_legacy.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                CONSTRAINT chunks_legacy_uniq UNIQUE(segment_id, ordinal)
            );",
        )
        .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&conn).unwrap(),
        ChunksUniqueShape::Legacy,
        "named UNIQUE constraints must still classify as legacy"
    );
}

#[test]
fn detect_chunks_unique_shape_recognises_tenant_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("tenant_scoped.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                UNIQUE(tenant_id, segment_id, ordinal)
            );",
        )
        .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&conn).unwrap(),
        ChunksUniqueShape::TenantScoped
    );
}

// Codex Item 2 NIT round-1 MEDIUM regression: `CREATE UNIQUE
// INDEX ... (segment_id, ordinal)` is origin='c', not 'u'. It must
// NOT classify as `Legacy` — that would cause
// `migrate_chunks_unique_to_tenant_scoped` to rebuild a foreign
// chunks schema when the owner only wanted a manual unique index.
#[test]
fn detect_chunks_unique_shape_ignores_create_unique_index_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("manual_unique_idx.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Foreign chunks schema with NO table-level UNIQUE
        // constraint, just a manual CREATE UNIQUE INDEX on the
        // columns the legacy-detector looks for.
        conn.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL
            );
             CREATE UNIQUE INDEX chunks_manual_uniq ON chunks(segment_id, ordinal);",
        )
        .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&conn).unwrap(),
        ChunksUniqueShape::Other,
        "CREATE UNIQUE INDEX (origin='c') must NOT classify as Legacy"
    );
}

#[test]
fn detect_chunks_unique_shape_returns_other_for_no_table_or_foreign_schema() {
    // Empty DB → no chunks table at all.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("empty.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&conn).unwrap(),
        ChunksUniqueShape::Other,
        "no chunks table → Other, not a rebuild trigger"
    );

    // Foreign schema: a chunks table exists but its UNIQUE is on
    // unrelated columns. Don't touch.
    let foreign_path = tmp.path().join("foreign.db");
    {
        let c = rusqlite::Connection::open(&foreign_path).unwrap();
        c.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                foreign_field TEXT,
                UNIQUE(foreign_field)
            );",
        )
        .unwrap();
    }
    let foreign_conn = rusqlite::Connection::open(&foreign_path).unwrap();
    assert_eq!(
        detect_chunks_unique_shape(&foreign_conn).unwrap(),
        ChunksUniqueShape::Other,
        "foreign chunks schema with a different UNIQUE must NOT classify as Legacy"
    );
}

#[test]
fn legacy_db_with_spaced_unique_ddl_is_rebuilt_end_to_end() {
    // End-to-end guarantee: a pre-Item-2 DB whose CREATE TABLE
    // text uses `UNIQUE (segment_id, ordinal)` (with a space) is
    // still rebuilt by the PRAGMA-based migration when opened via
    // SqliteMetadataStore. The substring-match version would have
    // missed this and left the legacy constraint live.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("spaced_legacy_end_to_end.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                UNIQUE (segment_id, ordinal)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status, timestamp_created, hash)
             VALUES (?1, ?2, ?3, ?4, 'doc', 'final', 1, 'h')",
            rusqlite::params![
                "019d0000-0000-7000-8000-000000000ac1",
                "tenant_a",
                7_i64,
                3_i32,
            ],
        )
        .unwrap();
    }

    let store = SqliteMetadataStore::open(&db_path).unwrap();
    let conn = store.pool.get();
    let shape = detect_chunks_unique_shape(&conn).unwrap();
    assert_eq!(
        shape,
        ChunksUniqueShape::TenantScoped,
        "spaced legacy DDL must be rebuilt to the tenant-scoped shape"
    );

    // Row survived the rebuild.
    let chunk_id = ChunkId::parse("019d0000-0000-7000-8000-000000000ac1").unwrap();
    let t_a = TenantId::new("tenant_a").unwrap();
    let meta = store.get(&t_a, &chunk_id).unwrap().unwrap();
    assert_eq!(meta.segment_id, 7);
    assert_eq!(meta.ordinal, 3);
}

#[test]
fn get_by_segment_is_tenant_scoped_after_migration() {
    // Regression for Codex Item 2 LOW: once cross-tenant same
    // (segment_id, ordinal) rows coexist, `get_by_segment` must
    // not cross-contaminate. A caller auditing tenant_a's segment
    // must never see tenant_b's rows.
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("scoped.db")).unwrap();

    let make = |tenant: &str, chunk_uuid: &str, seg: u64, ord: u32| {
        let chunk_id = ChunkId::parse(chunk_uuid).unwrap();
        let mut meta = create_test_metadata(tenant, &chunk_id);
        meta.segment_id = seg;
        meta.ordinal = ord;
        store.insert(&meta).unwrap();
    };

    make("tenant_a", "019d0000-0000-7000-8000-000000000a01", 1, 0);
    make("tenant_a", "019d0000-0000-7000-8000-000000000a02", 1, 1);
    make("tenant_b", "019d0000-0000-7000-8000-000000000b01", 1, 0);
    make("tenant_b", "019d0000-0000-7000-8000-000000000b02", 1, 1);

    let t_a = TenantId::new("tenant_a").unwrap();
    let t_b = TenantId::new("tenant_b").unwrap();

    let a_rows = store.get_by_segment(&t_a, 1).unwrap();
    assert_eq!(a_rows.len(), 2);
    for row in &a_rows {
        assert_eq!(row.tenant_id, t_a, "tenant_a query must not leak tenant_b");
    }

    let b_rows = store.get_by_segment(&t_b, 1).unwrap();
    assert_eq!(b_rows.len(), 2);
    for row in &b_rows {
        assert_eq!(row.tenant_id, t_b);
    }
}

#[test]
fn cross_tenant_same_segment_ordinal_coexist() {
    // Regression for Item 2: the legacy global UNIQUE let
    // `INSERT OR REPLACE` silently overwrite tenant_a's
    // (segment_id=1, ordinal=0) when tenant_b's first segment
    // allocated the same pair. The new tenant-scoped UNIQUE means
    // both rows must coexist.
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("mt.db")).unwrap();

    let make = |tenant: &str, chunk_uuid: &str| {
        let chunk_id = ChunkId::parse(chunk_uuid).unwrap();
        let mut meta = create_test_metadata(tenant, &chunk_id);
        meta.segment_id = 1;
        meta.ordinal = 0;
        store.insert(&meta).unwrap();
    };

    make("tenant_a", "019d0000-0000-7000-8000-0000000000a1");
    make("tenant_b", "019d0000-0000-7000-8000-0000000000b1");

    let a = store
        .get(
            &TenantId::new("tenant_a").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-0000000000a1").unwrap(),
        )
        .unwrap()
        .expect("tenant_a row");
    let b = store
        .get(
            &TenantId::new("tenant_b").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-0000000000b1").unwrap(),
        )
        .unwrap()
        .expect("tenant_b row");
    assert_eq!(a.segment_id, b.segment_id);
    assert_eq!(a.ordinal, b.ordinal);
    assert_ne!(a.chunk_id, b.chunk_id);
}

/// Seed a fresh chunk row for the given tenant and return its
/// `ChunkMetadata`. Used by the A4 lifecycle tests to avoid
/// repeating insert boilerplate.
fn seed_chunk(store: &SqliteMetadataStore, tenant: &str) -> ChunkMetadata {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Unique (segment_id, ordinal) pair per call so we do not
    // collide with prior seeds in the same test.
    static NEXT_SEGMENT: AtomicU64 = AtomicU64::new(1_000);
    let segment_id = NEXT_SEGMENT.fetch_add(1, Ordering::SeqCst);

    let chunk_id = ChunkId::new();
    let mut meta = create_test_metadata(tenant, &chunk_id);
    meta.segment_id = segment_id;
    meta.ordinal = 0;
    store.insert(&meta).unwrap();
    meta
}

fn seed_scoped_chunk(
    store: &SqliteMetadataStore,
    tenant: &str,
    project_id: Option<&str>,
    status: ChunkStatus,
) -> ChunkMetadata {
    let mut metadata = seed_chunk(store, tenant);
    let conn = store.pool.get();
    conn.execute(
        "UPDATE chunks SET project_id = ?1, status = ?2 WHERE chunk_id = ?3",
        rusqlite::params![
            project_id,
            status.to_string(),
            metadata.chunk_id.to_string()
        ],
    )
    .unwrap();
    metadata.project_id = project_id.map(str::to_string);
    metadata.status = status;
    metadata
}

fn consolidation_plan(
    tenant_id: &TenantId,
    project_id: Option<&str>,
    input_hash: &str,
    source_id: &ChunkId,
    candidate_id: &ChunkId,
) -> (
    ConsolidationRun,
    Vec<ConsolidationEntryRecord>,
    Vec<MemoryLineage>,
) {
    let run_id = ConsolidationRunId::new();
    let run = ConsolidationRun {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        project_id: project_id.map(str::to_string),
        input_hash: input_hash.to_string(),
        state: ConsolidationState::Planned,
        consolidator: "test".to_string(),
        consolidator_command: Some("internal-test".to_string()),
        consolidator_model: None,
        consolidator_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        prompt_hash: Some("prompt".to_string()),
        response_hash: Some("response".to_string()),
        audit_artifact_path: None,
        validation_result: None,
        error: None,
        sparse_cleanup_done: false,
        promotion_requested: true,
        created_at_ms: 100,
        updated_at_ms: 100,
    };
    let entries = vec![ConsolidationEntryRecord {
        run_id: run_id.clone(),
        entry_index: 0,
        candidate_chunk_id: Some(candidate_id.clone()),
        source_set_hash: format!("source-{source_id}"),
        state: ConsolidationState::Planned,
        validation_error: None,
        created_at_ms: 100,
        updated_at_ms: 100,
    }];
    let lineage = vec![MemoryLineage {
        run_id,
        tenant_id: tenant_id.clone(),
        project_id: project_id.map(str::to_string),
        source_chunk_id: source_id.clone(),
        result_chunk_id: candidate_id.clone(),
        relation: if project_id.is_some() {
            LineageRelation::Supersedes
        } else {
            LineageRelation::DerivesFrom
        },
        created_at_ms: 100,
    }];
    (run, entries, lineage)
}

fn validate_planned_run(store: &SqliteMetadataStore, run_id: &ConsolidationRunId) {
    assert!(store
        .transition_consolidation_run(
            run_id,
            ConsolidationState::Planned,
            ConsolidationState::CandidateWritten,
            200,
            None,
            None,
        )
        .unwrap());
    assert!(store
        .transition_consolidation_run(
            run_id,
            ConsolidationState::CandidateWritten,
            ConsolidationState::Validated,
            300,
            Some("accepted"),
            None,
        )
        .unwrap());
}

#[test]
fn begin_consolidation_run_is_idempotent_by_scope_and_input() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let source = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Final);
    let candidate = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Candidate);
    let (first, entries, lineage) = consolidation_plan(
        &source.tenant_id,
        Some("p"),
        "same-input",
        &source.chunk_id,
        &candidate.chunk_id,
    );
    let created = store
        .begin_consolidation_run(&first, &entries, &lineage)
        .unwrap();
    assert_eq!(created.run_id, first.run_id);

    let other_candidate = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Candidate);
    let (duplicate, duplicate_entries, duplicate_lineage) = consolidation_plan(
        &source.tenant_id,
        Some("p"),
        "same-input",
        &source.chunk_id,
        &other_candidate.chunk_id,
    );
    let reused = store
        .begin_consolidation_run(&duplicate, &duplicate_entries, &duplicate_lineage)
        .unwrap();
    assert_eq!(reused.run_id, first.run_id);
    assert_ne!(reused.run_id, duplicate.run_id);
    assert_eq!(
        store
            .get_consolidation_entries(&first.run_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn project_promotion_is_atomic_and_idempotent() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let source = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Final);
    let candidate = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Candidate);
    let (run, entries, lineage) = consolidation_plan(
        &source.tenant_id,
        Some("p"),
        "project-input",
        &source.chunk_id,
        &candidate.chunk_id,
    );
    store
        .begin_consolidation_run(&run, &entries, &lineage)
        .unwrap();
    validate_planned_run(&store, &run.run_id);

    assert_eq!(
        store
            .atomic_promote_consolidation_run(&run.run_id, 400)
            .unwrap(),
        PromotionOutcome::Committed
    );
    assert_eq!(
        store
            .atomic_promote_consolidation_run(&run.run_id, 500)
            .unwrap(),
        PromotionOutcome::AlreadyCommitted
    );
    let source_after = store
        .get(&source.tenant_id, &source.chunk_id)
        .unwrap()
        .unwrap();
    let candidate_after = store
        .get(&candidate.tenant_id, &candidate.chunk_id)
        .unwrap()
        .unwrap();
    assert_eq!(source_after.status, ChunkStatus::Superseded);
    assert_eq!(
        source_after.lifecycle.superseded_by,
        Some(candidate.chunk_id.clone())
    );
    assert_eq!(candidate_after.status, ChunkStatus::Final);
    assert_eq!(
        store
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Committed
    );
    let pending_cleanup = store
        .list_consolidation_runs_pending_sparse_cleanup(10)
        .unwrap();
    assert_eq!(pending_cleanup.len(), 1);
    assert_eq!(pending_cleanup[0].run_id, run.run_id);
    assert!(store
        .mark_consolidation_sparse_cleanup_done(&run.run_id, 600)
        .unwrap());
    assert!(store
        .list_consolidation_runs_pending_sparse_cleanup(10)
        .unwrap()
        .is_empty());
    assert!(
        store
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .sparse_cleanup_done
    );
}

#[test]
fn tenant_wide_promotion_keeps_project_source_active() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let source = seed_scoped_chunk(&store, "t", Some("owned"), ChunkStatus::Final);
    let candidate = seed_scoped_chunk(&store, "t", None, ChunkStatus::Candidate);
    let (run, entries, lineage) = consolidation_plan(
        &source.tenant_id,
        None,
        "tenant-input",
        &source.chunk_id,
        &candidate.chunk_id,
    );
    store
        .begin_consolidation_run(&run, &entries, &lineage)
        .unwrap();
    validate_planned_run(&store, &run.run_id);
    store
        .atomic_promote_consolidation_run(&run.run_id, 400)
        .unwrap();

    assert_eq!(
        store
            .get(&source.tenant_id, &source.chunk_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
    assert_eq!(
        store
            .get(&candidate.tenant_id, &candidate.chunk_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[test]
fn promotion_conflict_rolls_back_candidate_and_run() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let source = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Final);
    let candidate = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Candidate);
    let (run, entries, lineage) = consolidation_plan(
        &source.tenant_id,
        Some("p"),
        "conflict-input",
        &source.chunk_id,
        &candidate.chunk_id,
    );
    store
        .begin_consolidation_run(&run, &entries, &lineage)
        .unwrap();
    validate_planned_run(&store, &run.run_id);
    store
        .update_lifecycle(
            &source.tenant_id,
            &source.chunk_id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(store
        .atomic_promote_consolidation_run(&run.run_id, 400)
        .is_err());
    assert_eq!(
        store
            .get(&candidate.tenant_id, &candidate.chunk_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Candidate
    );
    assert_eq!(
        store
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Validated
    );
}

#[test]
fn promotion_rejects_source_that_expires_after_validation() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let source = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Final);
    let candidate = seed_scoped_chunk(&store, "t", Some("p"), ChunkStatus::Candidate);
    let (run, entries, lineage) = consolidation_plan(
        &source.tenant_id,
        Some("p"),
        "expired-source-input",
        &source.chunk_id,
        &candidate.chunk_id,
    );
    store
        .begin_consolidation_run(&run, &entries, &lineage)
        .unwrap();
    validate_planned_run(&store, &run.run_id);
    store
        .update_lifecycle(
            &source.tenant_id,
            &source.chunk_id,
            &LifecycleDelta {
                expires_at_ms: Some(Some(350)),
                ..Default::default()
            },
        )
        .unwrap();

    assert!(store
        .atomic_promote_consolidation_run(&run.run_id, 400)
        .is_err());
    assert_eq!(
        store
            .get(&candidate.tenant_id, &candidate.chunk_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Candidate
    );
    assert_eq!(
        store
            .get(&source.tenant_id, &source.chunk_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
    assert_eq!(
        store
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Validated
    );
}

#[test]
fn update_lifecycle_writes_overlay_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let meta = seed_chunk(&store, "t");
    let new_id = ChunkId::new();

    store
        .update_lifecycle(
            &meta.tenant_id,
            &meta.chunk_id,
            &crate::types::LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                superseded_by: Some(new_id.clone()),
                lifecycle_updated_at_ms: Some(1_700_000_000_000),
                ..Default::default()
            },
        )
        .unwrap();

    let reloaded = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
    assert_eq!(reloaded.status, ChunkStatus::Superseded);
    assert_eq!(reloaded.lifecycle.superseded_by.as_ref().unwrap(), &new_id);
    assert_eq!(
        reloaded.lifecycle.lifecycle_updated_at_ms,
        1_700_000_000_000
    );
}

#[test]
fn update_lifecycle_triple_state_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let meta = seed_chunk(&store, "t");

    // Set expires_at_ms via Some(Some(value)).
    store
        .update_lifecycle(
            &meta.tenant_id,
            &meta.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(Some(1_000_000)),
                ..Default::default()
            },
        )
        .unwrap();
    let r1 = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
    assert_eq!(r1.lifecycle.expires_at_ms, Some(1_000_000));

    // Clear expires_at_ms via Some(None).
    store
        .update_lifecycle(
            &meta.tenant_id,
            &meta.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    let r2 = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
    assert!(r2.lifecycle.expires_at_ms.is_none());
}

#[test]
fn atomic_supersede_links_both_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let old = seed_chunk(&store, "t");
    let new = seed_chunk(&store, "t");
    assert_ne!(old.chunk_id, new.chunk_id);

    store
        .atomic_supersede(
            &old.tenant_id,
            &old.chunk_id,
            &new.chunk_id,
            1_800_000_000_000,
        )
        .unwrap();

    let old_r = store.get(&old.tenant_id, &old.chunk_id).unwrap().unwrap();
    assert_eq!(old_r.status, ChunkStatus::Superseded);
    assert_eq!(
        old_r.lifecycle.superseded_by.as_ref().unwrap(),
        &new.chunk_id
    );
    assert_eq!(old_r.lifecycle.lifecycle_updated_at_ms, 1_800_000_000_000);

    let new_r = store.get(&new.tenant_id, &new.chunk_id).unwrap().unwrap();
    assert_eq!(new_r.lifecycle.supersedes.as_ref().unwrap(), &old.chunk_id);
    assert_eq!(new_r.lifecycle.lifecycle_updated_at_ms, 1_800_000_000_000);
}

#[test]
fn list_expired_before_returns_only_old_expiring_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let tenant = TenantId::new("t").unwrap();

    // One expires at 500 (before 1000).
    let early = seed_chunk(&store, "t");
    store
        .update_lifecycle(
            &tenant,
            &early.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(Some(500)),
                ..Default::default()
            },
        )
        .unwrap();

    // One expires at 2000 (after 1000).
    let later = seed_chunk(&store, "t");
    store
        .update_lifecycle(
            &tenant,
            &later.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(Some(2000)),
                ..Default::default()
            },
        )
        .unwrap();

    // One with no expiry.
    let never = seed_chunk(&store, "t");

    let expired = store.list_expired_before(&tenant, 1000).unwrap();
    assert_eq!(
        expired.len(),
        1,
        "only the row expiring before 1000 should appear"
    );
    assert_eq!(expired[0], early.chunk_id);
    assert!(!expired.contains(&later.chunk_id));
    assert!(!expired.contains(&never.chunk_id));
}

#[test]
fn list_by_canonical_text_filters_by_project_and_text() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let tenant = TenantId::new("t").unwrap();

    // Three rows: two share canonical='alpha' but differ by project;
    // one has canonical='beta'.
    let mut a = seed_chunk(&store, "t");
    a.project_id = Some("proj_a".to_string());
    a.canonical_text = Some("alpha".to_string());
    store.insert(&a).unwrap();

    let mut b = seed_chunk(&store, "t");
    b.project_id = Some("proj_b".to_string());
    b.canonical_text = Some("alpha".to_string());
    store.insert(&b).unwrap();

    let mut c = seed_chunk(&store, "t");
    c.project_id = Some("proj_a".to_string());
    c.canonical_text = Some("beta".to_string());
    store.insert(&c).unwrap();

    // Filter by project=proj_a + canonical=alpha → only `a`.
    let scoped = store
        .list_by_canonical_text(&tenant, Some("proj_a"), "alpha")
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].chunk_id, a.chunk_id);

    // Project=None matches across all projects → both `a` and `b`.
    let all_projects = store
        .list_by_canonical_text(&tenant, None, "alpha")
        .unwrap();
    assert_eq!(all_projects.len(), 2);
    let ids: std::collections::HashSet<_> =
        all_projects.iter().map(|m| m.chunk_id.clone()).collect();
    assert!(ids.contains(&a.chunk_id));
    assert!(ids.contains(&b.chunk_id));

    // Non-matching canonical returns empty.
    let empty = store
        .list_by_canonical_text(&tenant, None, "gamma")
        .unwrap();
    assert!(empty.is_empty());
}

#[test]
fn atomic_supersede_rolls_back_when_old_chunk_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let new = seed_chunk(&store, "t");
    let bogus_old = ChunkId::new(); // never inserted

    let result =
        store.atomic_supersede(&new.tenant_id, &bogus_old, &new.chunk_id, 1_900_000_000_000);
    assert!(result.is_err(), "expected error when old chunk missing");

    // The new chunk must be untouched — supersedes should still be None.
    let new_r = store.get(&new.tenant_id, &new.chunk_id).unwrap().unwrap();
    assert!(
        new_r.lifecycle.supersedes.is_none(),
        "new row must not have been updated"
    );
    assert_eq!(
        new_r.lifecycle.lifecycle_updated_at_ms, 0,
        "lifecycle_updated_at_ms must not have been bumped"
    );
}

#[test]
fn atomic_supersede_rolls_back_when_new_chunk_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let old = seed_chunk(&store, "t");
    let bogus_new = ChunkId::new(); // never inserted

    let before = store.get(&old.tenant_id, &old.chunk_id).unwrap().unwrap();

    let result =
        store.atomic_supersede(&old.tenant_id, &old.chunk_id, &bogus_new, 1_900_000_000_000);
    assert!(result.is_err(), "expected error when new chunk missing");

    // The old chunk must be untouched — status unchanged, no bump.
    let old_r = store.get(&old.tenant_id, &old.chunk_id).unwrap().unwrap();
    assert_eq!(
        old_r.status, before.status,
        "old status must not have changed"
    );
    assert!(
        old_r.lifecycle.superseded_by.is_none(),
        "old row must not have gained superseded_by"
    );
    assert_eq!(
        old_r.lifecycle.lifecycle_updated_at_ms,
        before.lifecycle.lifecycle_updated_at_ms
    );
}

#[test]
fn update_lifecycle_triple_state_clear_review_after() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let meta = seed_chunk(&store, "t");

    store
        .update_lifecycle(
            &meta.tenant_id,
            &meta.chunk_id,
            &crate::types::LifecycleDelta {
                review_after_ms: Some(Some(2_000_000)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        store
            .get(&meta.tenant_id, &meta.chunk_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .review_after_ms,
        Some(2_000_000)
    );

    store
        .update_lifecycle(
            &meta.tenant_id,
            &meta.chunk_id,
            &crate::types::LifecycleDelta {
                review_after_ms: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(store
        .get(&meta.tenant_id, &meta.chunk_id)
        .unwrap()
        .unwrap()
        .lifecycle
        .review_after_ms
        .is_none());
}

#[test]
fn list_expired_before_skips_already_expired_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
    let a = seed_chunk(&store, "t");
    let b = seed_chunk(&store, "t");

    // A has expires_at_ms < now AND status=Final: eligible
    store
        .update_lifecycle(
            &a.tenant_id,
            &a.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(Some(500)),
                ..Default::default()
            },
        )
        .unwrap();

    // B has expires_at_ms < now BUT status=Expired already: should be skipped
    store
        .update_lifecycle(
            &b.tenant_id,
            &b.chunk_id,
            &crate::types::LifecycleDelta {
                expires_at_ms: Some(Some(500)),
                status: Some(ChunkStatus::Expired),
                ..Default::default()
            },
        )
        .unwrap();

    let ids = store.list_expired_before(&a.tenant_id, 1_000).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], a.chunk_id);
}
