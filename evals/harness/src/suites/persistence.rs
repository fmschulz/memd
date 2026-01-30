//! Suite A persistence tests: isolation, recovery, soft deletes
//!
//! Tests:
//! - A3_tenant_isolation: Tenant B cannot see Tenant A's data
//! - A4_crash_recovery: Data survives daemon restart (WAL replay)
//! - A5_soft_delete: Deleted chunks never returned

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

/// Run all persistence tests
pub fn run_all(memd_binary: &PathBuf) -> Vec<TestResult> {
    vec![
        a3_tenant_isolation(memd_binary),
        a4_crash_recovery(memd_binary),
        a5_soft_delete(memd_binary),
    ]
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

/// Parse the JSON content text and extract chunk_id
fn extract_chunk_id(response: &Value) -> Option<String> {
    let text = extract_content_text(response)?;
    let parsed: Value = serde_json::from_str(text).ok()?;
    parsed.get("chunk_id").and_then(|id| id.as_str()).map(String::from)
}

/// A3: Tenant B cannot see Tenant A's data
fn a3_tenant_isolation(memd_binary: &PathBuf) -> TestResult {
    let test_name = "A3_tenant_isolation";
    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail(test_name, &format!("tempdir: {}", e)),
    };

    // Start memd with persistent storage
    let mut client = match McpClient::start_with_args(
        memd_binary,
        &["--data-dir", data_dir.path().to_str().unwrap()],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail(test_name, &format!("start: {}", e)),
    };

    // Initialize
    if let Err(e) = client.initialize() {
        return TestResult::fail(test_name, &format!("initialize: {}", e));
    }

    // Add chunk as tenant_a
    let add_result = client.call_tool("memory.add", serde_json::json!({
        "tenant_id": "tenant_a",
        "text": "secret data for tenant A",
        "type": "doc"
    }));

    let chunk_id = match add_result {
        Ok(ref r) => match extract_chunk_id(r) {
            Some(id) => id,
            None => return TestResult::fail(test_name, &format!("no chunk_id in add response: {:?}", r)),
        },
        Err(e) => return TestResult::fail(test_name, &format!("add: {}", e)),
    };

    // Try to get as tenant_b - should fail
    let get_result = client.call_tool("memory.get", serde_json::json!({
        "tenant_id": "tenant_b",
        "chunk_id": chunk_id
    }));

    match get_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                let parsed: Value = serde_json::from_str(text).unwrap_or_default();
                // Should return null/not found - chunk_id presence means tenant_b can see tenant_a's data
                if parsed.get("chunk_id").is_some() {
                    return TestResult::fail(test_name, "tenant_b can see tenant_a's data");
                }
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("get: {}", e)),
    }

    // Try to search as tenant_b - should return empty
    let search_result = client.call_tool("memory.search", serde_json::json!({
        "tenant_id": "tenant_b",
        "query": "secret",
        "k": 10
    }));

    match search_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                let parsed: Value = serde_json::from_str(text).unwrap_or_default();
                if let Some(arr) = parsed["results"].as_array() {
                    if !arr.is_empty() {
                        return TestResult::fail(test_name, "tenant_b search returned tenant_a's data");
                    }
                }
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("search: {}", e)),
    }

    // Verify tenant_a can still access their data
    let get_a = client.call_tool("memory.get", serde_json::json!({
        "tenant_id": "tenant_a",
        "chunk_id": chunk_id
    }));

    match get_a {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                if !text.contains("secret data") {
                    return TestResult::fail(test_name, "tenant_a cannot access own data");
                }
            } else {
                return TestResult::fail(test_name, "no content in get tenant_a response");
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("get tenant_a: {}", e)),
    }

    drop(client);
    TestResult::pass(test_name)
}

