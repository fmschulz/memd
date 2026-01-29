//! MCP conformance test suite (Suite A)
//!
//! Tests MCP protocol conformance including:
//! - EVAL-02: Protocol methods and tool execution
//! - EVAL-03: Schema validation and error handling

use std::time::Instant;

use serde_json::json;

use crate::mcp_client::McpClient;
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
        test_invalid_json(memd_path),
        test_unknown_method(memd_path),
        test_missing_tenant_id(memd_path),
        test_invalid_chunk_type(memd_path),
    ]
}

/// A1: Initialize returns protocol version and capabilities
fn test_initialize(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_initialize", &format!("Failed to start: {}", e), start),
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
        Err(e) => TestResult::fail_with_duration("A1_initialize", &format!("Request failed: {}", e), start),
    }
}

/// A1: tools/list returns tools array
fn test_tools_list(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tools_list", &format!("Failed to start: {}", e), start),
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
                TestResult::fail_with_duration("A1_tools_list", "Missing tools array in response", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tools_list", &format!("Request failed: {}", e), start),
    }
}

/// A1: Verify all 6 tools are present
fn test_tools_list_count(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tools_count", &format!("Failed to start: {}", e), start),
    };

    let _ = client.initialize();

    match client.tools_list() {
        Ok(response) => {
            let tool_count = response
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);

            if tool_count == 6 {
                TestResult::pass_with_duration("A1_tools_count", start)
            } else {
                TestResult::fail_with_duration(
                    "A1_tools_count",
                    &format!("Expected 6 tools, got {}", tool_count),
                    start,
                )
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tools_count", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.add returns chunk_id
fn test_tool_call_add(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_add", &format!("Failed to start: {}", e), start),
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
                TestResult::fail_with_duration("A1_tool_add", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_add", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.search returns results array
fn test_tool_call_search(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_search", &format!("Failed to start: {}", e), start),
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
                TestResult::fail_with_duration("A1_tool_search", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_search", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.get returns chunk or null
fn test_tool_call_get(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_get", &format!("Failed to start: {}", e), start),
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
        Err(e) => return TestResult::fail_with_duration("A1_tool_get", &format!("Add failed: {}", e), start),
    };

    // Extract chunk_id
    let chunk_id = add_response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|v| v.get("chunk_id").and_then(|id| id.as_str()).map(String::from));

    let chunk_id = match chunk_id {
        Some(id) => id,
        None => return TestResult::fail_with_duration("A1_tool_get", "Could not extract chunk_id from add", start),
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
                TestResult::fail_with_duration("A1_tool_get", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_get", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.delete returns deleted boolean
fn test_tool_call_delete(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_delete", &format!("Failed to start: {}", e), start),
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
        Err(e) => return TestResult::fail_with_duration("A1_tool_delete", &format!("Add failed: {}", e), start),
    };

    // Extract chunk_id
    let chunk_id = add_response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|v| v.get("chunk_id").and_then(|id| id.as_str()).map(String::from));

    let chunk_id = match chunk_id {
        Some(id) => id,
        None => return TestResult::fail_with_duration("A1_tool_delete", "Could not extract chunk_id from add", start),
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
                TestResult::fail_with_duration("A1_tool_delete", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_delete", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.stats returns statistics
fn test_tool_call_stats(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_stats", &format!("Failed to start: {}", e), start),
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
                TestResult::fail_with_duration("A1_tool_stats", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_stats", &format!("Request failed: {}", e), start),
    }
}

/// A1: memory.add_batch returns chunk_ids array
fn test_tool_call_add_batch(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A1_tool_add_batch", &format!("Failed to start: {}", e), start),
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
                TestResult::fail_with_duration("A1_tool_add_batch", "Response missing content text", start)
            }
        }
        Err(e) => TestResult::fail_with_duration("A1_tool_add_batch", &format!("Request failed: {}", e), start),
    }
}

/// A2: Invalid JSON returns PARSE_ERROR (-32700)
fn test_invalid_json(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A2_invalid_json", &format!("Failed to start: {}", e), start),
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
        Err(e) => TestResult::fail_with_duration("A2_invalid_json", &format!("Request failed: {}", e), start),
    }
}

/// A2: Unknown method returns METHOD_NOT_FOUND (-32601)
fn test_unknown_method(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A2_unknown_method", &format!("Failed to start: {}", e), start),
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
        Err(e) => TestResult::fail_with_duration("A2_unknown_method", &format!("Request failed: {}", e), start),
    }
}

/// A2: Missing required param returns INVALID_PARAMS (-32602)
fn test_missing_tenant_id(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A2_missing_tenant", &format!("Failed to start: {}", e), start),
    };

    let _ = client.initialize();

    match client.call_tool(
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
        Err(e) => TestResult::fail_with_duration("A2_missing_tenant", &format!("Request failed: {}", e), start),
    }
}

/// A2: Invalid param type returns INVALID_PARAMS (-32602)
fn test_invalid_chunk_type(memd_path: &str) -> TestResult {
    let start = Instant::now();
    let mut client = match McpClient::start(memd_path) {
        Ok(c) => c,
        Err(e) => return TestResult::fail_with_duration("A2_invalid_chunk_type", &format!("Failed to start: {}", e), start),
    };

    let _ = client.initialize();

    match client.call_tool(
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
        Err(e) => TestResult::fail_with_duration("A2_invalid_chunk_type", &format!("Request failed: {}", e), start),
    }
}
