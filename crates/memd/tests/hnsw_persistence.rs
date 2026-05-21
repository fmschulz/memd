use memd::index::{HnswConfig, HnswIndex};
use memd::types::ChunkId;
use tempfile::TempDir;

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[test]
fn test_hnsw_persistence_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_index");

    let config = HnswConfig {
        max_connections: 16,
        ef_construction: 100,
        ef_search: 50,
        max_elements: 1000,
        dimension: 4,
        persist_graph_dump: true,
    };

    // Create index and insert embeddings
    let index = HnswIndex::with_persistence(config.clone(), &index_path).unwrap();

    let chunk1 = ChunkId::new();
    let chunk2 = ChunkId::new();
    let chunk3 = ChunkId::new();

    let mut emb1 = vec![1.0, 0.0, 0.0, 0.0];
    let mut emb2 = vec![0.0, 1.0, 0.0, 0.0];
    let mut emb3 = vec![0.9, 0.1, 0.0, 0.0];

    normalize(&mut emb1);
    normalize(&mut emb2);
    normalize(&mut emb3);

    index.insert(&chunk1, &emb1).unwrap();
    index.insert(&chunk2, &emb2).unwrap();
    index.insert(&chunk3, &emb3).unwrap();

    // Save index
    index.save().unwrap();

    // Verify search works before reload
    let mut query = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut query);
    let results_before = index.search(&query, 2).unwrap();
    assert_eq!(results_before.len(), 2);
    assert_eq!(results_before[0].chunk_id, chunk1);

    // Drop index to simulate restart
    drop(index);

    // Load index from disk
    let loaded_index = HnswIndex::load(&index_path, config).unwrap();

    // Verify cache was loaded and HNSW rebuilt
    let (cache_size, hnsw_size) = loaded_index.rebuild_stats();
    assert_eq!(cache_size, 3, "Cache should contain 3 embeddings");
    assert_eq!(hnsw_size, 3, "HNSW should contain 3 embeddings");

    // Verify search still works after reload
    let results_after = loaded_index.search(&query, 2).unwrap();
    assert_eq!(results_after.len(), 2);
    assert_eq!(results_after[0].chunk_id, chunk1);

    // Verify results are identical
    assert_eq!(results_before[0].chunk_id, results_after[0].chunk_id);
    assert!((results_before[0].score - results_after[0].score).abs() < 0.001);
}

#[test]
fn test_hnsw_persistence_batch_insert() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_batch");

    let config = HnswConfig {
        dimension: 4,
        ..Default::default()
    };

    let index = HnswIndex::with_persistence(config.clone(), &index_path).unwrap();

    // Insert batch
    let mut items = Vec::new();
    for i in 0..10 {
        let chunk_id = ChunkId::new();
        let mut embedding = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
        normalize(&mut embedding);
        items.push((chunk_id, embedding));
    }

    index.insert_batch(&items).unwrap();
    index.save().unwrap();

    // Reload
    drop(index);
    let loaded = HnswIndex::load(&index_path, config).unwrap();

    let (cache_size, hnsw_size) = loaded.rebuild_stats();
    assert_eq!(cache_size, 10);
    assert_eq!(hnsw_size, 10);
}

#[test]
fn test_hnsw_missing_cache_graceful_fallback() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_missing");

    let config = HnswConfig {
        dimension: 4,
        ..Default::default()
    };

    // Create index without embeddings
    std::fs::create_dir_all(&index_path).unwrap();

    // Create a minimal mapping.json
    let mapping = serde_json::json!({
        "id_to_chunk": {},
        "chunk_to_id": {},
        "next_id": 0,
        "version": 0
    });

    std::fs::write(
        index_path.join("mapping.json"),
        serde_json::to_vec(&mapping).unwrap(),
    )
    .unwrap();

    // Load should succeed with empty cache
    let loaded = HnswIndex::load(&index_path, config).unwrap();
    assert!(loaded.cache_is_empty());
    assert_eq!(loaded.len(), 0);
}

