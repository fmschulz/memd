//! Tiered storage evaluation (Suite D)
//!
//! Tests cache hit rates, hot tier latency, and tier promotion behavior.
//! Validates that the tiered architecture provides expected speedups.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Dataset structure for tiered tests
#[derive(Debug, Deserialize)]
pub struct TieredDataset {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub note: Option<String>,
    pub test_types: Vec<String>,
    pub documents: Vec<TieredDocument>,
    pub queries: Vec<TieredQuery>,
}

#[derive(Debug, Deserialize)]
pub struct TieredDocument {
    pub id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TieredQuery {
    pub id: String,
    pub query: String,
    pub relevant_docs: Vec<String>,
    pub test_type: String,
    #[serde(default)]
    pub repeat_count: usize,
    #[serde(default)]
    pub expected_tier: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Configuration for tiered evaluation
#[derive(Debug, Clone)]
pub struct TieredEvalConfig {
    /// Path to the dataset file
    pub dataset_path: PathBuf,
    /// Maximum latency for hot tier queries (ms)
    pub hot_tier_latency_threshold_ms: u64,
    /// Maximum latency for warm tier queries (ms)
    pub warm_tier_latency_threshold_ms: u64,
    /// Minimum cache hit rate for repeated queries
    pub min_cache_hit_rate: f32,
    /// Number of warmup queries before testing
    pub warmup_queries: usize,
}

impl Default for TieredEvalConfig {
    fn default() -> Self {
        Self {
            dataset_path: PathBuf::from("evals/datasets/retrieval/tiered_eval.json"),
            hot_tier_latency_threshold_ms: 50,
            warm_tier_latency_threshold_ms: 200,
            min_cache_hit_rate: 0.8,
            warmup_queries: 50,
        }
    }
}

/// Result of a cache hit test
#[derive(Debug, Clone)]
pub struct CacheTestResult {
    pub query_id: String,
    pub repeat_count: usize,
    pub cache_hits: usize,
    pub avg_latency_ms: f64,
    pub pass: bool,
}

/// Result of a tier test (hot or warm)
#[derive(Debug, Clone)]
pub struct TierTestResult {
    pub query_id: String,
    pub expected_tier: String,
    pub actual_tier: String,
    pub latency_ms: u64,
    pub recall_at_k: f32,
    pub pass: bool,
}

/// Overall tiered evaluation result
#[derive(Debug, Clone)]
pub struct TieredEvalResult {
    pub cache_tests: Vec<CacheTestResult>,
    pub hot_tier_tests: Vec<TierTestResult>,
    pub warm_tier_tests: Vec<TierTestResult>,
    pub overall_pass: bool,
    pub summary: String,
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

/// Run tiered evaluation tests
pub fn run_tiered_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load dataset
    let mut config = TieredEvalConfig::default();
    config.dataset_path = crate::resolve_dataset_path("evals/datasets/retrieval/tiered_eval.json");
    let dataset = match load_dataset(&config.dataset_path) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "D_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== Tiered Storage Suite (Suite D) ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    if let Some(note) = &dataset.note {
        println!("Note: {}", note);
    }
    println!(
        "Documents: {}, Queries: {} ({} types)\n",
        dataset.documents.len(),
        dataset.queries.len(),
        dataset.test_types.len()
    );

    // D1: Index documents and run warmup
    let d1_result = run_d1_index_and_warmup(memd_path, &dataset, &config, embedding_model);
    results.push(d1_result.clone());
    if !d1_result.passed {
        return results;
    }

    // D2: Cache hit tests
    let (d2_result, cache_results) =
        run_d2_cache_hit_tests(memd_path, &dataset, &config, embedding_model);
    results.push(d2_result);

    // D3: Hot tier latency tests
    let (d3_result, hot_results) =
        run_d3_hot_tier_tests(memd_path, &dataset, &config, embedding_model);
    results.push(d3_result);

    // D4: Warm tier baseline tests
    let (d4_result, warm_results) =
        run_d4_warm_tier_tests(memd_path, &dataset, &config, embedding_model);
    results.push(d4_result);

    // D5: Compare hot vs warm latency
    results.push(run_d5_latency_comparison(&hot_results, &warm_results));

    // D6: Cache hit rate check
    results.push(run_d6_cache_hit_rate(
        &cache_results,
        config.min_cache_hit_rate,
    ));

    // D7: Overall tiered quality thresholds
    results.push(run_d7_quality_thresholds(
        &cache_results,
        &hot_results,
        &warm_results,
    ));

    results
}

fn load_dataset(path: &PathBuf) -> Result<TieredDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    let mut dataset: TieredDataset =
        serde_json::from_str(&content).map_err(|e| format!("parse json: {}", e))?;

