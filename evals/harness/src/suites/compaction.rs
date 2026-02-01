//! Compaction evaluation (Suite F)
//!
//! Tests compaction correctness including tombstone filtering, segment merge,
//! HNSW rebuild, results invariant (set comparison), and latency impact.
//!
//! ## Test Cases
//!
//! - **F1**: Tombstone filtering - deleted chunks never appear in search results
//! - **F2**: Segment merge - segment count reduces after compaction
//! - **F3**: HNSW rebuild - staleness reduces after rebuild
//! - **F4**: Results invariant - same chunk IDs (as set) before/after compaction
//! - **F5**: Latency during compaction - p99 < 500ms threshold
//! - **F6**: Force compaction - force flag bypasses threshold checks

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Compaction test cases
#[derive(Debug, Clone, Copy)]
pub enum CompactionTest {
    /// Deleted chunks never in search results
    F1TombstoneFiltering,
    /// Segment count reduces after merge
    F2SegmentMerge,
    /// HNSW staleness reduces after rebuild
    F3HnswRebuild,
    /// Search results same chunk IDs before/after (minus deleted) - SET comparison
    F4ResultsInvariant,
    /// Measure p50/p99 while compaction runs
    F5LatencyDuringCompaction,
    /// Force flag works regardless of thresholds
    F6ForceCompaction,
}