/// A4: Data survives daemon restart (WAL replay)
fn a4_crash_recovery(memd_binary: &PathBuf) -> TestResult {
    let test_name = "A4_crash_recovery";
    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail(test_name, &format!("tempdir: {}", e)),
    };

    let data_path = data_dir.path().to_str().unwrap().to_string();
    let chunk_id: String;

    // Session 1: Add data
    {
        let mut client = match McpClient::start_with_args(
            memd_binary,
            &["--data-dir", &data_path],
        ) {
            Ok(c) => c,
            Err(e) => return TestResult::fail(test_name, &format!("start session 1: {}", e)),
        };

        if let Err(e) = client.initialize() {
            return TestResult::fail(test_name, &format!("init session 1: {}", e));
        }

        let add_result = client.call_tool("memory.add", serde_json::json!({
            "tenant_id": "recovery_test",
            "text": "data that must survive restart",
            "type": "doc"
        }));

        chunk_id = match add_result {
            Ok(ref r) => match extract_chunk_id(r) {
                Some(id) => id,
                None => return TestResult::fail(test_name, &format!("no chunk_id in add response: {:?}", r)),
            },
            Err(e) => return TestResult::fail(test_name, &format!("add: {}", e)),
        };

        // Shutdown daemon (simulates crash/restart)
        drop(client);
    }

    // Small delay to ensure files are flushed
    std::thread::sleep(Duration::from_millis(100));

    // Session 2: Verify data persisted
    {
        let mut client = match McpClient::start_with_args(
            memd_binary,
            &["--data-dir", &data_path],
        ) {
            Ok(c) => c,
            Err(e) => return TestResult::fail(test_name, &format!("start session 2: {}", e)),
        };

        if let Err(e) = client.initialize() {
            return TestResult::fail(test_name, &format!("init session 2: {}", e));
        }

        let get_result = client.call_tool("memory.get", serde_json::json!({
            "tenant_id": "recovery_test",
            "chunk_id": chunk_id
        }));

        match get_result {
            Ok(r) => {
                if let Some(text) = extract_content_text(&r) {
                    if !text.contains("survive restart") {
                        return TestResult::fail(test_name, &format!(
                            "data not recovered after restart. Got: {}",
                            &text[..text.len().min(200)]
                        ));
                    }
                } else {
                    return TestResult::fail(test_name, "no content in get response after restart");
                }
            }
            Err(e) => return TestResult::fail(test_name, &format!("get after restart: {}", e)),
        }

        drop(client);
    }

    TestResult::pass(test_name)
}

/// A5: Deleted chunks never returned
fn a5_soft_delete(memd_binary: &PathBuf) -> TestResult {
    let test_name = "A5_soft_delete";
    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return TestResult::fail(test_name, &format!("tempdir: {}", e)),
    };

    let mut client = match McpClient::start_with_args(
        memd_binary,
        &["--data-dir", data_dir.path().to_str().unwrap()],
    ) {
        Ok(c) => c,
        Err(e) => return TestResult::fail(test_name, &format!("start: {}", e)),
    };

    if let Err(e) = client.initialize() {
        return TestResult::fail(test_name, &format!("initialize: {}", e));
    }

    // Add chunk
    let add_result = client.call_tool("memory.add", serde_json::json!({
        "tenant_id": "delete_test",
        "text": "this will be deleted",
        "type": "doc"
    }));

    let chunk_id = match add_result {
        Ok(ref r) => match extract_chunk_id(r) {
            Some(id) => id,
            None => return TestResult::fail(test_name, &format!("no chunk_id in add response: {:?}", r)),
        },
        Err(e) => return TestResult::fail(test_name, &format!("add: {}", e)),
    };

    // Delete chunk
    let delete_result = client.call_tool("memory.delete", serde_json::json!({
        "tenant_id": "delete_test",
        "chunk_id": chunk_id
    }));

    match delete_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                if !text.contains("true") && !text.contains("deleted") {
                    return TestResult::fail(test_name, &format!("delete failed: {}", text));
                }
            } else {
                return TestResult::fail(test_name, "no content in delete response");
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("delete: {}", e)),
    }

    // Try to get - should return null/not found
    let get_result = client.call_tool("memory.get", serde_json::json!({
        "tenant_id": "delete_test",
        "chunk_id": chunk_id
    }));

    match get_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                let parsed: Value = serde_json::from_str(text).unwrap_or_default();
                if parsed.get("chunk_id").is_some() && parsed.get("text").is_some() {
                    return TestResult::fail(test_name, "deleted chunk still returned by get");
                }
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("get after delete: {}", e)),
    }

    // Search should not return deleted chunk
    let search_result = client.call_tool("memory.search", serde_json::json!({
        "tenant_id": "delete_test",
        "query": "deleted",
        "k": 10
    }));

    match search_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                let parsed: Value = serde_json::from_str(text).unwrap_or_default();
                if let Some(results) = parsed["results"].as_array() {
                    if !results.is_empty() {
                        return TestResult::fail(test_name, "deleted chunk appears in search results");
                    }
                }
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("search: {}", e)),
    }

    // Stats should show deleted count
    let stats_result = client.call_tool("memory.stats", serde_json::json!({
        "tenant_id": "delete_test"
    }));

    match stats_result {
        Ok(r) => {
            if let Some(text) = extract_content_text(&r) {
                let parsed: Value = serde_json::from_str(text).unwrap_or_default();
                let deleted = parsed["deleted_chunks"].as_u64().unwrap_or(0);
                if deleted != 1 {
                    return TestResult::fail(test_name, &format!("expected 1 deleted, got {}", deleted));
                }
            } else {
                return TestResult::fail(test_name, "no content in stats response");
            }
        }
        Err(e) => return TestResult::fail(test_name, &format!("stats: {}", e)),
    }

    drop(client);
    TestResult::pass(test_name)
}
