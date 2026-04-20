//! Track G integration tests: pure render_markdown_tree (G1) and the
//! `memory.export_markdown` MCP tool (G2).

mod common;
use common::*;

#[tokio::test]
async fn export_markdown_returns_files_without_writing() {
    let (server, _tmp) = test_server().await;
    add_chunk(&server, "t", "First note about the freeze.").await;
    add_chunk(&server, "t", "Second note about the migration.").await;

    let r = call_tool(
        &server,
        "memory.export_markdown",
        serde_json::json!({
            "tenant_id": "t",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let files = body["files"]
        .as_array()
        .expect("files array");
    assert!(!files.is_empty(), "expected at least one file");
    for f in files {
        assert!(f["path"].as_str().is_some(), "file.path must be a string");
        let content = f["content"].as_str().expect("content string");
        assert!(!content.is_empty(), "file.content must be non-empty");
        assert!(
            content.contains("---"),
            "rendered markdown should carry YAML frontmatter delimited by ---"
        );
    }
}

#[tokio::test]
async fn export_markdown_groups_chunks_by_project_and_type() {
    let (server, _tmp) = test_server().await;
    // Two chunks under p1 (one doc, one code), one under p2 (doc).
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "doc one",
            "type": "doc",
        }),
    )
    .await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "fn main() {}",
            "type": "code",
        }),
    )
    .await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p2",
            "text": "doc two",
            "type": "doc",
        }),
    )
    .await;

    let r = call_tool(
        &server,
        "memory.export_markdown",
        serde_json::json!({ "tenant_id": "t" }),
    )
    .await;
    let body = parse_result_text(&r);
    let paths: Vec<String> = body["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths.len(), 3, "expected one file per (project, type) bucket");
    assert!(
        paths.iter().any(|p| p.contains("p1/doc")),
        "p1/doc bucket missing: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("p1/code")),
        "p1/code bucket missing: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("p2/doc")),
        "p2/doc bucket missing: {paths:?}"
    );
}

#[tokio::test]
async fn export_markdown_handles_empty_tenant_returns_empty_files_array() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.export_markdown",
        serde_json::json!({ "tenant_id": "empty_tenant" }),
    )
    .await;
    let body = parse_result_text(&r);
    let files = body["files"].as_array().expect("files array");
    assert!(
        files.is_empty(),
        "empty tenant should return empty files array, got {files:?}"
    );
}

// Codex round-1 G1/G2 HIGH regression at the MCP layer: distinct raw
// project_ids whose sanitised names would collide must produce
// distinct files in the MCP response too. Pre-fix the bucket key was
// the sanitised string, so `"a/b"` and `"a:b"` collapsed.
#[tokio::test]
async fn export_markdown_keeps_collision_prone_project_ids_distinct() {
    let (server, _tmp) = test_server().await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "a/b",
            "text": "from a/b",
            "type": "doc",
        }),
    )
    .await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "a:b",
            "text": "from a:b",
            "type": "doc",
        }),
    )
    .await;

    let r = call_tool(
        &server,
        "memory.export_markdown",
        serde_json::json!({ "tenant_id": "t" }),
    )
    .await;
    let body = parse_result_text(&r);
    let files = body["files"].as_array().expect("files array");
    assert_eq!(
        files.len(),
        2,
        "raw projects 'a/b' and 'a:b' must produce two files"
    );
    let paths: Vec<String> = files
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    assert_ne!(paths[0], paths[1], "paths must differ — got {paths:?}");
    // Frontmatter preserves the raw project_id (sanitisation is path-only).
    let raw_projects: Vec<&str> = files
        .iter()
        .map(|f| f["content"].as_str().unwrap())
        .collect();
    assert!(raw_projects.iter().any(|c| c.contains("project: a/b")));
    assert!(raw_projects.iter().any(|c| c.contains("project: a:b")));
}

#[tokio::test]
async fn export_markdown_can_filter_by_project_id() {
    let (server, _tmp) = test_server().await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "p1 note",
            "type": "doc",
        }),
    )
    .await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p2",
            "text": "p2 note",
            "type": "doc",
        }),
    )
    .await;

    let r = call_tool(
        &server,
        "memory.export_markdown",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let paths: Vec<String> = body["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0].contains("p1/doc"),
        "expected only p1 bucket, got {paths:?}"
    );
}
