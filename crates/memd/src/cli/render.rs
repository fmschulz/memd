use std::path::Path;

use serde_json::Value;

use crate::error::{MemdError, Result};
use crate::types::{MemoryChunk, TenantId};

use super::args::{ExportFormat, TenantScopeConfig};

pub(super) fn unwrap_content_payload(value: Value) -> Result<Value> {
    let text = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            MemdError::ProtocolError("memory.search returned no text payload".to_string())
        })?;
    Ok(serde_json::from_str(text)?)
}

pub(super) fn render_search_payload(payload: &Value, format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Json => Ok(serde_json::to_string_pretty(payload)? + "\n"),
        ExportFormat::Jsonl => render_results_jsonl(payload),
        ExportFormat::Markdown => render_memory_markdown(payload, "memd search"),
    }
}

pub(super) fn render_agent_context(payload: &Value, format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Json => Ok(serde_json::to_string_pretty(payload)? + "\n"),
        ExportFormat::Jsonl => render_results_jsonl(payload),
        ExportFormat::Markdown => render_memory_markdown(payload, "memd CLI Context"),
    }
}

fn render_results_jsonl(payload: &Value) -> Result<String> {
    let mut out = String::new();
    if let Some(results) = payload.get("results").and_then(Value::as_array) {
        for result in results {
            out.push_str(&serde_json::to_string(result)?);
            out.push('\n');
        }
    }
    Ok(out)
}