#[test]
fn test_hnsw_corrupted_cache_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_corrupt");

    let config = HnswConfig {
        dimension: 4,
        ..Default::default()
    };

    // Create valid index
    let index = HnswIndex::with_persistence(config.clone(), &index_path).unwrap();
    let chunk = ChunkId::new();
    let mut emb = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut emb);
    index.insert(&chunk, &emb).unwrap();
    index.save().unwrap();
    drop(index);

    // Corrupt the embeddings.bin file
    let cache_path = index_path.join("embeddings.bin");
    let mut data = std::fs::read(&cache_path).unwrap();
    data[10] ^= 0xFF; // Corrupt a byte
    std::fs::write(&cache_path, data).unwrap();

    // Load should succeed but with empty cache (corrupted file deleted)
    let loaded = HnswIndex::load(&index_path, config).unwrap();
    assert!(
        loaded.cache_is_empty(),
        "Cache should be empty after corruption"
    );

    // Cache file should be deleted
    assert!(
        !cache_path.exists(),
        "Corrupted cache file should be deleted"
    );
}

#[test]
fn test_hnsw_dimension_mismatch() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_dim_mismatch");

    // Create index with dimension 4
    let config = HnswConfig {
        dimension: 4,
        ..Default::default()
    };

    let index = HnswIndex::with_persistence(config.clone(), &index_path).unwrap();
    let chunk = ChunkId::new();
    let mut emb = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut emb);
    index.insert(&chunk, &emb).unwrap();
    index.save().unwrap();
    drop(index);

    // Try to load with different dimension
    let wrong_config = HnswConfig {
        dimension: 8,
        ..Default::default()
    };

    let loaded = HnswIndex::load(&index_path, wrong_config).unwrap();

    // Should load but with empty cache (dimension mismatch)
    assert!(
        loaded.cache_is_empty(),
        "Cache should be empty due to dimension mismatch"
    );
}

#[test]
fn test_hnsw_large_index_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_large");

    let config = HnswConfig {
        dimension: 384,
        max_elements: 10000,
        ..Default::default()
    };

    let index = HnswIndex::with_persistence(config.clone(), &index_path).unwrap();

    // Insert 100 embeddings
    for i in 0..100 {
        let chunk = ChunkId::new();
        let mut embedding: Vec<f32> = (0..384).map(|j| (i * 384 + j) as f32).collect();
        normalize(&mut embedding);
        index.insert(&chunk, &embedding).unwrap();
    }

    index.save().unwrap();

    // Reload and verify
    drop(index);
    let loaded = HnswIndex::load(&index_path, config).unwrap();

    let (cache_size, hnsw_size) = loaded.rebuild_stats();
    assert_eq!(cache_size, 100);
    assert_eq!(hnsw_size, 100);

    // Verify search works
    let mut query = vec![0.0; 384];
    query[0] = 1.0;
    normalize(&mut query);
    let results = loaded.search(&query, 5).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn save_after_reload_does_not_create_orphan_snapshots() {
    use std::fs;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");

    let config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        ..Default::default()
    };

    // First cycle: create, populate, save.
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&ChunkId::new(), &emb).unwrap();
        index.save().unwrap();
    }

    // Second cycle: reload, add one more, save again. With hnsw_rs 0.3.3
    // this triggers the unique-basename fallback because load_hnsw sets
    // datamap_opt=true on the reloaded Hnsw, so file_dump refuses to
    // overwrite "graph.hnsw.*" and emits "graph-NNNN.hnsw.*" instead.
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![0.0, 1.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&ChunkId::new(), &emb).unwrap();
        index.save().unwrap();
    }

    // Canonical files = graph.hnsw.{graph,data}. Anything matching
    // graph-*.hnsw.* is a stale snapshot the loader never reads.
    let mut orphans = 0usize;
    for entry in fs::read_dir(&path).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("graph-")
            && (name.ends_with(".hnsw.graph") || name.ends_with(".hnsw.data"))
        {
            orphans += 1;
        }
    }
    assert_eq!(
        orphans, 0,
        "found {} orphan HNSW snapshots in {:?}",
        orphans, path
    );
}

