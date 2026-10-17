use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::{ChunkId, ChunkType, MemdError, MemoryChunk, ProjectId, Store, TenantId};

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot(BTreeMap<PathBuf, Option<u64>>);

fn light_config(data_dir: &Path, read_only: bool) -> PersistentStoreConfig {
    PersistentStoreConfig {
        data_dir: data_dir.to_path_buf(),
        read_only,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    }
}

fn finalized_light_config(data_dir: &Path, read_only: bool) -> PersistentStoreConfig {
    PersistentStoreConfig {
        min_finalize_chunks: 1,
        ..light_config(data_dir, read_only)
    }
}

fn chunk(tenant: &TenantId, text: &str) -> MemoryChunk {
    MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
        .with_project(ProjectId::from("ro_project"))
}

fn remove_writer_lock(data_dir: &Path) {
    let path = data_dir.join(".writer.lock");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!("remove {}: {err}", path.display()),
    }
}

fn snapshot_files(data_dir: &Path) -> FileSnapshot {
    let mut files = BTreeMap::new();
    collect_snapshot(data_dir, data_dir, &mut files);
    FileSnapshot(files)
}

fn collect_snapshot(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Option<u64>>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("entry file type");
        if file_type.is_dir() {
            collect_snapshot(root, &path, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.ends_with("-wal") || name.ends_with("-shm") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("relative path")
            .to_path_buf();
        let size = if name == "metadata.db" {
            None
        } else {
            Some(entry.metadata().expect("file metadata").len())
        };
        files.insert(rel, size);
    }
}

#[tokio::test]
async fn read_only_open_missing_data_dir_returns_empty_without_creating_disk_state() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("missing_store");
    let tenant = TenantId::new("readonly_missing").unwrap();

    {
        let store = PersistentStore::open(light_config(&data_dir, true)).unwrap();
        let search = store.search(&tenant, "anything", 10).await.unwrap();
        assert!(search.is_empty());
        let listed = store.list_chunks(&tenant, 10, 0).await.unwrap();
        assert!(listed.is_empty());
    }

    assert!(
        !data_dir.exists(),
        "read-only open of missing data_dir must not create disk state"
    );
}

#[tokio::test]
async fn read_only_store_suppresses_all_disk_mutations() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("store");
    let tenant = TenantId::new("readonly").unwrap();

    let known_id = {
        let store = PersistentStore::open(finalized_light_config(&data_dir, false)).unwrap();
        let first = store
            .add(chunk(&tenant, "readonly alpha token"))
            .await
            .unwrap();
        store
            .add(chunk(&tenant, "readonly beta token"))
            .await
            .unwrap();
        store
            .add(chunk(&tenant, "readonly gamma token"))
            .await
            .unwrap();
        drop(store);
        first
    };
    remove_writer_lock(&data_dir);
    let before = snapshot_files(&data_dir);
    assert!(
        !data_dir.join(".writer.lock").exists(),
        "baseline should not contain writer lock"
    );

    {
        let store = PersistentStore::open(finalized_light_config(&data_dir, true)).unwrap();
        let got = store.get(&tenant, &known_id).await.unwrap();
        assert_eq!(
            got.as_ref().map(|chunk| chunk.text.as_str()),
            Some("readonly alpha token")
        );

        let results = store.search(&tenant, "beta", 10).await.unwrap();
        assert!(results.iter().any(|chunk| chunk.text.contains("beta")));

        let stats = store.stats(&tenant).await.unwrap();
        assert_eq!(stats.active_chunks, 3);

        let err = store
            .add(chunk(&tenant, "readonly mutation should fail"))
            .await
            .unwrap_err();
        assert!(matches!(err, MemdError::ReadOnlyStore { .. }));
    }

    let after = snapshot_files(&data_dir);
    assert_eq!(before, after, "read-only open/read/drop mutated data_dir");
    assert!(
        !data_dir.join(".writer.lock").exists(),
        "read-only open must not create .writer.lock"
    );
}