fn render_memory_markdown(payload: &Value, title: &str) -> Result<String> {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(title);
    out.push_str("\n\n");

    if let Some(tenant_id) = payload.get("tenant_id").and_then(Value::as_str) {
        out.push_str(&format!("- tenant_id: `{tenant_id}`\n"));
    }
    if let Some(project_id) = payload.get("project_id").and_then(Value::as_str) {
        out.push_str(&format!("- project_id: `{project_id}`\n"));
    }
    if let Some(count) = payload.get("result_count").and_then(Value::as_u64) {
        out.push_str(&format!("- result_count: `{count}`\n"));
    }
    out.push_str("- interface: `cli_only`\n");
    out.push_str("- contract: use these memories only when they match current evidence; cite chunk_id or citation_id when used.\n");
    render_scope_status_lines(&mut out, payload.get("scope_status"));

    if let Some(queries) = payload.get("queries").and_then(Value::as_array) {
        out.push_str("\n## Queries\n\n");
        for query in queries {
            if let Some(text) = query.get("query").and_then(Value::as_str) {
                let count = query
                    .get("result_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                out.push_str(&format!("- `{text}` -> {count} result(s)\n"));
            }
        }
    }

    out.push_str("\n## Results\n\n");
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return Ok(out);
    };
    for (idx, result) in results.iter().enumerate() {
        let chunk_id = result
            .get("chunk_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let chunk_type = result
            .get("chunk_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let score = result.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        out.push_str(&format!(
            "### {}. `{}` ({}, score {:.3})\n\n",
            idx + 1,
            chunk_id,
            chunk_type,
            score
        ));
        if let Some(citation_id) = result
            .get("citation")
            .and_then(|c| c.get("citation_id"))
            .and_then(Value::as_str)
        {
            out.push_str(&format!("- citation_id: `{citation_id}`\n"));
        }
        if let Some(trust_tier) = result.get("trust_tier").and_then(Value::as_str) {
            out.push_str(&format!("- trust_tier: `{trust_tier}`\n"));
        }
        if let Some(text) = result.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                out.push('\n');
                out.push_str(text.trim());
                out.push('\n');
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// Compact scope/degradation footer lines for the markdown renderers.
/// Kept terse so token budgets are unaffected on the healthy path: one
/// line for retrieval_mode, plus warning/hint lines only when present.
fn render_scope_status_lines(out: &mut String, scope_status: Option<&Value>) {
    let Some(status) = scope_status else {
        return;
    };
    if let Some(mode) = status.get("retrieval_mode").and_then(Value::as_str) {
        out.push_str(&format!("- retrieval_mode: `{mode}`\n"));
    }
    if let Some(warnings) = status.get("warnings").and_then(Value::as_array) {
        for warning in warnings {
            if let Some(text) = warning.as_str() {
                out.push_str(&format!("- warning: {text}\n"));
            }
        }
    }
    if let Some(hint) = status.get("widen_hint").and_then(Value::as_str) {
        out.push_str(&format!("- hint: {hint}\n"));
    }
}

pub(super) fn write_rendered(path: Option<&Path>, rendered: &str) -> Result<()> {
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
    } else {
        print!("{rendered}");
    }
    Ok(())
}

pub(super) fn write_cli_log(log_dir: Option<&Path>, prefix: &str, payload: &Value) -> Result<()> {
    let Some(log_dir) = log_dir else {
        return Ok(());
    };
    std::fs::create_dir_all(log_dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
        .as_millis();
    let path = log_dir.join(format!("{prefix}_{stamp}.json"));
    let rendered = serde_json::to_string_pretty(payload)? + "\n";
    std::fs::write(path, rendered)?;
    let jsonl_path = log_dir.join(format!("{prefix}_log.jsonl"));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(jsonl_path)?;
    writeln!(file, "{}", serde_json::to_string(payload)?)?;
    Ok(())
}

pub(super) fn render_export(
    chunks: &[MemoryChunk],
    tenant: &TenantId,
    format: ExportFormat,
) -> Result<String> {
    match format {
        ExportFormat::Markdown => Ok(render_markdown_export(chunks, tenant)),
        ExportFormat::Json => Ok(serde_json::to_string_pretty(chunks)?),
        ExportFormat::Jsonl => {
            let mut out = String::new();
            for chunk in chunks {
                out.push_str(&serde_json::to_string(chunk)?);
                out.push('\n');
            }
            Ok(out)
        }
    }
}

pub(super) fn render_guardrail_block(
    scope_config: &TenantScopeConfig,
    memd_command: &str,
) -> String {
    let policy = include_str!("../../../../memd-skill/memory-policy.md");
    format!(
        "<!-- memd-guardrails:start -->\n## memd CLI Memory Guardrails\n\n\
         - Write tenant: `{}`\n\
         - Use `.memd/project_scope.json` for the repository's pinned scope.\n\
         - Refresh: `{memd_command} memory-md --tenant-id {} --project-dir . --output memory.md`.\n\
         - Retrieve when needed: `{memd_command} agent-context --tenant-id {} --query \"<task>\" --k 2 --token-budget 700 --format markdown`.\n\n\
         {policy}<!-- memd-guardrails:end -->\n",
        scope_config.write_tenant, scope_config.write_tenant, scope_config.write_tenant,
    )
}

pub(super) fn upsert_guardrail_file(path: &Path, guardrail_block: &str) -> Result<()> {
    const START: &str = "<!-- memd-guardrails:start -->";
    const END: &str = "<!-- memd-guardrails:end -->";

    let mut content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    if let (Some(start), Some(end)) = (content.find(START), content.find(END)) {
        let end_idx = end + END.len();
        content.replace_range(start..end_idx, guardrail_block);
    } else {
        if !content.trim().is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(guardrail_block);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

pub(super) fn render_markdown_export(chunks: &[MemoryChunk], tenant: &TenantId) -> String {
    let mut out = String::new();
    out.push_str("# memd export\n\n");
    out.push_str(&format!("- tenant_id: `{}`\n", tenant));
    out.push_str(&format!("- chunk_count: `{}`\n\n", chunks.len()));

    for chunk in chunks {
        out.push_str(&format!("## {}\n\n", chunk.chunk_id));
        out.push_str(&format!("- type: `{}`\n", chunk.chunk_type));
        out.push_str(&format!("- project_id: `{}`\n", chunk.project_id));
        out.push_str(&format!(
            "- timestamp_created_ms: `{}`\n",
            chunk.timestamp_created
        ));
        if let Some(path) = &chunk.source.path {
            out.push_str(&format!("- source_path: `{}`\n", path));
        }
        if chunk.tags.is_empty() {
            out.push_str("- tags: `<none>`\n\n");
        } else {
            out.push_str(&format!("- tags: `{}`\n\n", chunk.tags.join(", ")));
        }
        out.push_str("Text:\n\n");
        for line in chunk.text.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
        if chunk.text.is_empty() {
            out.push_str("> \n");
        }
        out.push('\n');
    }

    out
}