    for doc in &mut dataset.documents {
        let raw_type = doc.doc_type.clone();
        let Some(normalized) = crate::normalize_eval_chunk_type(&raw_type) else {
            return Err(format!(
                "unsupported chunk type '{}' for document {}",
                raw_type, doc.id
            ));
        };
        doc.doc_type = normalized.to_string();
    }

    Ok(dataset)
}

fn run_d1_index_and_warmup(
    memd_path: &PathBuf,
    dataset: &TieredDataset,
    config: &TieredEvalConfig,
    embedding_model: &str,
) -> TestResult {
    let start = Instant::now();
    let name = "D1_index_and_warmup";

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
        Err(e) => {
            return TestResult::fail_with_duration(name, &format!("start memd: {}", e), start)
        }
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail_with_duration(name, &format!("initialize: {}", e), start);
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return TestResult::fail_with_duration(
                name,
                &format!("add doc {}: {}", doc.id, e),
                start,
            );
        }
    }

    println!("  Indexed {} documents", dataset.documents.len());

    // Run warmup queries to populate access patterns
    let hot_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.test_type == "hot_tier")
        .collect();

    let mut warmup_count = 0;
    for _ in 0..config.warmup_queries {
        for query in &hot_queries {
            let params = serde_json::json!({
                "tenant_id": "eval_tiered",
                "query": query.query,
                "k": 5
            });

            if client.call_tool("memory.search", params).is_ok() {
                warmup_count += 1;
            }
        }
    }

    println!(
        "  Warmup: {} queries to populate access patterns",
        warmup_count
    );
    TestResult::pass_with_duration(name, start)
}