#[tokio::test]
async fn read_only_open_serves_wal_pending_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("store");
    let tenant = TenantId::new("wal_pending").unwrap();
    let unique = "wal pending unique visible token";

    let chunk_id: ChunkId = {
        let store = PersistentStore::open(light_config(&data_dir, false)).unwrap();
        let id = store.add(chunk(&tenant, unique)).await.unwrap();
        drop(store);
        id
    };
    remove_writer_lock(&data_dir);
    let before = snapshot_files(&data_dir);

    {
        let store = PersistentStore::open(light_config(&data_dir, true)).unwrap();
        let got = store.get(&tenant, &chunk_id).await.unwrap();
        assert_eq!(got.as_ref().map(|chunk| chunk.text.as_str()), Some(unique));

        let listed = store.list_chunks(&tenant, 10, 0).await.unwrap();
        assert!(
            listed.iter().any(|chunk| chunk.chunk_id == chunk_id),
            "list_chunks should include WAL-pending chunk"
        );
    }

    let after = snapshot_files(&data_dir);
    assert_eq!(before, after, "read-only WAL overlay read mutated data_dir");
}

#[tokio::test]
async fn missing_tenant_dir_returns_empty_not_create() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("store");
    let tenant = TenantId::new("present").unwrap();
    let missing = TenantId::new("missing_tenant").unwrap();

    {
        let store = PersistentStore::open(finalized_light_config(&data_dir, false)).unwrap();
        store
            .add(chunk(&tenant, "present tenant data"))
            .await
            .unwrap();
        drop(store);
    }
    remove_writer_lock(&data_dir);
    let before = snapshot_files(&data_dir);

    {
        let store = PersistentStore::open(finalized_light_config(&data_dir, true)).unwrap();
        let search = store.search(&missing, "anything", 10).await.unwrap();
        assert!(search.is_empty());
        let listed = store.list_chunks(&missing, 10, 0).await.unwrap();
        assert!(listed.is_empty());
        let stats = store.stats(&missing).await.unwrap();
        assert_eq!(stats.active_chunks, 0);
        assert_eq!(stats.total_chunks, 0);
    }

    let after = snapshot_files(&data_dir);
    assert_eq!(
        before, after,
        "missing read-only tenant query mutated data_dir"
    );
    assert!(
        !data_dir.join("tenants").join(missing.as_str()).exists(),
        "read-only missing tenant query must not create tenant dir"
    );
}

/// CLI reads against a data dir that cannot be written, as in a Codex sandbox.
///
/// The data dir is made read-only with `chmod`, the CLI binary runs the read
/// commands, and every file under the dir must keep the same bytes.
#[cfg(unix)]
mod cli_reads {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use memd::experience::{
        record, record_check, CheckReceipt, ConditionalLesson, ExperienceContext,
        LessonApplicability, LessonVisibility, RecordEvent, SourceFingerprint,
        SourceFingerprintCoverage,
    };
    use memd::store::persistent::PersistentStore;
    use memd::task_memory::ExecutionContext;
    use memd::{ProjectId, TenantId};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    use super::light_config;

    const TENANT: &str = "cli_readonly";
    const PROJECT: &str = "cli_readonly_project";
    const START_MS: i64 = 1_800_000_000_000;

    /// Make `root` read-only on creation and writable again on drop.
    struct ReadOnlyTree {
        root: PathBuf,
    }

    impl ReadOnlyTree {
        fn new(root: &Path) -> Option<Self> {
            set_write_bits(root, false);
            let probe = root.join("write-probe");
            if std::fs::File::create(&probe).is_ok() {
                // Running as root: permission bits do not stop writes.
                let _ = std::fs::remove_file(probe);
                set_write_bits(root, true);
                return None;
            }
            Some(Self {
                root: root.to_path_buf(),
            })
        }
    }

    impl Drop for ReadOnlyTree {
        fn drop(&mut self) {
            set_write_bits(&self.root, true);
        }
    }