#[test]
fn load_with_partial_canonical_dump_falls_back_to_rebuild() {
    use std::fs;
    use std::io::Write;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");

    let config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        ..Default::default()
    };

    // Seed a working index, save, then corrupt the canonical dump to
    // simulate a process crash that landed mid-`file_dump`.
    let chunk_id = ChunkId::new();
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&chunk_id, &emb).unwrap();
        index.save().unwrap();
    }

    // Truncate-and-garbage both canonical files. hnsw_rs 0.3.3 can panic
    // (not just return Err) on malformed dumps; load_dumped_graph wraps
    // the call in catch_unwind and returns Ok(None) on panic so we fall
    // through to rebuild_graph_from_cache.
    for f in ["graph.hnsw.graph", "graph.hnsw.data"] {
        let mut h = fs::File::create(path.join(f)).unwrap();
        h.write_all(&[0u8; 64]).unwrap();
    }

    // Reload must succeed (no panic propagating, no error returned) and
    // the index must still find the original vector via the cache rebuild.
    let reloaded = HnswIndex::with_persistence(config, &path).unwrap();
    let mut q = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut q);
    let results = reloaded.search(&q, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk_id);
}

#[test]
fn save_without_graph_dump_writes_only_embedding_cache() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");

    let config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        persist_graph_dump: false,
        ..Default::default()
    };

    let chunk_id = ChunkId::new();
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&chunk_id, &emb).unwrap();
        index.save().unwrap();
    }

    assert!(path.join("embeddings.bin").exists());
    assert!(path.join("mapping.bin").exists());
    assert!(
        !path.join("graph.hnsw.graph").exists(),
        "graph dump must not exist when persist_graph_dump=false"
    );
    assert!(!path.join("graph.hnsw.data").exists());

    // Round trip: reload rebuilds from cache and still finds the vector.
    let reloaded = HnswIndex::with_persistence(config, &path).unwrap();
    let mut q = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut q);
    let results = reloaded.search(&q, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk_id);
    assert!(results[0].score > 0.99);
}

#[test]
fn load_ignores_stale_dump_when_persist_graph_dump_disabled() {
    // Toggle scenario: previous run had persist_graph_dump=true and
    // wrote graph.hnsw.*. A subsequent open with persist_graph_dump=false
    // must NOT load that stale dump — otherwise the embedding cache is
    // not the source of truth for read-only workloads.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");

    let dumped_config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        persist_graph_dump: true,
        ..Default::default()
    };
    let live_id = ChunkId::new();
    {
        let index = HnswIndex::with_persistence(dumped_config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&live_id, &emb).unwrap();
        index.save().unwrap();
    }
    assert!(path.join("graph.hnsw.graph").exists());

    // Now invalidate the embedding cache by injecting a different chunk
    // via a flag-disabled save, then drop. The graph dump on disk still
    // references `live_id` only; the cache rebuild would yield the same
    // result. To make a behavioral difference observable, mutate the
    // canonical graph dump bytes so that loading it would either panic
    // or return a different point count than the cache.
    {
        use std::io::Write;
        let mut h = std::fs::OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(path.join("graph.hnsw.data"))
            .unwrap();
        // Corrupt by truncating to a few bytes; with the flag honored
        // we should rebuild from cache and ignore this entirely.
        h.set_len(8).unwrap();
        h.write_all(&[0u8; 8]).unwrap();
    }

    let disabled_config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        persist_graph_dump: false,
        ..Default::default()
    };
    let reloaded = HnswIndex::with_persistence(disabled_config, &path).unwrap();
    // Search must still find the original vector via the rebuilt graph.
    let mut q = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut q);
    let results = reloaded.search(&q, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, live_id);
}