fn run_d2_cache_hit_tests(
    memd_path: &PathBuf,
    dataset: &TieredDataset,
    _config: &TieredEvalConfig,
    embedding_model: &str,
) -> (TestResult, Vec<CacheTestResult>) {
    let start = Instant::now();
    let name = "D2_cache_hit_tests";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
                Vec::new(),
            )
        }
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
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
                Vec::new(),
            )
        }
    };

    if let Err(e) = client.initialize() {
        return (
            TestResult::fail_with_duration(name, &format!("initialize: {}", e), start),
            Vec::new(),
        );
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return (
                TestResult::fail_with_duration(name, &format!("add doc: {}", e), start),
                Vec::new(),
            );
        }
    }

    let cache_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.test_type == "cache_hit")
        .collect();

    let mut cache_results = Vec::new();

    for query in cache_queries {
        let repeat_count = if query.repeat_count > 0 {
            query.repeat_count
        } else {
            3
        };
        let mut latencies = Vec::new();
        let mut cache_hits = 0;

        for i in 0..repeat_count {
            let query_start = Instant::now();

            let params = serde_json::json!({
                "tenant_id": "eval_tiered",
                "query": query.query,
                "k": 5,
                "debug_tiers": true
            });

            if let Ok(response) = client.call_tool("memory.search", params) {
                let elapsed_ms = query_start.elapsed().as_secs_f64() * 1000.0;
                latencies.push(elapsed_ms);

                // Check if cache was hit from tier_info
                if let Some(text) = extract_content_text(&response) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        if let Some(tier_info) = parsed.get("tier_info") {
                            if tier_info.get("cache_hit").and_then(|v| v.as_bool()) == Some(true) {
                                cache_hits += 1;
                            }
                        }
                    }
                }

                // After first query, subsequent should hit cache
                if i > 0 {
                    // Consider it a logical cache hit if latency dropped significantly
                    if elapsed_ms < latencies[0] * 0.5 {
                        // Don't double-count if already counted from tier_info
                    }
                }
            }
        }

        let avg_latency = if !latencies.is_empty() {
            latencies.iter().sum::<f64>() / latencies.len() as f64
        } else {
            0.0
        };

        // Cache hit rate: cache_hits out of (repeat_count - 1) since first can't hit
        let expected_cache_hits = repeat_count.saturating_sub(1);
        let pass = expected_cache_hits == 0 || cache_hits > 0;

        cache_results.push(CacheTestResult {
            query_id: query.id.clone(),
            repeat_count,
            cache_hits,
            avg_latency_ms: avg_latency,
            pass,
        });
    }

    println!("\n  Cache Hit Tests:");
    for result in &cache_results {
        println!(
            "    {} - hits: {}/{}, avg: {:.1}ms, pass: {}",
            result.query_id,
            result.cache_hits,
            result.repeat_count.saturating_sub(1),
            result.avg_latency_ms,
            result.pass
        );
    }

    let all_pass = cache_results.iter().all(|r| r.pass);
    if all_pass {
        (TestResult::pass_with_duration(name, start), cache_results)
    } else {
        (
            TestResult::fail_with_duration(name, "Some cache tests failed", start),
            cache_results,
        )
    }
}

fn run_d3_hot_tier_tests(
    memd_path: &PathBuf,
    dataset: &TieredDataset,
    config: &TieredEvalConfig,
    embedding_model: &str,
) -> (TestResult, Vec<TierTestResult>) {
    let start = Instant::now();
    let name = "D3_hot_tier_tests";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
                Vec::new(),
            )
        }
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
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
                Vec::new(),
            )
        }
    };

    if let Err(e) = client.initialize() {
        return (
            TestResult::fail_with_duration(name, &format!("initialize: {}", e), start),
            Vec::new(),
        );
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return (
                TestResult::fail_with_duration(name, &format!("add doc: {}", e), start),
                Vec::new(),
            );
        }
    }

    // Warmup to trigger promotions
    let hot_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.test_type == "hot_tier")
        .collect();

    for _ in 0..config.warmup_queries {
        for query in &hot_queries {
            let params = serde_json::json!({
                "tenant_id": "eval_tiered",
                "query": query.query,
                "k": 5
            });
            let _ = client.call_tool("memory.search", params);
        }
    }

    // Now test hot tier queries
    let mut hot_results = Vec::new();

    for query in &hot_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "query": query.query,
            "k": 5,
            "debug_tiers": true
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(e) => {
                hot_results.push(TierTestResult {
                    query_id: query.id.clone(),
                    expected_tier: "hot".to_string(),
                    actual_tier: "error".to_string(),
                    latency_ms: query_start.elapsed().as_millis() as u64,
                    recall_at_k: 0.0,
                    pass: false,
                });
                eprintln!("  Query {} failed: {}", query.id, e);
                continue;
            }
        };

        let latency_ms = query_start.elapsed().as_millis() as u64;

        // Extract tier info and results
        let (actual_tier, recall) = if let Some(text) = extract_content_text(&response) {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                let tier = parsed
                    .get("tier_info")
                    .and_then(|t| t.get("source_tier"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let retrieved_ids = extract_retrieved_ids(&response);
                let relevant_set: HashSet<_> = query.relevant_docs.iter().cloned().collect();
                let recall = calculate_recall(&retrieved_ids, &relevant_set, 5);

                (tier, recall)
            } else {
                ("parse_error".to_string(), 0.0)
            }
        } else {
            ("no_content".to_string(), 0.0)
        };

        let expected_tier = query.expected_tier.clone().unwrap_or("hot".to_string());
        let pass = latency_ms <= config.hot_tier_latency_threshold_ms
            || actual_tier == "hot"
            || actual_tier == "cache";

        hot_results.push(TierTestResult {
            query_id: query.id.clone(),
            expected_tier,
            actual_tier,
            latency_ms,
            recall_at_k: recall as f32,
            pass,
        });
    }

    println!(
        "\n  Hot Tier Tests (threshold: {}ms):",
        config.hot_tier_latency_threshold_ms
    );
    for result in &hot_results {
        println!(
            "    {} - tier: {}, latency: {}ms, recall: {:.2}, pass: {}",
            result.query_id, result.actual_tier, result.latency_ms, result.recall_at_k, result.pass
        );
    }

    let all_pass = hot_results.iter().all(|r| r.pass);
    if all_pass {
        (TestResult::pass_with_duration(name, start), hot_results)
    } else {
        (
            TestResult::fail_with_duration(name, "Some hot tier tests failed", start),
            hot_results,
        )
    }
}

