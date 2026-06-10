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