#[test]
fn save_writes_bincode_mapping_not_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");

    let config = HnswConfig {
        dimension: 4,
        max_elements: 100,
        ..Default::default()
    };
    let index = HnswIndex::with_persistence(config, &path).unwrap();
    let mut emb = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut emb);
    index.insert(&ChunkId::new(), &emb).unwrap();
    index.save().unwrap();

    assert!(
        path.join("mapping.bin").exists(),
        "save must write mapping.bin"
    );
    assert!(
        !path.join("mapping.json").exists(),
        "mapping.json must not survive a fresh save under the new format"
    );
}

#[test]
fn load_falls_back_to_legacy_mapping_json() {
    use std::fs;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");
    fs::create_dir_all(&path).unwrap();

    let config = HnswConfig {
        dimension: 4,
        max_elements: 100,
        ..Default::default()
    };

    // Seed a real index, save (produces mapping.bin) — capture the
    // chunk id so we can confirm the legacy-load preserves it.
    let chunk_id = ChunkId::new();
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&chunk_id, &emb).unwrap();
        index.save().unwrap();
    }
    assert!(path.join("mapping.bin").exists());

    // Simulate a legacy install: rename mapping.bin -> mapping.json by
    // copying the serde_json projection of the deserialized mapping.
    let bin_bytes = fs::read(path.join("mapping.bin")).unwrap();
    let (legacy_mapping, _len): (memd::index::hnsw::IndexMapping, _) =
        bincode::serde::decode_from_slice(&bin_bytes, bincode::config::standard()).unwrap();
    fs::remove_file(path.join("mapping.bin")).unwrap();
    fs::write(
        path.join("mapping.json"),
        serde_json::to_vec(&legacy_mapping).unwrap(),
    )
    .unwrap();

    // Reload must accept mapping.json and still find the chunk.
    let reloaded = HnswIndex::with_persistence(config, &path).unwrap();
    assert_eq!(reloaded.len(), 1);
    let mut q = vec![1.0, 0.0, 0.0, 0.0];
    normalize(&mut q);
    let results = reloaded.search(&q, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk_id);
}

#[test]
fn legacy_mapping_json_is_replaced_on_next_save() {
    use std::fs;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("warm_index");
    fs::create_dir_all(&path).unwrap();

    let config = HnswConfig {
        dimension: 4,
        max_elements: 100,
        ..Default::default()
    };

    // Seed via the new save format, then convert to legacy on disk.
    let chunk_id = ChunkId::new();
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&chunk_id, &emb).unwrap();
        index.save().unwrap();
    }
    let bin_bytes = fs::read(path.join("mapping.bin")).unwrap();
    let (legacy_mapping, _len): (memd::index::hnsw::IndexMapping, _) =
        bincode::serde::decode_from_slice(&bin_bytes, bincode::config::standard()).unwrap();
    fs::remove_file(path.join("mapping.bin")).unwrap();
    fs::write(
        path.join("mapping.json"),
        serde_json::to_vec(&legacy_mapping).unwrap(),
    )
    .unwrap();

    // Reopen + save. The legacy file must be gone after the save.
    let index = HnswIndex::with_persistence(config, &path).unwrap();
    index.save().unwrap();
    assert!(path.join("mapping.bin").exists());
    assert!(
        !path.join("mapping.json").exists(),
        "legacy mapping.json must be removed after first save under the new format"
    );
}

#[test]
fn legacy_config_without_persist_graph_dump_round_trips_via_serde() {
    // Simulates loading an HnswConfig serialized by a pre-Phase-2 build:
    // the new persist_graph_dump field is absent and serde must default
    // it to true so the on-disk format stays back-compatible.
    let legacy_json = r#"{
        "max_connections": 16,
        "ef_construction": 200,
        "ef_search": 50,
        "max_elements": 100,
        "dimension": 4
    }"#;
    let config: HnswConfig = serde_json::from_str(legacy_json).unwrap();
    assert!(
        config.persist_graph_dump,
        "missing persist_graph_dump must default to true for back-compat"
    );
}