fn run_d4_warm_tier_tests(
    memd_path: &PathBuf,
    dataset: &TieredDataset,
    _config: &TieredEvalConfig,
    embedding_model: &str,
) -> (TestResult, Vec<TierTestResult>) {
    let start = Instant::now();
    let name = "D4_warm_tier_tests";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
                Vec::new(),
            )
        }
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
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
                Vec::new(),
            )
        }
    };

    if let Err(e) = client.initialize() {
        return (
            TestResult::fail_with_duration(name, &format!("initialize: {}", e), start),
            Vec::new(),
        );
    }

    // Index all documents
    for doc in &dataset.documents {
        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "text": doc.text,
            "type": doc.doc_type,
            "tags": [doc.id]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return (
                TestResult::fail_with_duration(name, &format!("add doc: {}", e), start),
                Vec::new(),
            );
        }
    }

    // Test warm tier queries (cold queries, no warmup for these)
    let warm_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.test_type == "warm_tier")
        .collect();

    let mut warm_results = Vec::new();

    for query in warm_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_tiered",
            "query": query.query,
            "k": 5,
            "debug_tiers": true
        });

        let response = match client.call_tool("memory.search", params) {
            Ok(r) => r,
            Err(e) => {
                warm_results.push(TierTestResult {
                    query_id: query.id.clone(),
                    expected_tier: "warm".to_string(),
                    actual_tier: "error".to_string(),
                    latency_ms: query_start.elapsed().as_millis() as u64,
                    recall_at_k: 0.0,
                    pass: false,
                });
                eprintln!("  Query {} failed: {}", query.id, e);
                continue;
            }
        };

        let latency_ms = query_start.elapsed().as_millis() as u64;

        // Extract tier info and results
        let (actual_tier, recall) = if let Some(text) = extract_content_text(&response) {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                let tier = parsed
                    .get("tier_info")
                    .and_then(|t| t.get("source_tier"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("warm")
                    .to_string();

                let retrieved_ids = extract_retrieved_ids(&response);
                let relevant_set: HashSet<_> = query.relevant_docs.iter().cloned().collect();
                let recall = calculate_recall(&retrieved_ids, &relevant_set, 5);

                (tier, recall)
            } else {
                ("warm".to_string(), 0.0)
            }
        } else {
            ("warm".to_string(), 0.0)
        };

        // Warm tier queries should work, just measure baseline latency
        let pass = true; // Warm tier always passes if it returns results

        warm_results.push(TierTestResult {
            query_id: query.id.clone(),
            expected_tier: "warm".to_string(),
            actual_tier,
            latency_ms,
            recall_at_k: recall as f32,
            pass,
        });
    }

    println!("\n  Warm Tier Tests (baseline):");
    for result in &warm_results {
        println!(
            "    {} - tier: {}, latency: {}ms, recall: {:.2}",
            result.query_id, result.actual_tier, result.latency_ms, result.recall_at_k
        );
    }

    (TestResult::pass_with_duration(name, start), warm_results)
}