    fn set_write_bits(path: &Path, writable: bool) {
        let metadata = std::fs::symlink_metadata(path).expect("stat");
        if metadata.file_type().is_symlink() {
            return;
        }
        let mode = metadata.permissions().mode();
        let mode = if writable {
            mode | 0o200
        } else {
            mode & !0o222
        };
        // Restore the dir bit first so its children can be reached and changed.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).expect("read dir") {
                set_write_bits(&entry.expect("entry").path(), writable);
            }
        }
    }

    /// Map every file to its SHA-256 and every dir to a marker.
    fn snapshot_bytes(root: &Path) -> BTreeMap<PathBuf, String> {
        fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, String>) {
            for entry in std::fs::read_dir(path).expect("read dir") {
                let path = entry.expect("entry").path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    out.insert(relative, "<dir>".to_string());
                    walk(root, &path, out);
                } else {
                    let bytes = std::fs::read(&path).expect("read file");
                    out.insert(relative, format!("{:x}", Sha256::digest(bytes)));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn memd(data_dir: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_memd"));
        command
            .env("MEMD_EMBED_DEVICE", "cpu")
            .env_remove("MEMD_WARM_ALLOW_EPHEMERAL")
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--search-variant", "bm25-only"]);
        command
    }

    fn success(mut command: Command, label: &str) -> Output {
        let output = command.output().expect(label);
        assert!(
            output.status.success(),
            "{label} failed\nstderr:\n{}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        output
    }

    /// A read command on an unwritable dir falls back to the cold path without
    /// a warm-worker warning.
    fn success_quietly(command: Command, label: &str) -> Output {
        let output = success(command, label);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("warm worker"),
            "{label} warned about the warm worker:\n{stderr}"
        );
        output
    }

    fn stdout_of(output: &Output) -> String {
        String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
    }

    fn add_chunk(data_dir: &Path, text: &str) {
        let mut command = memd(data_dir);
        command
            .args(["add", "--tenant-id", TENANT, "--project-id", PROJECT])
            .args(["--chunk-type", "doc", "--warm", "off", "--text", text]);
        success(command, "add chunk");
    }

    fn context(time: i64) -> ExperienceContext {
        ExperienceContext {
            tenant_id: TenantId::new(TENANT).unwrap(),
            project_id: ProjectId::from(PROJECT),
            agent_id: Some("read-only-test".to_string()),
            session_id: Some("session-ro".to_string()),
            observed_at_ms: Some(time),
            provenance: Default::default(),
        }
    }

    fn receipt(attempt_id: &str) -> CheckReceipt {
        let fingerprint = SourceFingerprint {
            sha256: "c".repeat(64),
            coverage: SourceFingerprintCoverage::TrackedRepository,
        };
        CheckReceipt {
            attempt_id: attempt_id.to_string(),
            cwd: "/tmp/project".to_string(),
            argv: vec!["cargo".to_string(), "check".to_string()],
            started_at_ms: START_MS + 2,
            finished_at_ms: START_MS + 3,
            timed_out: false,
            exit_code: Some(0),
            stdout_sha256: Some("a".repeat(64)),
            stderr_sha256: Some("b".repeat(64)),
            source_before: Some(fingerprint.clone()),
            source_after: Some(fingerprint),
            execution: ExecutionContext::default(),
        }
    }

    /// Record one problem, attempt, check, and shared lesson. Returns the problem ID.
    async fn record_case(store: &PersistentStore) -> String {
        let problem = record(
            store,
            context(START_MS),
            RecordEvent::Problem {
                description: "cargo registry index is read-only".to_string(),
                symptom_terms: vec!["cargo registry read-only".to_string()],
            },
        )
        .await
        .unwrap();
        let attempt = record(
            store,
            context(START_MS + 1),
            RecordEvent::Attempt {
                problem_id: problem.artifact_id.clone(),
                previous_attempt_id: None,
                action: "set CARGO_HOME to a writable directory".to_string(),
            },
        )
        .await
        .unwrap();
        let check = record_check(store, context(START_MS + 4), receipt(&attempt.artifact_id))
            .await
            .unwrap();
        record(
            store,
            context(START_MS + 5),
            RecordEvent::Lesson(ConditionalLesson {
                problem_id: problem.artifact_id.clone(),
                supporting_check_id: check.artifact_id,
                guidance: "set CARGO_HOME to a writable directory".to_string(),
                symptom_terms: vec!["cargo registry read-only".to_string()],
                applicability: LessonApplicability {
                    tool: Some("cargo".to_string()),
                    ..Default::default()
                },
                visibility: LessonVisibility::Shared,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        problem.artifact_id
    }

    /// Run every read command and require success plus unchanged bytes.
    fn assert_reads_leave_store_untouched(data_dir: &Path, problem_id: &str) {
        let Some(_read_only) = ReadOnlyTree::new(data_dir) else {
            eprintln!("skipping: running as root, chmod cannot make the store read-only");
            return;
        };
        let before = snapshot_bytes(data_dir);

        let mut search = memd(data_dir);
        search.args(["search", "--tenant-id", TENANT, "--project-id", PROJECT]);
        search.args(["--k", "3", "readonly alpha"]);
        let output = success_quietly(search, "search");
        assert!(
            stdout_of(&output).contains("readonly alpha token"),
            "search must return the stored chunk: {}",
            stdout_of(&output)
        );

        let mut agent_context = memd(data_dir);
        agent_context.args([
            "agent-context",
            "--tenant-id",
            TENANT,
            "--project-id",
            PROJECT,
        ]);
        agent_context.args(["--query", "readonly alpha"]);
        let output = success_quietly(agent_context, "agent-context");
        assert!(stdout_of(&output).contains("readonly alpha token"));

        let experience = |args: &[&str]| {
            let mut command = memd(data_dir);
            command.args(["experience", "--tenant-id", TENANT, "--project-id", PROJECT]);
            command.args(args);
            command
        };
        let output = success_quietly(
            experience(&["find", "cargo registry read-only", "--tool", "cargo"]),
            "experience find",
        );
        let found: Value = serde_json::from_str(&stdout_of(&output)).expect("find JSON");
        assert_eq!(found["status"], "matches", "{found}");
        assert!(found.to_string().contains(problem_id));

        let output = success_quietly(experience(&["get", problem_id]), "experience get");
        let case: Value = serde_json::from_str(&stdout_of(&output)).expect("get JSON");
        assert_eq!(case["problem"]["artifact_id"], problem_id);

        let output = success_quietly(experience(&["export", problem_id]), "experience export");
        let bundle: Value = serde_json::from_str(&stdout_of(&output)).expect("export JSON");
        assert_eq!(bundle["format"], "memd-experience");

        assert_eq!(
            before,
            snapshot_bytes(data_dir),
            "read commands changed files under the data dir"
        );
    }

    async fn populated_store(data_dir: &Path) -> String {
        add_chunk(data_dir, "readonly alpha token");
        let store = PersistentStore::open(light_config(data_dir, false)).unwrap();
        record_case(&store).await
    }

    #[tokio::test]
    async fn cli_reads_work_on_unwritable_dir_with_sidecars_left_by_exited_writers() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("store");
        let problem_id = populated_store(&data_dir).await;
        let _ = std::process::Command::new("cp").arg("-r").arg(&data_dir).arg("/tmp/claude-1000/-home-fschulz-dev-memd/be18d679-e126-4ba4-984f-cb316871239a/scratchpad/dbg").status();
        eprintln!("PROBLEM {problem_id}");
        assert_reads_leave_store_untouched(&data_dir, &problem_id);
    }

    #[tokio::test]
    async fn cli_reads_work_on_unwritable_dir_without_wal_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("store");
        let problem_id = populated_store(&data_dir).await;
        // Fold the WAL into the main file, then remove the sidecars, as after
        // the last connection of a database closes cleanly.
        let connection = rusqlite::Connection::open(data_dir.join("metadata.db")).unwrap();
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        drop(connection);
        for sidecar in ["metadata.db-wal", "metadata.db-shm"] {
            let _ = std::fs::remove_file(data_dir.join(sidecar));
        }
        assert_reads_leave_store_untouched(&data_dir, &problem_id);
        assert!(!data_dir.join("metadata.db-wal").exists());
        assert!(!data_dir.join("metadata.db-shm").exists());
    }

    #[tokio::test]
    async fn cli_reads_see_wal_content_of_a_live_writer_on_unwritable_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("store");
        add_chunk(&data_dir, "readonly alpha token");
        let store = PersistentStore::open(light_config(&data_dir, false)).unwrap();
        let problem_id = record_case(&store).await;
        assert!(
            std::fs::metadata(data_dir.join("metadata.db-wal")).is_ok_and(|m| m.len() > 0),
            "the live writer must hold uncheckpointed WAL content"
        );
        assert_reads_leave_store_untouched(&data_dir, &problem_id);
        drop(store);
    }
}
