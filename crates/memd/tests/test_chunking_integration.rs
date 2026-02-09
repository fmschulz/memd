// Quick test to verify PersistentStore chunking works
use memd::store::{PersistentStore, PersistentStoreConfig, Store};
use memd::types::{ChunkType, MemoryChunk, TenantId};
use tempfile::TempDir;

#[tokio::test]
async fn test_persistent_store_chunks_long_document() {
    let temp_dir = TempDir::new().unwrap();
    let mut config = PersistentStoreConfig::default();
    config.data_dir = temp_dir.path().to_path_buf();

    let store = PersistentStore::open(config).unwrap();

    // Create a document > 1000 chars (should trigger chunking)
    let long_text = "A".repeat(1500);
    let tenant_id = TenantId::new("test_tenant").unwrap();
    let chunk = MemoryChunk::new(tenant_id.clone(), &long_text, ChunkType::Code);

    println!("Adding document with {} chars", long_text.len());

    // This should trigger chunking
    let chunk_id = store.add(chunk).await.unwrap();

    println!("Chunk ID returned: {}", chunk_id);

    // Verify it was stored
    let retrieved = store.get(&tenant_id, &chunk_id).await.unwrap();
    assert!(retrieved.is_some(), "Failed to retrieve chunk");

    // Search for the document to see how many chunks we got
    // If chunking worked, we should get multiple results
    let search_results = store.search(&tenant_id, "AAAA", 20).await.unwrap();
    println!("Search returned {} chunks", search_results.len());

    // Check if any have chunk_index tags
    let chunked_results: Vec<_> = search_results
        .iter()
        .filter(|c| c.tags.iter().any(|t| t.starts_with("chunk_index:")))
        .collect();

    println!("Chunks with chunk_index tag: {}", chunked_results.len());

    if chunked_results.is_empty() {
        println!("WARNING: No chunking occurred! Document was stored as single chunk.");
    } else {
        println!(
            "SUCCESS: Chunking worked! Created {} chunks",
            chunked_results.len()
        );
    }

    // For this test, we expect chunking to happen since doc is 1500 chars
    assert!(
        !chunked_results.is_empty(),
        "Expected chunking to occur for 1500 char document"
    );

    println!("Test passed!");
}