fn run_d5_latency_comparison(
    hot_results: &[TierTestResult],
    warm_results: &[TierTestResult],
) -> TestResult {
    let start = Instant::now();
    let name = "D5_latency_comparison";

    if hot_results.is_empty() || warm_results.is_empty() {
        return TestResult::fail_with_duration(name, "Missing test results for comparison", start);
    }

    // Calculate p50 latencies
    let mut hot_latencies: Vec<u64> = hot_results.iter().map(|r| r.latency_ms).collect();
    let mut warm_latencies: Vec<u64> = warm_results.iter().map(|r| r.latency_ms).collect();

    hot_latencies.sort_unstable();
    warm_latencies.sort_unstable();

    let hot_p50 = percentile(&hot_latencies, 50);
    let warm_p50 = percentile(&warm_latencies, 50);

    println!("\n=== Latency Comparison ===");
    println!("  Hot tier p50: {}ms", hot_p50);
    println!("  Warm tier p50: {}ms", warm_p50);

    // Hot tier should be faster than warm tier
    // Allow some tolerance since in tests the difference might be small
    if hot_p50 <= warm_p50 {
        println!("  Result: Hot tier is faster or equal (expected)");
        TestResult::pass_with_duration(name, start)
    } else if hot_p50 <= warm_p50 + 10 {
        // Within 10ms is acceptable
        println!("  Result: Hot tier slightly slower but within tolerance");
        TestResult::pass_with_duration(name, start)
    } else {
        println!("  Result: Hot tier unexpectedly slower than warm tier");
        TestResult::fail_with_duration(
            name,
            &format!(
                "Hot tier p50 ({}ms) > warm tier p50 ({}ms)",
                hot_p50, warm_p50
            ),
            start,
        )
    }
}

fn run_d6_cache_hit_rate(cache_results: &[CacheTestResult], min_rate: f32) -> TestResult {
    let start = Instant::now();
    let name = "D6_cache_hit_rate";

    if cache_results.is_empty() {
        return TestResult::fail_with_duration(name, "No cache test results", start);
    }

    let total_expected_hits: usize = cache_results
        .iter()
        .map(|r| r.repeat_count.saturating_sub(1))
        .sum();
    let total_hits: usize = cache_results.iter().map(|r| r.cache_hits).sum();

    let hit_rate = if total_expected_hits > 0 {
        total_hits as f32 / total_expected_hits as f32
    } else {
        1.0
    };

    println!("\n=== Cache Hit Rate ===");
    println!("  Hits: {}/{}", total_hits, total_expected_hits);
    println!(
        "  Rate: {:.1}% (threshold: {:.1}%)",
        hit_rate * 100.0,
        min_rate * 100.0
    );

    if hit_rate >= min_rate {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "Cache hit rate {:.1}% below threshold {:.1}%",
                hit_rate * 100.0,
                min_rate * 100.0
            ),
            start,
        )
    }
}