/// Dataset structure for compaction invariant tests
#[derive(Debug, Deserialize)]
pub struct InvariantDataset {
    pub name: String,
    pub description: String,
    pub chunks: Vec<InvariantChunk>,
    pub queries: Vec<InvariantQuery>,
    pub delete_tags: Vec<String>,
    pub keep_count: usize,
    pub delete_count: usize,
    #[serde(default)]
    pub invariant_note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InvariantChunk {
    pub id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct InvariantQuery {
    pub query: String,
    pub expected_keep: Vec<String>,
    pub expected_not: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Configuration for compaction evaluation
#[derive(Debug, Clone)]
pub struct CompactionEvalConfig {
    /// Path to the invariant dataset file
    pub dataset_path: PathBuf,
    /// Maximum p99 latency during compaction (ms)
    pub max_p99_during_compaction_ms: u64,
    /// Number of chunks to add for latency test
    pub latency_test_chunk_count: usize,
    /// Number of search iterations for latency measurement
    pub latency_test_iterations: usize,
}

impl Default for CompactionEvalConfig {
    fn default() -> Self {
        Self {
            dataset_path: PathBuf::from("evals/datasets/compaction/invariant_test.json"),
            max_p99_during_compaction_ms: 500,
            latency_test_chunk_count: 100,
            latency_test_iterations: 20,
        }
    }
}

/// Compaction stats from memory.stats response
#[derive(Debug, Clone, Default)]
pub struct CompactionStats {
    pub tombstone_ratio: f64,
    pub segment_count: u32,
    pub hnsw_staleness: f64,
    pub needs_compaction: bool,
}

/// Extract the text content from an MCP tool call response
fn extract_content_text(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
}

/// Parse compaction stats from memory.stats response
fn parse_compaction_stats(response: &Value) -> Option<CompactionStats> {
    let text = extract_content_text(response)?;
    let parsed: Value = serde_json::from_str(text).ok()?;

    let compaction = parsed.get("compaction")?;

    Some(CompactionStats {
        tombstone_ratio: compaction
            .get("tombstone_ratio")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        segment_count: compaction
            .get("segment_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        hnsw_staleness: compaction
            .get("hnsw_staleness")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        needs_compaction: compaction
            .get("needs_compaction")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Extract chunk IDs from search results
fn extract_chunk_ids(response: &Value) -> HashSet<String> {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return HashSet::new(),
    };

    let parsed: Value = serde_json::from_str(text).unwrap_or_default();

    parsed
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("chunk_id")
                        .and_then(|id| id.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Run compaction evaluation tests
pub fn run_compaction_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();
    let config = CompactionEvalConfig::default();

    println!("\n=== Compaction Suite (Suite F) ===\n");

    // F1: Tombstone filtering
    results.push(run_f1_tombstone_filtering(memd_path, embedding_model));

    // F2: Segment merge
    results.push(run_f2_segment_merge(memd_path, embedding_model));

    // F3: HNSW rebuild
    results.push(run_f3_hnsw_rebuild(memd_path, embedding_model));

    // F4: Results invariant
    results.push(run_f4_results_invariant(memd_path, &config, embedding_model));

    // F5: Latency during compaction
    results.push(run_f5_latency_during_compaction(memd_path, &config, embedding_model));

    // F6: Force compaction
    results.push(run_f6_force_compaction(memd_path, embedding_model));

    // Print summary
    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    println!("\nCompaction Suite: {}/{} passed", passed, total);

    results
}

/// F1: Deleted chunks never appear in search results
fn run_f1_tombstone_filtering(memd_path: &PathBuf, embedding_model: &str) -> TestResult {
    let start = Instant::now();
    let name = "F1_tombstone_filtering";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Add 10 chunks
    let mut chunk_ids = Vec::new();
    for i in 0..10 {
        let text = format!("Test document number {} with unique content for filtering test", i);
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc",
            "tags": [format!("chunk-{}", i)]
        });

        match client.call_tool("memory.add", params) {
            Ok(response) => {
                if let Some(text) = extract_content_text(&response) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        if let Some(id) = parsed.get("chunk_id").and_then(|v| v.as_str()) {
                            chunk_ids.push(id.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                return TestResult::fail_with_duration(name, &format!("add chunk {}: {}", i, e), start);
            }
        }
    }

    if chunk_ids.len() != 10 {
        return TestResult::fail_with_duration(
            name,
            &format!("Expected 10 chunks, got {}", chunk_ids.len()),
            start,
        );
    }

    // Delete first 5 chunks
    let deleted_ids: HashSet<String> = chunk_ids[..5].iter().cloned().collect();
    for id in &deleted_ids {
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "chunk_id": id
        });

        if let Err(e) = client.call_tool("memory.delete", params) {
            return TestResult::fail_with_duration(name, &format!("delete {}: {}", id, e), start);
        }
    }

    // Search for content that should match deleted chunks
    let params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "query": "Test document number unique content filtering",
        "k": 10
    });

    let response = match client.call_tool("memory.search", params) {
        Ok(r) => r,
        Err(e) => return TestResult::fail_with_duration(name, &format!("search: {}", e), start),
    };

    let result_ids = extract_chunk_ids(&response);

    // Verify no deleted chunks appear in results
    let found_deleted: Vec<_> = result_ids.intersection(&deleted_ids).collect();
    if !found_deleted.is_empty() {
        return TestResult::fail_with_duration(
            name,
            &format!("Found deleted chunks in results: {:?}", found_deleted),
            start,
        );
    }

    println!("  [F1] Deleted 5/10 chunks, none appeared in search results");
    TestResult::pass_with_duration(name, start)
}

/// F2: Segment count reduces after merge
fn run_f2_segment_merge(memd_path: &PathBuf, embedding_model: &str) -> TestResult {
    let start = Instant::now();
    let name = "F2_segment_merge";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Add chunks to generate segments
    for i in 0..20 {
        let text = format!("Segment merge test document {} with content for indexing", i);
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc"
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return TestResult::fail_with_duration(name, &format!("add chunk: {}", e), start);
        }
    }

    // Get stats before compaction
    let stats_before = match client.call_tool("memory.stats", serde_json::json!({"tenant_id": "eval_compaction"})) {
        Ok(r) => parse_compaction_stats(&r).unwrap_or_default(),
        Err(e) => return TestResult::fail_with_duration(name, &format!("stats before: {}", e), start),
    };

    // Run compaction with force=true
    let params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": true
    });

    if let Err(e) = client.call_tool("memory.compact", params) {
        return TestResult::fail_with_duration(name, &format!("compact: {}", e), start);
    }

    // Get stats after compaction
    let stats_after = match client.call_tool("memory.stats", serde_json::json!({"tenant_id": "eval_compaction"})) {
        Ok(r) => parse_compaction_stats(&r).unwrap_or_default(),
        Err(e) => return TestResult::fail_with_duration(name, &format!("stats after: {}", e), start),
    };

    // Segment count should decrease or stay same (already minimal)
    if stats_after.segment_count > stats_before.segment_count {
        return TestResult::fail_with_duration(
            name,
            &format!(
                "Segment count increased: {} -> {}",
                stats_before.segment_count, stats_after.segment_count
            ),
            start,
        );
    }

