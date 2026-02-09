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