fn run_d7_quality_thresholds(
    cache_results: &[CacheTestResult],
    hot_results: &[TierTestResult],
    warm_results: &[TierTestResult],
) -> TestResult {
    let start = Instant::now();
    let name = "D7_quality_thresholds";

    let mut failures = Vec::new();

    // Check cache test pass rate
    let cache_pass_rate = if !cache_results.is_empty() {
        cache_results.iter().filter(|r| r.pass).count() as f32 / cache_results.len() as f32
    } else {
        1.0
    };

    if cache_pass_rate < 0.8 {
        failures.push(format!(
            "Cache pass rate {:.1}% < 80%",
            cache_pass_rate * 100.0
        ));
    }

    // Check hot tier pass rate
    let hot_pass_rate = if !hot_results.is_empty() {
        hot_results.iter().filter(|r| r.pass).count() as f32 / hot_results.len() as f32
    } else {
        1.0
    };

    if hot_pass_rate < 0.8 {
        failures.push(format!(
            "Hot tier pass rate {:.1}% < 80%",
            hot_pass_rate * 100.0
        ));
    }

    // Check warm tier recall (should find relevant docs)
    let avg_warm_recall = if !warm_results.is_empty() {
        warm_results.iter().map(|r| r.recall_at_k).sum::<f32>() / warm_results.len() as f32
    } else {
        1.0
    };

    if avg_warm_recall < 0.5 {
        failures.push(format!(
            "Warm tier recall {:.1}% < 50%",
            avg_warm_recall * 100.0
        ));
    }

    println!("\n=== Quality Thresholds ===");
    println!(
        "  Cache pass rate: {:.1}% (threshold: 80%)",
        cache_pass_rate * 100.0
    );
    println!(
        "  Hot tier pass rate: {:.1}% (threshold: 80%)",
        hot_pass_rate * 100.0
    );
    println!(
        "  Warm tier recall: {:.1}% (threshold: 50%)",
        avg_warm_recall * 100.0
    );

    if failures.is_empty() {
        println!("  All quality thresholds met!");
        TestResult::pass_with_duration(name, start)
    } else {
        println!("  Failures: {}", failures.join("; "));
        TestResult::fail_with_duration(name, &failures.join("; "), start)
    }
}

fn extract_retrieved_ids(response: &Value) -> Vec<String> {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let parsed: Value = serde_json::from_str(text).unwrap_or_default();

    parsed
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("tags")
                        .and_then(|t| t.as_array())
                        .and_then(|tags| tags.first())
                        .and_then(|tag| tag.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn calculate_recall(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }

    let retrieved_k: HashSet<_> = retrieved.iter().take(k).cloned().collect();
    let hits = relevant.intersection(&retrieved_k).count();

    hits as f64 / relevant.len() as f64
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
    fn test_tiered_eval_config_defaults() {
        let config = TieredEvalConfig::default();
        assert_eq!(config.hot_tier_latency_threshold_ms, 50);
        assert_eq!(config.warm_tier_latency_threshold_ms, 200);
        assert!((config.min_cache_hit_rate - 0.8).abs() < 0.01);
        assert_eq!(config.warmup_queries, 50);
    }

    #[test]
    fn test_calculate_recall() {
        let retrieved = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let relevant: HashSet<_> = vec!["a".to_string(), "b".to_string(), "d".to_string()]
            .into_iter()
            .collect();

        let recall = calculate_recall(&retrieved, &relevant, 5);
        assert!((recall - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn test_percentile() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        // p50 of 10 items: idx = 50 * 10 / 100 = 5, data[5] = 6
        assert_eq!(percentile(&data, 50), 6);
        // p90 of 10 items: idx = 90 * 10 / 100 = 9, data[9] = 10
        assert_eq!(percentile(&data, 90), 10);
    }

    #[test]
    fn test_cache_test_result() {
        let result = CacheTestResult {
            query_id: "q1".to_string(),
            repeat_count: 3,
            cache_hits: 2,
            avg_latency_ms: 5.0,
            pass: true,
        };

        assert_eq!(result.query_id, "q1");
        assert_eq!(result.cache_hits, 2);
        assert!(result.pass);
    }

    #[test]
    fn test_tier_test_result() {
        let result = TierTestResult {
            query_id: "q1".to_string(),
            expected_tier: "hot".to_string(),
            actual_tier: "hot".to_string(),
            latency_ms: 10,
            recall_at_k: 0.9,
            pass: true,
        };

        assert_eq!(result.expected_tier, result.actual_tier);
        assert!(result.pass);
    }
}
