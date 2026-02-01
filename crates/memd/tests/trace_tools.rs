//! Integration tests for trace query tools.
//!
//! Tests the debug.find_tool_calls and debug.find_errors MCP tools
//! through the full handler chain.

use std::sync::Arc;

use memd::mcp::handlers::{
    handle_find_errors, handle_find_tool_calls, FindErrorsParams, FindToolCallsParams,
};
use memd::structural::{StackFrameRecord, StackTraceRecord, StructuralStore, ToolTraceRecord, TraceQueryService};
use memd::types::TenantId;

fn test_tenant() -> TenantId {
    TenantId::new("test_tenant").unwrap()
}

fn create_test_store() -> Arc<StructuralStore> {
    Arc::new(StructuralStore::in_memory().unwrap())
}

fn create_trace_service(store: Arc<StructuralStore>) -> Arc<TraceQueryService> {
    Arc::new(TraceQueryService::new(store))
}

#[test]
fn test_find_tool_calls_handler_empty() {
    let store = create_test_store();
    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();

    assert!(text.contains("tool_calls"));
    assert!(text.contains("total_count"));
}

#[test]
fn test_find_tool_calls_handler_with_data() {
    let store = create_test_store();
    let tenant = test_tenant();

    // Insert test traces
    let mut trace1 = ToolTraceRecord::new(tenant.clone(), "memory.search", 1000);
    trace1.input_json = Some(r#"{"query": "test"}"#.to_string());
    trace1.output_json = Some(r#"{"results": []}"#.to_string());
    trace1.session_id = Some("session_1".to_string());
    store.insert_tool_trace(&trace1).unwrap();

    let mut trace2 = ToolTraceRecord::new(tenant.clone(), "memory.add", 2000);
    trace2.input_json = Some(r#"{"text": "hello"}"#.to_string());
    trace2.error_json = Some(r#"{"message": "quota exceeded"}"#.to_string());
    trace2.session_id = Some("session_1".to_string());
    store.insert_tool_trace(&trace2).unwrap();

    let service = create_trace_service(store);

    // Test: find all tool calls
    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 2);
}

#[test]
fn test_find_tool_calls_by_name() {
    let store = create_test_store();
    let tenant = test_tenant();

    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "memory.search", 1000)).unwrap();
    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "memory.add", 2000)).unwrap();
    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "memory.search", 3000)).unwrap();

    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: Some("memory.search".to_string()),
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 2);

    let calls = response["tool_calls"].as_array().unwrap();
    assert!(calls.iter().all(|c| c["tool_name"] == "memory.search"));
}

#[test]
fn test_find_tool_calls_errors_only() {
    let store = create_test_store();
    let tenant = test_tenant();

    let mut success = ToolTraceRecord::new(tenant.clone(), "tool", 1000);
    success.output_json = Some(r#"{"ok": true}"#.to_string());
    store.insert_tool_trace(&success).unwrap();

    let mut error = ToolTraceRecord::new(tenant.clone(), "tool", 2000);
    error.error_json = Some(r#"{"message": "failed"}"#.to_string());
    store.insert_tool_trace(&error).unwrap();

    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: true,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 1);
    assert!(response["tool_calls"][0]["error"].is_object());
}

#[test]
fn test_find_tool_calls_time_range() {
    let store = create_test_store();
    let tenant = test_tenant();

    // 2024-01-01 00:00:00 UTC
    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 1704067200000)).unwrap();
    // 2024-01-02 00:00:00 UTC
    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 1704153600000)).unwrap();
    // 2024-01-03 00:00:00 UTC
    store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 1704240000000)).unwrap();

    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: Some("2024-01-01T12:00:00Z".to_string()),
        time_to: Some("2024-01-02T12:00:00Z".to_string()),
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    // Only the middle trace should match
    assert_eq!(response["total_count"], 1);
}