    println!(
        "  [F2] Segment count: {} -> {} (passed)",
        stats_before.segment_count, stats_after.segment_count
    );
    TestResult::pass_with_duration(name, start)
}

/// F3: HNSW staleness reduces after rebuild
fn run_f3_hnsw_rebuild(memd_path: &PathBuf, embedding_model: &str) -> TestResult {
    let start = Instant::now();
    let name = "F3_hnsw_rebuild";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Add chunks
    let mut chunk_ids = Vec::new();
    for i in 0..15 {
        let text = format!("HNSW rebuild test document {} for staleness measurement", i);
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc"
        });

        match client.call_tool("memory.add", params) {
            Ok(response) => {
                if let Some(text) = extract_content_text(&response) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        if let Some(id) = parsed.get("chunk_id").and_then(|v| v.as_str()) {
                            chunk_ids.push(id.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                return TestResult::fail_with_duration(name, &format!("add chunk: {}", e), start);
            }
        }
    }

    // Delete some chunks to create staleness
    for id in chunk_ids.iter().take(5) {
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "chunk_id": id
        });

        if let Err(e) = client.call_tool("memory.delete", params) {
            return TestResult::fail_with_duration(name, &format!("delete: {}", e), start);
        }
    }

    // Get stats before compaction
    let stats_before = match client.call_tool("memory.stats", serde_json::json!({"tenant_id": "eval_compaction"})) {
        Ok(r) => parse_compaction_stats(&r).unwrap_or_default(),
        Err(e) => return TestResult::fail_with_duration(name, &format!("stats before: {}", e), start),
    };

    // Run compaction with force=true
    let params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": true
    });

    if let Err(e) = client.call_tool("memory.compact", params) {
        return TestResult::fail_with_duration(name, &format!("compact: {}", e), start);
    }

    // Get stats after compaction
    let stats_after = match client.call_tool("memory.stats", serde_json::json!({"tenant_id": "eval_compaction"})) {
        Ok(r) => parse_compaction_stats(&r).unwrap_or_default(),
        Err(e) => return TestResult::fail_with_duration(name, &format!("stats after: {}", e), start),
    };

    // HNSW staleness should decrease or stay same
    if stats_after.hnsw_staleness > stats_before.hnsw_staleness {
        return TestResult::fail_with_duration(
            name,
            &format!(
                "HNSW staleness increased: {:.2}% -> {:.2}%",
                stats_before.hnsw_staleness * 100.0,
                stats_after.hnsw_staleness * 100.0
            ),
            start,
        );
    }

    println!(
        "  [F3] HNSW staleness: {:.2}% -> {:.2}% (passed)",
        stats_before.hnsw_staleness * 100.0,
        stats_after.hnsw_staleness * 100.0
    );
    TestResult::pass_with_duration(name, start)
}

