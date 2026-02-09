//! MCP conformance test suite (Suite A)
//!
//! Tests MCP protocol conformance including:
//! - EVAL-02: Protocol methods and tool execution
//! - EVAL-03: Schema validation and error handling

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::json;
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::{McpClient, McpClientError};
use crate::TestResult;

/// Run all MCP conformance tests
pub fn run(memd_path: &str) -> Vec<TestResult> {
    vec![
        test_initialize(memd_path),
        test_tools_list(memd_path),
        test_tools_list_count(memd_path),
        test_tool_call_add(memd_path),
        test_tool_call_search(memd_path),
        test_tool_call_get(memd_path),
        test_tool_call_delete(memd_path),
        test_tool_call_stats(memd_path),
        test_tool_call_add_batch(memd_path),
        test_e2e_memory_tools_in_memory(memd_path),
        test_e2e_memory_tools_persistent(memd_path),
        test_invalid_json(memd_path),
        test_unknown_method(memd_path),
        test_missing_tenant_id(memd_path),
        test_invalid_chunk_type(memd_path),
        test_tool_error_propagates(memd_path),
    ]
}

fn parse_tool_payload(response: &Value) -> Result<Value, String> {
    let text = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| "response missing content text".to_string())?;

    serde_json::from_str(text).map_err(|e| format!("invalid response payload JSON: {}", e))
}

fn start_persistent_client(memd_path: &str) -> Result<(McpClient, TempDir), McpClientError> {
    let data_dir = TempDir::new()?;
    let memd_binary = PathBuf::from(memd_path);
    let data_dir_arg = data_dir.path().to_string_lossy().to_string();
    let client = McpClient::start_with_args(&memd_binary, &["--data-dir", data_dir_arg.as_str()])?;
    Ok((client, data_dir))
}

fn run_memory_tools_flow(client: &mut McpClient, tenant_id: &str) -> Result<(), String> {
    client
        .initialize()
        .map_err(|e| format!("initialize failed: {}", e))?;

    let add_response = client
        .call_tool(
            "memory.add",
            json!({
                "tenant_id": tenant_id,
                "text": "end to end MCP test content",
                "type": "doc"
            }),
        )
        .map_err(|e| format!("memory.add failed: {}", e))?;
    let add_payload = parse_tool_payload(&add_response)?;
    let chunk_id = add_payload
        .get("chunk_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "memory.add payload missing chunk_id".to_string())?;

    let search_response = client
        .call_tool(
            "memory.search",
            json!({
                "tenant_id": tenant_id,
                "query": "end to end",
                "k": 5
            }),
        )
        .map_err(|e| format!("memory.search failed: {}", e))?;
    let search_payload = parse_tool_payload(&search_response)?;
    let results = search_payload
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "memory.search payload missing results array".to_string())?;
    let found = results
        .iter()
        .any(|r| r.get("chunk_id").and_then(|id| id.as_str()) == Some(chunk_id));
    if !found {
        return Err("memory.search did not return added chunk".to_string());
    }

    let metrics_response = client
        .call_tool(
            "memory.metrics",
            json!({
                "include_recent": false
            }),
        )
        .map_err(|e| format!("memory.metrics failed: {}", e))?;
    let metrics_payload = parse_tool_payload(&metrics_response)?;
    if !metrics_payload.get("index").is_some_and(Value::is_object) {
        return Err("memory.metrics payload missing index object".to_string());
    }

    let compact_response = client
        .call_tool(
            "memory.compact",
            json!({
                "tenant_id": tenant_id,
                "force": false
            }),
        )
        .map_err(|e| format!("memory.compact failed: {}", e))?;
    let compact_payload = parse_tool_payload(&compact_response)?;
    let status = compact_payload
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "memory.compact payload missing status".to_string())?;
    if !matches!(status, "completed" | "skipped") {
        return Err(format!(
            "memory.compact returned unexpected status '{}'",
            status
        ));
    }

    Ok(())
}