#[test]
fn test_find_errors_handler_empty() {
    let store = create_test_store();
    let service = create_trace_service(store);

    let params = FindErrorsParams {
        tenant_id: "test_tenant".to_string(),
        error_signature: None,
        function_name: None,
        file_path: None,
        time_from: None,
        time_to: None,
        limit: 50,
        include_frames: true,
    };

    let result = handle_find_errors(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();

    assert!(text.contains("errors"));
    assert!(text.contains("total_count"));
}

#[test]
fn test_find_errors_with_data() {
    let store = create_test_store();
    let tenant = test_tenant();

    let trace = StackTraceRecord::new(
        tenant.clone(),
        1000,
        "TypeError",
        "Cannot read property 'foo' of undefined",
        "trace_1",
    );
    let frames = vec![
        StackFrameRecord {
            frame_id: None,
            trace_id: 0,
            frame_idx: 0,
            file_path: Some("src/index.js".to_string()),
            function_name: Some("processData".to_string()),
            line_number: Some(42),
            col_number: Some(10),
            context: None,
        },
        StackFrameRecord {
            frame_id: None,
            trace_id: 0,
            frame_idx: 1,
            file_path: Some("src/utils.js".to_string()),
            function_name: Some("handleRequest".to_string()),
            line_number: Some(100),
            col_number: None,
            context: None,
        },
    ];
    store.insert_stack_trace(&trace, &frames).unwrap();

    let service = create_trace_service(store);

    let params = FindErrorsParams {
        tenant_id: "test_tenant".to_string(),
        error_signature: None,
        function_name: None,
        file_path: None,
        time_from: None,
        time_to: None,
        limit: 50,
        include_frames: true,
    };

    let result = handle_find_errors(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 1);

    let error = &response["errors"][0];
    assert_eq!(error["error_signature"], "TypeError");
    assert!(error["error_message"].as_str().unwrap().contains("foo"));

    let frames = error["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["function_name"], "processData");
}

#[test]
fn test_find_errors_by_signature() {
    let store = create_test_store();
    let tenant = test_tenant();

    store.insert_stack_trace(
        &StackTraceRecord::new(tenant.clone(), 1000, "TypeError", "msg1", "t1"),
        &[],
    ).unwrap();
    store.insert_stack_trace(
        &StackTraceRecord::new(tenant.clone(), 2000, "ReferenceError", "msg2", "t2"),
        &[],
    ).unwrap();
    store.insert_stack_trace(
        &StackTraceRecord::new(tenant.clone(), 3000, "TypeError", "msg3", "t3"),
        &[],
    ).unwrap();

    let service = create_trace_service(store);

    let params = FindErrorsParams {
        tenant_id: "test_tenant".to_string(),
        error_signature: Some("TypeError".to_string()),
        function_name: None,
        file_path: None,
        time_from: None,
        time_to: None,
        limit: 50,
        include_frames: true,
    };

    let result = handle_find_errors(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 2);

    let errors = response["errors"].as_array().unwrap();
    assert!(errors.iter().all(|e| e["error_signature"] == "TypeError"));
}

#[test]
fn test_find_errors_by_function() {
    let store = create_test_store();
    let tenant = test_tenant();

    // Error with matching function in stack
    let trace1 = StackTraceRecord::new(tenant.clone(), 1000, "Error", "msg1", "t1");
    let frames1 = vec![StackFrameRecord {
        frame_id: None,
        trace_id: 0,
        frame_idx: 0,
        file_path: None,
        function_name: Some("targetFunction".to_string()),
        line_number: None,
        col_number: None,
        context: None,
    }];
    store.insert_stack_trace(&trace1, &frames1).unwrap();

    // Error without matching function
    let trace2 = StackTraceRecord::new(tenant.clone(), 2000, "Error", "msg2", "t2");
    let frames2 = vec![StackFrameRecord {
        frame_id: None,
        trace_id: 0,
        frame_idx: 0,
        file_path: None,
        function_name: Some("otherFunction".to_string()),
        line_number: None,
        col_number: None,
        context: None,
    }];
    store.insert_stack_trace(&trace2, &frames2).unwrap();

    let service = create_trace_service(store);

    let params = FindErrorsParams {
        tenant_id: "test_tenant".to_string(),
        error_signature: None,
        function_name: Some("targetFunction".to_string()),
        file_path: None,
        time_from: None,
        time_to: None,
        limit: 50,
        include_frames: true,
    };

    let result = handle_find_errors(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(response["total_count"], 1);
}

#[test]
fn test_find_errors_without_frames() {
    let store = create_test_store();
    let tenant = test_tenant();

    let trace = StackTraceRecord::new(tenant.clone(), 1000, "Error", "msg", "t1");
    let frames = vec![StackFrameRecord {
        frame_id: None,
        trace_id: 0,
        frame_idx: 0,
        file_path: Some("src/main.rs".to_string()),
        function_name: Some("main".to_string()),
        line_number: Some(10),
        col_number: None,
        context: None,
    }];
    store.insert_stack_trace(&trace, &frames).unwrap();

    let service = create_trace_service(store);

    let params = FindErrorsParams {
        tenant_id: "test_tenant".to_string(),
        error_signature: None,
        function_name: None,
        file_path: None,
        time_from: None,
        time_to: None,
        limit: 50,
        include_frames: false, // Don't include frames
    };

    let result = handle_find_errors(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    // Frames should be omitted when include_frames is false
    let error = &response["errors"][0];
    assert!(error.get("frames").is_none());
}

#[test]
fn test_invalid_tenant_id() {
    let store = create_test_store();
    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "invalid-tenant".to_string(), // hyphens not allowed
        tool_name: None,
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params);
    assert!(result.is_err());
}

#[test]
fn test_invalid_time_format() {
    let store = create_test_store();
    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: Some("not-a-date".to_string()),
        time_to: None,
        errors_only: false,
        limit: 50,
    };

    let result = handle_find_tool_calls(&service, params);
    assert!(result.is_err());
}

#[test]
fn test_limit_enforcement() {
    let store = create_test_store();
    let tenant = test_tenant();

    // Insert many traces
    for i in 0..20 {
        store.insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", i * 1000)).unwrap();
    }

    let service = create_trace_service(store);

    let params = FindToolCallsParams {
        tenant_id: "test_tenant".to_string(),
        tool_name: None,
        session_id: None,
        time_from: None,
        time_to: None,
        errors_only: false,
        limit: 5, // Request only 5
    };

    let result = handle_find_tool_calls(&service, params).unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();

    // Should respect the limit
    assert_eq!(response["tool_calls"].as_array().unwrap().len(), 5);
}