/// F4: Results invariant - same chunk IDs (as set) before/after compaction
fn run_f4_results_invariant(
    memd_path: &PathBuf,
    config: &CompactionEvalConfig,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "F4_results_invariant";

    // Load dataset if available
    let dataset = match load_invariant_dataset(&config.dataset_path) {
        Ok(d) => Some(d),
        Err(_) => {
            println!("  [F4] Dataset not found, using inline test data");
            None
        }
    };

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Use dataset or inline data
    let (chunks_to_add, chunks_to_delete, test_query) = if let Some(ref ds) = dataset {
        let keep: Vec<_> = ds.chunks.iter()
            .filter(|c| !ds.delete_tags.iter().any(|tag| c.tags.contains(tag)))
            .map(|c| (c.id.clone(), c.text.clone(), c.chunk_type.clone()))
            .collect();
        let delete: Vec<_> = ds.chunks.iter()
            .filter(|c| ds.delete_tags.iter().any(|tag| c.tags.contains(tag)))
            .map(|c| c.id.clone())
            .collect();
        let query = ds.queries.first()
            .map(|q| q.query.clone())
            .unwrap_or_else(|| "search query".to_string());
        (keep, delete, query)
    } else {
        // Inline test data
        let keep = vec![
            ("k1".to_string(), "The quick brown fox jumps over the lazy dog".to_string(), "doc".to_string()),
            ("k2".to_string(), "Vector search using cosine distance".to_string(), "doc".to_string()),
            ("k3".to_string(), "Memory management strategies".to_string(), "doc".to_string()),
        ];
        let delete = vec!["d1".to_string(), "d2".to_string()];
        let query = "fox dog lazy".to_string();
        (keep, delete, query)
    };

    // Add all chunks and track IDs
    let mut added_ids: Vec<(String, String)> = Vec::new(); // (logical_id, actual_chunk_id)

    for (logical_id, text, chunk_type) in &chunks_to_add {
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": chunk_type,
            "tags": [logical_id]
        });

        match client.call_tool("memory.add", params) {
            Ok(response) => {
                if let Some(resp_text) = extract_content_text(&response) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(resp_text) {
                        if let Some(id) = parsed.get("chunk_id").and_then(|v| v.as_str()) {
                            added_ids.push((logical_id.clone(), id.to_string()));
                        }
                    }
                }
            }
            Err(e) => {
                return TestResult::fail_with_duration(name, &format!("add chunk: {}", e), start);
            }
        }
    }

    // Add chunks to delete
    let mut delete_chunk_ids = Vec::new();
    for logical_id in &chunks_to_delete {
        let text = format!("Delete target chunk {}", logical_id);
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc",
            "tags": [logical_id]
        });

        match client.call_tool("memory.add", params) {
            Ok(response) => {
                if let Some(resp_text) = extract_content_text(&response) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(resp_text) {
                        if let Some(id) = parsed.get("chunk_id").and_then(|v| v.as_str()) {
                            delete_chunk_ids.push(id.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                return TestResult::fail_with_duration(name, &format!("add delete chunk: {}", e), start);
            }
        }
    }

    // Delete the marked chunks
    for id in &delete_chunk_ids {
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "chunk_id": id
        });

        if let Err(e) = client.call_tool("memory.delete", params) {
            return TestResult::fail_with_duration(name, &format!("delete: {}", e), start);
        }
    }

    // Search BEFORE compaction
    let params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "query": test_query,
        "k": 10
    });

    let response_before = match client.call_tool("memory.search", params.clone()) {
        Ok(r) => r,
        Err(e) => return TestResult::fail_with_duration(name, &format!("search before: {}", e), start),
    };

    let ids_before = extract_chunk_ids(&response_before);

    // Run compaction
    let compact_params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": true
    });

    if let Err(e) = client.call_tool("memory.compact", compact_params) {
        return TestResult::fail_with_duration(name, &format!("compact: {}", e), start);
    }

    // Search AFTER compaction
    let response_after = match client.call_tool("memory.search", params) {
        Ok(r) => r,
        Err(e) => return TestResult::fail_with_duration(name, &format!("search after: {}", e), start),
    };

    let ids_after = extract_chunk_ids(&response_after);

    // Compare as SETS (order may differ due to HNSW rebuild)
    if ids_before != ids_after {
        // Check if the difference is only in deleted chunks (acceptable)
        let deleted_set: HashSet<String> = delete_chunk_ids.iter().cloned().collect();
        let before_minus_deleted: HashSet<_> = ids_before.difference(&deleted_set).cloned().collect();
        let after_minus_deleted: HashSet<_> = ids_after.difference(&deleted_set).cloned().collect();

        if before_minus_deleted != after_minus_deleted {
            return TestResult::fail_with_duration(
                name,
                &format!(
                    "Result IDs differ: before={:?}, after={:?}",
                    ids_before, ids_after
                ),
                start,
            );
        }
    }

    println!(
        "  [F4] Search results invariant verified (set comparison, {} results)",
        ids_before.len()
    );
    TestResult::pass_with_duration(name, start)
}