/// A1: Initialize returns protocol version and capabilities
fn test_initialize(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_initialize",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    match client.initialize() {
        Ok(response) => {
            let has_version = response
                .get("result")
                .and_then(|r| r.get("protocolVersion"))
                .is_some();
            let has_capabilities = response
                .get("result")
                .and_then(|r| r.get("capabilities"))
                .is_some();

            if has_version && has_capabilities {
                TestResult::pass_with_duration("A1_initialize", start)
            } else {
                TestResult::fail_with_duration(
                    "A1_initialize",
                    "Missing protocolVersion or capabilities",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_initialize",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: tools/list returns tools array
fn test_tools_list(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tools_list",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.tools_list() {
        Ok(response) => {
            let has_tools = response
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(|t| t.as_array())
                .is_some();

            if has_tools {
                TestResult::pass_with_duration("A1_tools_list", start)
            } else {
                TestResult::fail_with_duration(
                    "A1_tools_list",
                    "Missing tools array in response",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tools_list",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: Verify core tools are present (allowing additional tools)
fn test_tools_list_count(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tools_count",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.tools_list() {
        Ok(response) => {
            let tools = response
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();

            let tool_names: HashSet<String> = tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect();

            let required = [
                "memory.search",
                "memory.add",
                "memory.add_batch",
                "memory.get",
                "memory.delete",
                "memory.stats",
                "memory.metrics",
                "memory.compact",
            ];

            let missing: Vec<&str> = required
                .iter()
                .copied()
                .filter(|name| !tool_names.contains(*name))
                .collect();

            if missing.is_empty() && tool_names.len() >= required.len() {
                TestResult::pass_with_duration("A1_tools_count", start)
            } else {
                let available: Vec<String> = tool_names.into_iter().collect();
                TestResult::fail_with_duration(
                    "A1_tools_count",
                    &format!(
                        "Missing core tools: {:?}. Available tool count: {}",
                        missing,
                        available.len()
                    ),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tools_count",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: memory.add returns chunk_id
fn test_tool_call_add(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_add",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "Test content for eval",
            "type": "doc"
        }),
    ) {
        Ok(response) => {
            // Check for error in response
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_add",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            // Parse the content response
            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result) if result.get("chunk_id").is_some() => {
                        TestResult::pass_with_duration("A1_tool_add", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_add",
                        "Response content missing chunk_id",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_add",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => {
            TestResult::fail_with_duration("A1_tool_add", &format!("Request failed: {}", e), start)
        }
    }
}

/// A1: memory.search returns results array
fn test_tool_call_search(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_search",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    // Add a chunk first
    let _ = client.call_tool(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "searchable content",
            "type": "doc"
        }),
    );

    match client.call_tool(
        "memory.search",
        json!({
            "tenant_id": "test_tenant",
            "query": "searchable"
        }),
    ) {
        Ok(response) => {
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_search",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result) if result.get("results").and_then(|r| r.as_array()).is_some() => {
                        TestResult::pass_with_duration("A1_tool_search", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_search",
                        "Response missing results array",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_search",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tool_search",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: memory.get returns chunk or null
fn test_tool_call_get(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_get",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    // Add a chunk first
    let add_response = match client.call_tool(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "get test content",
            "type": "doc"
        }),
    ) {
        Ok(r) => r,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_get",
                &format!("Add failed: {}", e),
                start,
            )
        }
    };

    // Extract chunk_id
    let chunk_id = add_response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|v| {
            v.get("chunk_id")
                .and_then(|id| id.as_str())
                .map(String::from)
        });

    let chunk_id = match chunk_id {
        Some(id) => id,
        None => {
            return TestResult::fail_with_duration(
                "A1_tool_get",
                "Could not extract chunk_id from add",
                start,
            )
        }
    };

    match client.call_tool(
        "memory.get",
        json!({
            "tenant_id": "test_tenant",
            "chunk_id": chunk_id
        }),
    ) {
        Ok(response) => {
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_get",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                // Should be a JSON chunk object or null
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result) if result.get("chunk_id").is_some() || result.is_null() => {
                        TestResult::pass_with_duration("A1_tool_get", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_get",
                        "Response not a valid chunk or null",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_get",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => {
            TestResult::fail_with_duration("A1_tool_get", &format!("Request failed: {}", e), start)
        }
    }
}

/// A1: memory.delete returns deleted boolean
fn test_tool_call_delete(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_delete",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    // Add a chunk first
    let add_response = match client.call_tool(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "delete test content",
            "type": "doc"
        }),
    ) {
        Ok(r) => r,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_delete",
                &format!("Add failed: {}", e),
                start,
            )
        }
    };

    // Extract chunk_id
    let chunk_id = add_response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|v| {
            v.get("chunk_id")
                .and_then(|id| id.as_str())
                .map(String::from)
        });

    let chunk_id = match chunk_id {
        Some(id) => id,
        None => {
            return TestResult::fail_with_duration(
                "A1_tool_delete",
                "Could not extract chunk_id from add",
                start,
            )
        }
    };

    match client.call_tool(
        "memory.delete",
        json!({
            "tenant_id": "test_tenant",
            "chunk_id": chunk_id
        }),
    ) {
        Ok(response) => {
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_delete",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result) if result.get("deleted").is_some() => {
                        TestResult::pass_with_duration("A1_tool_delete", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_delete",
                        "Response missing deleted field",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_delete",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tool_delete",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: memory.stats returns statistics
fn test_tool_call_stats(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_stats",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool(
        "memory.stats",
        json!({
            "tenant_id": "test_tenant"
        }),
    ) {
        Ok(response) => {
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_stats",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result)
                        if result.get("total_chunks").is_some()
                            && result.get("chunk_types").is_some() =>
                    {
                        TestResult::pass_with_duration("A1_tool_stats", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_stats",
                        "Response missing total_chunks or chunk_types",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_stats",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tool_stats",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: memory.add_batch returns chunk_ids array
fn test_tool_call_add_batch(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_tool_add_batch",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool(
        "memory.add_batch",
        json!({
            "tenant_id": "test_tenant",
            "chunks": [
                {"text": "batch item 1", "type": "doc"},
                {"text": "batch item 2", "type": "code"}
            ]
        }),
    ) {
        Ok(response) => {
            if response.get("error").is_some() {
                return TestResult::fail_with_duration(
                    "A1_tool_add_batch",
                    &format!("Tool returned error: {:?}", response.get("error")),
                    start,
                );
            }

            let content_text = response
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str());

            if let Some(text) = content_text {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
                match parsed {
                    Ok(result)
                        if result
                            .get("chunk_ids")
                            .and_then(|ids| ids.as_array())
                            .map(|a| a.len() == 2)
                            .unwrap_or(false) =>
                    {
                        TestResult::pass_with_duration("A1_tool_add_batch", start)
                    }
                    _ => TestResult::fail_with_duration(
                        "A1_tool_add_batch",
                        "Response missing chunk_ids array with 2 items",
                        start,
                    ),
                }
            } else {
                TestResult::fail_with_duration(
                    "A1_tool_add_batch",
                    "Response missing content text",
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A1_tool_add_batch",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A1: End-to-end MCP memory flow in in-memory mode.
fn test_e2e_memory_tools_in_memory(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_e2e_tools_in_memory",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    match run_memory_tools_flow(&mut client, "mcp_e2e_memory") {
        Ok(_) => TestResult::pass_with_duration("A1_e2e_tools_in_memory", start),
        Err(e) => TestResult::fail_with_duration("A1_e2e_tools_in_memory", &e, start),
    }
}

/// A1: End-to-end MCP memory flow in persistent mode.
fn test_e2e_memory_tools_persistent(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let (mut client, _data_dir) = match start_persistent_client(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A1_e2e_tools_persistent",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    match run_memory_tools_flow(&mut client, "mcp_e2e_persistent") {
        Ok(_) => TestResult::pass_with_duration("A1_e2e_tools_persistent", start),
        Err(e) => TestResult::fail_with_duration("A1_e2e_tools_persistent", &e, start),
    }
}

/// A2: Invalid JSON returns PARSE_ERROR (-32700)
fn test_invalid_json(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A2_invalid_json",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    // Send malformed JSON
    match client.send_raw("{not valid json}") {
        Ok(response) => {
            let error_code = response
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0);

            if error_code == -32700 {
                TestResult::pass_with_duration("A2_invalid_json", start)
            } else {
                TestResult::fail_with_duration(
                    "A2_invalid_json",
                    &format!("Expected error code -32700, got {}", error_code),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A2_invalid_json",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A2: Unknown method returns METHOD_NOT_FOUND (-32601)
fn test_unknown_method(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A2_unknown_method",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    match client.request("nonexistent/method", None) {
        Ok(response) => {
            let error_code = response
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0);

            if error_code == -32601 {
                TestResult::pass_with_duration("A2_unknown_method", start)
            } else {
                TestResult::fail_with_duration(
                    "A2_unknown_method",
                    &format!("Expected error code -32601, got {}", error_code),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A2_unknown_method",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A2: Missing required param returns INVALID_PARAMS (-32602)
fn test_missing_tenant_id(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A2_missing_tenant",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool_raw(
        "memory.search",
        json!({
            "query": "test"
            // Missing tenant_id
        }),
    ) {
        Ok(response) => {
            let error_code = response
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0);

            if error_code == -32602 {
                TestResult::pass_with_duration("A2_missing_tenant", start)
            } else {
                TestResult::fail_with_duration(
                    "A2_missing_tenant",
                    &format!("Expected error code -32602, got {}", error_code),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A2_missing_tenant",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A2: Invalid param type returns INVALID_PARAMS (-32602)
fn test_invalid_chunk_type(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A2_invalid_chunk_type",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool_raw(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "test",
            "type": "invalid_type_xyz"
        }),
    ) {
        Ok(response) => {
            let error_code = response
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0);

            if error_code == -32602 {
                TestResult::pass_with_duration("A2_invalid_chunk_type", start)
            } else {
                TestResult::fail_with_duration(
                    "A2_invalid_chunk_type",
                    &format!("Expected error code -32602, got {}", error_code),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration(
            "A2_invalid_chunk_type",
            &format!("Request failed: {}", e),
            start,
        ),
    }
}

/// A2: Regression test - tool call RPC errors must propagate as Err from McpClient::call_tool
fn test_tool_error_propagates(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => {
            return TestResult::fail_with_duration(
                "A2_tool_error_propagates",
                &format!("Failed to start: {}", e),
                start,
            )
        }
    };

    let _ = client.initialize();

    match client.call_tool(
        "memory.add",
        json!({
            "tenant_id": "test_tenant",
            "text": "tool error propagation regression",
            "type": "invalid_type_xyz"
        }),
    ) {
        Err(McpClientError::RpcError(_)) => {
            TestResult::pass_with_duration("A2_tool_error_propagates", start)
        }
        Err(e) => TestResult::fail_with_duration(
            "A2_tool_error_propagates",
            &format!("Expected RpcError, got: {}", e),
            start,
        ),
        Ok(_) => TestResult::fail_with_duration(
            "A2_tool_error_propagates",
            "call_tool returned Ok for invalid chunk type (error propagation regression)",
            start,
        ),
    }
}