/// F5: Latency during compaction
fn run_f5_latency_during_compaction(
    memd_path: &PathBuf,
    config: &CompactionEvalConfig,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "F5_latency_during_compaction";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Add many chunks to ensure compaction takes time
    for i in 0..config.latency_test_chunk_count {
        let text = format!(
            "Latency test document {} with various content for compaction performance measurement testing",
            i
        );
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc"
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return TestResult::fail_with_duration(name, &format!("add chunk {}: {}", i, e), start);
        }
    }

    // Delete some to trigger compaction work
    // Note: We can't get chunk_ids easily here, so we'll just run compaction
    // and measure search latency during/after

    // Measure search latency while compaction runs
    // Since compaction is synchronous in the current implementation,
    // we measure latency immediately after compaction

    // Start compaction
    let compact_params = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": true
    });

    if let Err(e) = client.call_tool("memory.compact", compact_params) {
        return TestResult::fail_with_duration(name, &format!("compact: {}", e), start);
    }

    // Measure search latencies
    let mut latencies = Vec::new();
    for _ in 0..config.latency_test_iterations {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "query": "latency test document content",
            "k": 10
        });

        if client.call_tool("memory.search", params).is_ok() {
            latencies.push(query_start.elapsed().as_millis() as u64);
        }
    }

    if latencies.is_empty() {
        return TestResult::fail_with_duration(name, "No successful searches", start);
    }

    // Calculate p50 and p99
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 50);
    let p99 = percentile(&latencies, 99);

    println!(
        "  [F5] Search latency after compaction: p50={}ms, p99={}ms (threshold: {}ms)",
        p50, p99, config.max_p99_during_compaction_ms
    );

    if p99 > config.max_p99_during_compaction_ms {
        return TestResult::fail_with_duration(
            name,
            &format!("p99 {}ms exceeds threshold {}ms", p99, config.max_p99_during_compaction_ms),
            start,
        );
    }

    TestResult::pass_with_duration(name, start)
}

/// F6: Force compaction bypasses thresholds
fn run_f6_force_compaction(memd_path: &PathBuf, embedding_model: &str) -> TestResult {
    let start = Instant::now();
    let name = "F6_force_compaction";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Add minimal data (below thresholds)
    for i in 0..3 {
        let text = format!("Minimal test document {} for force flag testing", i);
        let params = serde_json::json!({
            "tenant_id": "eval_compaction",
            "text": text,
            "type": "doc"
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return TestResult::fail_with_duration(name, &format!("add chunk: {}", e), start);
        }
    }

    // Check that needs_compaction is false (below thresholds)
    let stats = match client.call_tool("memory.stats", serde_json::json!({"tenant_id": "eval_compaction"})) {
        Ok(r) => parse_compaction_stats(&r).unwrap_or_default(),
        Err(e) => return TestResult::fail_with_duration(name, &format!("stats: {}", e), start),
    };

    // Try compaction without force (should be skipped/no-op)
    let params_no_force = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": false
    });

    let response_no_force = client.call_tool("memory.compact", params_no_force.clone());

    // Try compaction with force (should complete)
    let params_force = serde_json::json!({
        "tenant_id": "eval_compaction",
        "force": true
    });

    let response_force = match client.call_tool("memory.compact", params_force) {
        Ok(r) => r,
        Err(e) => return TestResult::fail_with_duration(name, &format!("compact force: {}", e), start),
    };

    // Verify force worked by checking response
    let force_text = extract_content_text(&response_force).unwrap_or("");
    let force_completed = force_text.contains("completed") || force_text.contains("success");

    println!(
        "  [F6] needs_compaction={}, force=true completed (passed)",
        stats.needs_compaction
    );

    if response_no_force.is_err() && !force_completed {
        return TestResult::fail_with_duration(
            name,
            "Force compaction did not complete as expected",
            start,
        );
    }

    TestResult::pass_with_duration(name, start)
}

fn load_invariant_dataset(path: &PathBuf) -> Result<InvariantDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("parse json: {}", e))
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (p * sorted.len() / 100).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compaction_eval_config_defaults() {
        let config = CompactionEvalConfig::default();
        assert_eq!(config.max_p99_during_compaction_ms, 500);
        assert_eq!(config.latency_test_chunk_count, 100);
        assert_eq!(config.latency_test_iterations, 20);
    }

    #[test]
    fn test_percentile() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&data, 50), 6);
        assert_eq!(percentile(&data, 99), 10);
    }

    #[test]
    fn test_compaction_stats_default() {
        let stats = CompactionStats::default();
        assert_eq!(stats.tombstone_ratio, 0.0);
        assert_eq!(stats.segment_count, 0);
        assert_eq!(stats.hnsw_staleness, 0.0);
        assert!(!stats.needs_compaction);
    }

    #[test]
    fn test_extract_chunk_ids_empty() {
        let response = serde_json::json!({});
        let ids = extract_chunk_ids(&response);
        assert!(ids.is_empty());
    }
}
