//! CLI mode for direct operation invocation
//!
//! Provides command-line interface for manual testing and debugging
//! through the local executable.

#[cfg(test)]
use std::path::PathBuf;

use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use tracing::info;

use crate::error::{MemdError, Result};
use crate::store::metadata::MetadataStore;
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, MemoryChunk, ProjectId, Source, TenantId};

mod args;
mod batch;
mod call;
mod memory_md;
mod ops_bridge;
mod paths;
mod render;
mod search;
mod warm;

#[cfg(test)]
use crate::types::ChunkType;
#[cfg(test)]
use args::parse_chunk_type;
pub use args::{
    CliCommand, CliQueryMode, ExportFormat, SearchReranker, TenantScopeMode, WarmCommand, WarmMode,
    WarmProcessConfig,
};
use args::{ProjectScopeConfig, SearchRerankerOptions};
use batch::{read_batch_input, run_batch_jsonl, stream_batch_jsonl};
use call::parse_call_arguments;
use memory_md::{refresh_memory_md, MemoryMdOptions};
use ops_bridge::cli_call_tool;
use paths::{
    absolutize_project_dir, build_tenant_scope_config, normalize_absolute, path_is_inside,
    read_omf_input, read_stdin_to_string, reject_if_any_symlink_inside_outdir, resolve_data_dir,
    resolve_export_markdown_data_dirs,
};
#[cfg(test)]
use paths::{discover_project_data_dir_from, resolve_export_markdown_data_dirs_from};
use render::{
    render_agent_context, render_export, render_guardrail_block, render_search_payload,
    unwrap_content_payload, upsert_guardrail_file, write_cli_log, write_rendered,
};
use search::{
    apply_search_reranker, cli_agent_context_payload, cli_search_payload, export_format_name,
};
use warm::run_warm_worker;
pub use warm::{run_warm_admin, try_run_warm_client, warm_socket_path};

/// Run a CLI command
///
/// Executes the specified command against the store and prints JSON output.
pub async fn run_cli<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    cmd: CliCommand,
) -> Result<()> {
    match cmd {
        CliCommand::Add {
            tenant_id,
            text,
            chunk_type,
            project_id,
            tags,
            source_uri,
            source_path,
        } => {
            let tenant = TenantId::new(&tenant_id)?;

            // Ensure tenant directory exists
            if let Some(tm) = tenant_manager {
                tm.ensure_tenant_dir(&tenant)?;
            }

            let mut chunk = MemoryChunk::new(tenant, &text, chunk_type);

            if let Some(pid) = project_id {
                chunk = chunk.with_project(ProjectId::new(Some(pid)));
            }

            if let Some(t) = tags {
                chunk = chunk.with_tags(t);
            }

            if source_uri.is_some() || source_path.is_some() {
                let source = Source {
                    uri: source_uri,
                    path: source_path,
                    ..Default::default()
                };
                chunk = chunk.with_source(source);
            }

            let chunk_id = store.add(chunk).await?;
            info!(chunk_id = %chunk_id, "chunk added");

            let output = json!({
                "chunk_id": chunk_id.to_string()
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Search {
            tenant_id,
            query,
            k,
            project_id,
            compact,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
            output,
            reranker,
            reranker_model,
            reranker_device,
            reranker_batch_size,
            reranker_timeout_seconds,
            reranker_python,
            warm: _,
        } => {
            let mut payload = cli_search_payload(
                store,
                tenant_id,
                project_id,
                query.clone(),
                k,
                compact,
                token_budget,
                mode,
                no_text,
                include_artifact,
            )
            .await?;
            payload = apply_search_reranker(
                payload,
                &query,
                &SearchRerankerOptions {
                    reranker,
                    model: reranker_model,
                    device: reranker_device,
                    batch_size: reranker_batch_size,
                    timeout_seconds: reranker_timeout_seconds,
                    python: reranker_python,
                },
            )?;
            write_rendered(output.as_deref(), &render_search_payload(&payload, format)?)?;
        }

        CliCommand::AgentContext {
            tenant_id,
            project_id,
            query,
            k,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
            output,
            log_dir,
            warm: _,
        } => {
            let payload = cli_agent_context_payload(
                store,
                &tenant_id,
                project_id.as_deref(),
                &query,
                k,
                token_budget,
                mode,
                no_text,
                include_artifact,
            )
            .await?;
            write_cli_log(log_dir.as_deref(), "memd_search", &payload)?;
            write_rendered(output.as_deref(), &render_agent_context(&payload, format)?)?;
        }

        CliCommand::MemoryMd {
            tenant_id,
            project_id,
            project_dir,
            output,
            project_limit,
            global_limit,
            candidate_k,
        } => {
            let result = refresh_memory_md(
                store,
                MemoryMdOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    output,
                    project_limit,
                    global_limit,
                    candidate_k,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::Call {
            tool,
            json,
            input,
            output,
            warm: _,
        } => {
            let arguments = parse_call_arguments(json.as_deref(), input.as_deref())?;
            let value = cli_call_tool(store, tenant_manager, &tool, arguments)
                .await
                .map_err(|e| MemdError::ProtocolError(e.to_string()))?;
            let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
            write_rendered(
                output.as_deref(),
                &(serde_json::to_string_pretty(&payload)? + "\n"),
            )?;
        }

        CliCommand::Batch {
            jsonl,
            stream,
            continue_on_error,
            output,
        } => {
            if stream {
                stream_batch_jsonl(
                    store,
                    tenant_manager,
                    jsonl.as_deref(),
                    output.as_deref(),
                    continue_on_error,
                )
                .await?;
            } else {
                let input = read_batch_input(jsonl.as_deref())?;
                let rendered =
                    run_batch_jsonl(store, tenant_manager, &input, continue_on_error).await?;
                write_rendered(output.as_deref(), &rendered)?;
            }
        }

        CliCommand::Warm { .. } => {
            return Err(MemdError::ValidationError(
                "internal error: warm admin commands must run before store initialization"
                    .to_string(),
            ));
        }

        CliCommand::WarmWorker { socket } => {
            run_warm_worker(store, tenant_manager, &socket).await?;
        }

        CliCommand::Get {
            tenant_id,
            chunk_id,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let cid = ChunkId::parse(&chunk_id)?;
            let chunk = store.get(&tenant, &cid).await?;

            if let Some(c) = chunk {
                info!(chunk_id = %cid, "chunk found");
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                info!(chunk_id = %cid, "chunk not found");
                println!("null");
            }
        }

        CliCommand::Delete {
            tenant_id,
            chunk_id,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let cid = ChunkId::parse(&chunk_id)?;
            let deleted = store.delete(&tenant, &cid).await?;

            info!(chunk_id = %cid, deleted = deleted, "delete operation");

            let output = json!({
                "deleted": deleted
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Stats { tenant_id } => {
            let tenant = TenantId::new(&tenant_id)?;
            let stats = store.stats(&tenant).await?;

            info!(tenant_id = %tenant, "stats retrieved");

            let mut output = json!({
                "total_chunks": stats.total_chunks,
                "deleted_chunks": stats.deleted_chunks,
                "chunk_types": stats.chunk_types,
            });

            // Add disk stats if available
            if let Some(tm) = tenant_manager {
                if let Ok(disk_stats) = tm.tenant_disk_stats(&tenant) {
                    output["disk_stats"] = json!({
                        "total_bytes": disk_stats.total_bytes,
                        "segment_count": disk_stats.segment_count,
                    });
                }
            }

            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Export {
            tenant_id,
            format,
            output,
            page_size,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let page_size = page_size.max(1).min(10_000);
            let chunks = collect_all_chunks(store, &tenant, page_size).await?;
            let rendered = render_export(&chunks, &tenant, format)?;

            if let Some(path) = output {
                std::fs::write(&path, rendered)?;
                let summary = json!({
                    "tenant_id": tenant.to_string(),
                    "format": export_format_name(format),
                    "chunks_exported": chunks.len(),
                    "output_path": path,
                });
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!("{rendered}");
            }
        }

        CliCommand::ExportMarkdown {
            tenant_id,
            outdir,
            project_id,
            include_history,
            data_dir,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let ps = store.as_persistent().ok_or_else(|| {
                crate::error::MemdError::StorageError(
                    "export-markdown requires a persistent store".to_string(),
                )
            })?;

            // Containment guard: refuse if the user pointed `--outdir` at
            // a path inside memd's data directory. We use a textual
            // normalise (no `canonicalize`) so the guard works before the
            // outdir exists — std `Path::canonicalize` would error out.
            // Containment guard refuses if `outdir` is inside ANY of
            // the known memd data directories. When `--data-dir` is
            // explicit, the list is just that path. When it's absent,
            // the list is `[<discovered from tenant_scope.json>?,
            // $HOME/.memd/data]` — discovery AUGMENTS the default
            // fallback, it doesn't replace it, so an untrusted
            // ancestor config can't turn off the guard for the
            // default-install data directory (Codex Item 4 HIGH).
            let effective_data_dirs = resolve_export_markdown_data_dirs(data_dir.as_deref())?;
            let outdir_abs = normalize_absolute(&outdir);
            for candidate in &effective_data_dirs {
                let data_dir_abs = normalize_absolute(candidate);
                if path_is_inside(&outdir_abs, &data_dir_abs) {
                    return Err(crate::error::MemdError::ValidationError(format!(
                        "refusing to write markdown export into memd data directory: \
                         outdir={} data_dir={}",
                        outdir_abs.display(),
                        data_dir_abs.display()
                    )));
                }
            }

            // Walk metadata in pages so a tenant with > 10k chunks
            // doesn't silently lose its tail. `list` supports an
            // offset; `list_recent_for_project` doesn't, but the
            // project-scoped branch already limits the candidate set by
            // project, so a 10k-row page is usually sufficient. For
            // whole-tenant exports we paginate `list` until we stop
            // seeing new rows. (Codex G3 review MEDIUM: silent cap.)
            const PAGE_SIZE: usize = 10_000;
            let mut metas = Vec::new();
            match project_id.as_deref() {
                Some(pid) => {
                    metas = ps
                        .metadata()
                        .list_recent_for_project(&tenant, Some(pid), PAGE_SIZE)?;
                }
                None => {
                    let mut offset = 0;
                    loop {
                        let page = ps.metadata().list(&tenant, PAGE_SIZE, offset)?;
                        if page.is_empty() {
                            break;
                        }
                        let got = page.len();
                        metas.extend(page);
                        if got < PAGE_SIZE {
                            break;
                        }
                        offset += got;
                    }
                }
            };
            let mut chunks = Vec::with_capacity(metas.len());
            for meta in metas {
                // Match the G2 handler's visibility rule: only Final,
                // non-superseded rows; tier filter depends on flag.
                if meta.status != crate::types::ChunkStatus::Final
                    || meta.lifecycle.superseded_by.is_some()
                {
                    continue;
                }
                if !include_history
                    && meta.lifecycle.tier == crate::types::lifecycle::MemoryTier::History
                {
                    continue;
                }
                if let Some(pid) = project_id.as_deref() {
                    if meta.project_id.as_deref() != Some(pid) {
                        continue;
                    }
                }
                if let Some(chunk) = <crate::store::persistent::PersistentStore as Store>::get(
                    ps,
                    &tenant,
                    &meta.chunk_id,
                )
                .await?
                {
                    chunks.push(chunk);
                }
            }

            let files = crate::mcp::markdown_export::render_markdown_tree(&chunks);
            std::fs::create_dir_all(&outdir_abs).map_err(|e| {
                crate::error::MemdError::StorageError(format!(
                    "failed to create outdir {}: {e}",
                    outdir_abs.display()
                ))
            })?;

            let mut written_paths: Vec<String> = Vec::with_capacity(files.len());
            for f in &files {
                // RenderedFile.path is a POSIX relative string; join it
                // onto the outdir so we write into the right bucket.
                let mut target = outdir_abs.clone();
                for segment in f.path.split('/').filter(|s| !s.is_empty()) {
                    target.push(segment);
                }
                // Refuse before any filesystem write if a pre-existing
                // symlink planted inside outdir would redirect the
                // write off to an attacker-chosen path. Runs before
                // create_dir_all because create_dir_all happily walks
                // through existing symlinked directories (Item 3).
                reject_if_any_symlink_inside_outdir(&target, &outdir_abs)?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        crate::error::MemdError::StorageError(format!(
                            "failed to create parent {}: {e}",
                            parent.display()
                        ))
                    })?;
                }
                std::fs::write(&target, &f.content).map_err(|e| {
                    crate::error::MemdError::StorageError(format!(
                        "failed to write {}: {e}",
                        target.display()
                    ))
                })?;
                written_paths.push(target.display().to_string());
            }

            let summary = json!({
                "tenant_id": tenant.to_string(),
                "outdir": outdir_abs.display().to_string(),
                "files_written": written_paths.len(),
                "paths": written_paths,
            });
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }

        CliCommand::ExportOmf {
            tenant_id,
            project_id,
            output,
            include_history,
            include_superseded,
            include_expired,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let ps = store.as_persistent().ok_or_else(|| {
                crate::error::MemdError::StorageError(
                    "export-omf requires a persistent store".to_string(),
                )
            })?;

            let opts = crate::omf::export::ExportOptions {
                project_id,
                include_history,
                include_superseded,
                include_expired,
            };
            let doc = crate::omf::export::export_omf(ps, &tenant, opts).await?;
            let rendered = serde_json::to_string_pretty(&doc)?;

            if let Some(path) = output {
                std::fs::write(&path, format!("{rendered}\n"))?;
                let summary = json!({
                    "tenant_id": tenant.to_string(),
                    "memories": doc.memories.len(),
                    "output_path": path,
                });
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("{rendered}");
            }
        }

        CliCommand::ImportOmf {
            tenant_id,
            input,
            include_archived,
            fuzzy_threshold,
            dry_run,
        } => {
            let tenant = TenantId::new(&tenant_id)?;

            // Read + parse BEFORE any side effect so a malformed input
            // or a missing file errors out without touching disk. Only
            // the non-dry-run branch calls `ensure_tenant_dir` — dry-run
            // stays fully read-only, matching preview_omf_import's operation
            // semantics (Codex F6 review MEDIUM).
            let raw = read_omf_input(input.as_deref())?;
            let doc: crate::omf::OmfDocument = serde_json::from_str(&raw).map_err(|e| {
                crate::error::MemdError::ValidationError(format!(
                    "input is not a valid OMF 1.0 document: {e}"
                ))
            })?;

            let ps = store.as_persistent().ok_or_else(|| {
                crate::error::MemdError::StorageError(
                    "import-omf requires a persistent store".to_string(),
                )
            })?;
            let opts = crate::omf::import::ImportOptions {
                include_archived,
                fuzzy_threshold,
            };

            if dry_run {
                let preview =
                    crate::omf::import::preview_omf_import(ps, &tenant, &doc, opts).await?;
                let output = json!({
                    "tenant_id": tenant.to_string(),
                    "dry_run": true,
                    "total": preview.total,
                    "to_import": preview.to_import,
                    "duplicates": preview.duplicates,
                    "filtered": preview.filtered,
                    "unscoped": preview.unscoped,
                    "by_project": preview.by_project,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                // Real import: now we can materialise the tenant dir.
                // Done AFTER parse so bad input doesn't create artefacts
                // on disk.
                if let Some(tm) = tenant_manager {
                    tm.ensure_tenant_dir(&tenant)?;
                }
                let result = crate::omf::import::import_omf(ps, &tenant, &doc, opts).await?;
                let output = json!({
                    "tenant_id": tenant.to_string(),
                    "dry_run": false,
                    "total": result.total,
                    "imported": result.imported,
                    "duplicates": result.duplicates,
                    "skipped": result.skipped,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        }

        CliCommand::Init {
            tenant_id,
            scope,
            allow_tenants,
            project_dir,
            project_id,
            memd_command,
            memd_data_dir,
            write_agent_files,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let project_dir = absolutize_project_dir(&project_dir)?;
            let memd_dir = project_dir.join(".memd");
            std::fs::create_dir_all(&memd_dir)?;

            let effective_data_dir = resolve_data_dir(memd_data_dir.as_deref())?;
            let scope_config = build_tenant_scope_config(
                tenant.as_str(),
                scope,
                allow_tenants.as_deref(),
                &effective_data_dir,
            )?;
            let guardrail_block = render_guardrail_block(&scope_config, &memd_command);

            let guardrail_path = memd_dir.join("memory_guardrails.md");
            let tenant_scope_path = memd_dir.join("tenant_scope.json");
            let project_scope_path = memd_dir.join("project_scope.json");
            let project_scope = ProjectScopeConfig {
                tenant_id: tenant.to_string(),
                project_id,
                read_tenants: scope_config.read_tenants.clone(),
                interface: "cli".to_string(),
                cli_command: memd_command.clone(),
                agent_context_output: ".memd/context.md".to_string(),
                project_dir: project_dir.display().to_string(),
            };

            std::fs::write(&guardrail_path, &guardrail_block)?;
            std::fs::write(
                &tenant_scope_path,
                format!("{}\n", serde_json::to_string_pretty(&scope_config)?),
            )?;
            std::fs::write(
                &project_scope_path,
                format!("{}\n", serde_json::to_string_pretty(&project_scope)?),
            )?;

            let mut updated_files = Vec::new();

            if write_agent_files {
                let agents_path = project_dir.join("AGENTS.md");
                let claude_path = project_dir.join("CLAUDE.md");

                upsert_guardrail_file(&agents_path, &guardrail_block)?;
                upsert_guardrail_file(&claude_path, &guardrail_block)?;
                updated_files.push(agents_path);
                updated_files.push(claude_path);
            }

            let result = json!({
                "tenant_id": tenant.to_string(),
                "project_dir": project_dir,
                "generated": {
                    "guardrail_markdown": guardrail_path,
                    "tenant_scope": tenant_scope_path,
                    "project_scope": project_scope_path
                },
                "scope": scope_config,
                "updated_files": updated_files,
                "interface": "cli"
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

async fn collect_all_chunks<S: Store>(
    store: &S,
    tenant: &TenantId,
    page_size: usize,
) -> Result<Vec<MemoryChunk>> {
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        let page = store.list_chunks(tenant, page_size, offset).await?;
        if page.is_empty() {
            break;
        }
        chunks.extend(page);
        offset += page_size;
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;

    #[test]
    fn parse_chunk_types() {
        assert!(matches!(parse_chunk_type("code"), Ok(ChunkType::Code)));
        assert!(matches!(parse_chunk_type("DOC"), Ok(ChunkType::Doc)));
        assert!(matches!(parse_chunk_type("Trace"), Ok(ChunkType::Trace)));
        assert!(parse_chunk_type("invalid").is_err());
    }

    fn unique_test_file(ext: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("memd_export_test_{now}.{ext}"))
    }

    #[tokio::test]
    async fn export_markdown_writes_human_readable_output() {
        let store = MemoryStore::new();
        let tenant = TenantId::new("export_tenant").unwrap();
        let chunk = MemoryChunk::new(tenant, "export me", ChunkType::Doc)
            .with_tags(vec!["ctx:doc".to_string(), "quality".to_string()])
            .with_project(ProjectId::from("demo_project"));
        store.add(chunk).await.unwrap();

        let output_path = unique_test_file("md");
        run_cli(
            &store,
            None,
            CliCommand::Export {
                tenant_id: "export_tenant".to_string(),
                format: ExportFormat::Markdown,
                output: Some(output_path.clone()),
                page_size: 100,
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("# memd export"));
        assert!(content.contains("export me"));
        assert!(content.contains("demo_project"));
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn export_json_writes_chunk_array() {
        let store = MemoryStore::new();
        let tenant = TenantId::new("export_json_tenant").unwrap();
        let chunk = MemoryChunk::new(tenant, "json export chunk", ChunkType::Decision);
        store.add(chunk).await.unwrap();

        let output_path = unique_test_file("json");
        run_cli(
            &store,
            None,
            CliCommand::Export {
                tenant_id: "export_json_tenant".to_string(),
                format: ExportFormat::Json,
                output: Some(output_path.clone()),
                page_size: 100,
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(&output_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let rows = parsed.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"], "json export chunk");
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn agent_context_builds_cli_prefetch_payload() {
        let store = MemoryStore::new();
        let tenant = TenantId::new("agent_context_tenant").unwrap();
        let chunk = MemoryChunk::new(
            tenant,
            "experience_id=mt-schema-defaults-v1 repair rule: shared defaults belong in one schema layer",
            ChunkType::Research,
        )
        .with_project(ProjectId::from("schema_defaults"));
        store.add(chunk).await.unwrap();

        let payload = cli_agent_context_payload(
            &store,
            "agent_context_tenant",
            Some("schema_defaults"),
            &[
                "mt-schema-defaults-v1 repair rules".to_string(),
                "schema defaults repair rules".to_string(),
            ],
            5,
            1200,
            CliQueryMode::Generic,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(payload["interface"], "cli_prefetch");
        assert!(payload["result_count"].as_u64().unwrap_or(0) >= 1);
        let markdown = render_agent_context(&payload, ExportFormat::Markdown).unwrap();
        assert!(markdown.contains("mt-schema-defaults-v1"));
        assert!(markdown.contains("interface: `cli_only`"));

        let dir = tempdir().unwrap();
        write_cli_log(Some(dir.path()), "memd_search", &payload).unwrap();
        let files = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(files.iter().any(|name| name.starts_with("memd_search_")));
        assert!(files.iter().any(|name| name == "memd_search_log.jsonl"));
    }

    #[tokio::test]
    async fn memory_md_writes_project_and_global_takeaways() {
        let store = MemoryStore::new();
        let tenant = TenantId::new("memory_md_tenant").unwrap();
        store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    "project takeaway decision: use project-scoped metadata before payload reads",
                    ChunkType::Decision,
                )
                .with_project(ProjectId::from("memory_md_project"))
                .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
            )
            .await
            .unwrap();
        store
            .add(
                MemoryChunk::new(
                    tenant,
                    "machine wide reusable takeaway: stop stale warm workers before replacing the bundled binary",
                    ChunkType::Summary,
                )
                .with_tags(vec!["kind:finish".to_string(), "priority:7".to_string()]),
            )
            .await
            .unwrap();

        let dir = tempdir().unwrap();
        run_cli(
            &store,
            None,
            CliCommand::MemoryMd {
                tenant_id: Some("memory_md_tenant".to_string()),
                project_id: Some("memory_md_project".to_string()),
                project_dir: dir.path().to_path_buf(),
                output: PathBuf::from("memory.md"),
                project_limit: 10,
                global_limit: 10,
                candidate_k: 10,
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("memory.md")).unwrap();
        assert!(content.contains("## Project Takeaways"));
        assert!(content.contains("memory_md_project"));
        assert!(content.contains("## Machine-Wide Takeaways"));
        assert!(content.contains("memory_md_tenant"));
        assert!(content.contains("priority:"));
    }

    #[tokio::test]
    async fn call_invokes_former_tool_operations_without_server() {
        let store = MemoryStore::new();

        let add_value = cli_call_tool(
            &store,
            None,
            "memory.add",
            json!({
                "tenant_id": "call_tenant",
                "project_id": "call_project",
                "type": "doc",
                "text": "call parity marker: local executable operation",
                "tags": ["kind:parity"]
            }),
        )
        .await
        .unwrap();
        let add_payload = unwrap_content_payload(add_value).unwrap();
        let chunk_id = add_payload["chunk_id"].as_str().unwrap().to_string();

        let get_value = cli_call_tool(
            &store,
            None,
            "memory.get",
            json!({
                "tenant_id": "call_tenant",
                "chunk_id": chunk_id
            }),
        )
        .await
        .unwrap();
        let get_payload = unwrap_content_payload(get_value).unwrap();
        assert!(get_payload["chunk"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("local executable operation")));

        let task_value = cli_call_tool(
            &store,
            None,
            "task.start",
            json!({
                "tenant_id": "call_tenant",
                "project_id": "call_project",
                "goal": "prove CLI call parity"
            }),
        )
        .await
        .unwrap();
        let task_payload = unwrap_content_payload(task_value).unwrap();
        assert!(task_payload["task_id"].as_str().is_some());
    }

    #[test]
    fn warm_socket_path_is_stable_and_config_scoped() {
        let dir = tempdir().unwrap();
        let config = WarmProcessConfig {
            data_dir: dir.path().join("data"),
            config_path: None,
            embedding_model: "all-minilm".to_string(),
            search_variant: "hybrid-feature".to_string(),
        };

        let same = warm_socket_path(&config);
        assert_eq!(same, warm_socket_path(&config));
        assert!(same.ends_with("memd.sock"));

        let mut dense = config.clone();
        dense.search_variant = "dense-only".to_string();
        assert_ne!(same, warm_socket_path(&dense));
    }

    #[test]
    fn warm_socket_path_uses_short_temp_path_for_long_data_dirs() {
        let config = WarmProcessConfig {
            data_dir: PathBuf::from("/tmp").join("a".repeat(180)),
            config_path: None,
            embedding_model: "all-minilm".to_string(),
            search_variant: "hybrid-feature".to_string(),
        };

        let socket = warm_socket_path(&config);
        assert!(socket.to_string_lossy().len() < 100);
        assert!(socket.starts_with(std::env::temp_dir().join("memd-warm")));
    }

    #[tokio::test]
    async fn batch_jsonl_runs_multiple_calls_through_one_store() {
        let store = MemoryStore::new();
        let input = r#"
{"tool":"memory.add","arguments":{"tenant_id":"batch_tenant","project_id":"batch_project","type":"doc","text":"batch marker one"}}
{"tool":"memory.stats","arguments":{"tenant_id":"batch_tenant"}}
"#;

        let rendered = run_batch_jsonl(&store, None, input, false).await.unwrap();
        let rows = rendered
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[1]["ok"], true);
        assert_eq!(rows[1]["tool"], "memory.stats");
        assert!(rows[1]["result"]["total_chunks"].as_u64().unwrap_or(0) >= 1);
    }

    #[tokio::test]
    async fn init_writes_cli_guardrails() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        run_cli(
            &store,
            None,
            CliCommand::Init {
                tenant_id: "demo_tenant".to_string(),
                scope: TenantScopeMode::Local,
                allow_tenants: None,
                project_dir: project_dir.clone(),
                project_id: Some("demo_project".to_string()),
                memd_command: "memd".to_string(),
                memd_data_dir: Some(PathBuf::from("/tmp/memd-data")),
                write_agent_files: true,
            },
        )
        .await
        .unwrap();

        let guardrails =
            std::fs::read_to_string(project_dir.join(".memd/memory_guardrails.md")).unwrap();
        assert!(guardrails.contains("demo_tenant"));
        assert!(guardrails.contains("memory-md"));
        assert!(guardrails.contains("memory.md"));
        assert!(guardrails.contains("memd agent-context"));
        assert!(guardrails.contains("memd add"));
        assert!(guardrails.contains("Read scope mode: `local`"));
        assert!(guardrails.contains(".memd/project_scope.json"));
        assert!(!project_dir.join(".memd/mcp_config_claude.json").exists());
        assert!(!project_dir.join(".memd/mcp_config_codex.toml").exists());

        let tenant_scope: Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tenant_scope["scope"], "local");
        assert_eq!(tenant_scope["read_tenants"][0], "demo_tenant");

        let project_scope: Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/project_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(project_scope["tenant_id"], "demo_tenant");
        assert_eq!(project_scope["project_id"], "demo_project");
        assert_eq!(project_scope["interface"], "cli");
        assert_eq!(project_scope["cli_command"], "memd");

        let agents = std::fs::read_to_string(project_dir.join("AGENTS.md")).unwrap();
        assert!(agents.contains("memd-guardrails:start"));
    }

    #[tokio::test]
    async fn init_upserts_guardrail_block_without_duplication() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        for tenant in ["tenant_one", "tenant_two"] {
            run_cli(
                &store,
                None,
                CliCommand::Init {
                    tenant_id: tenant.to_string(),
                    scope: TenantScopeMode::Local,
                    allow_tenants: None,
                    project_dir: project_dir.clone(),
                    project_id: Some("shared_project".to_string()),
                    memd_command: "memd".to_string(),
                    memd_data_dir: None,
                    write_agent_files: true,
                },
            )
            .await
            .unwrap();
        }

        let agents = std::fs::read_to_string(project_dir.join("AGENTS.md")).unwrap();
        let marker_count = agents.matches("memd-guardrails:start").count();
        assert_eq!(marker_count, 1);
        assert!(agents.contains("tenant_two"));
    }

    #[tokio::test]
    async fn init_allowlist_scope_writes_read_set() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        run_cli(
            &store,
            None,
            CliCommand::Init {
                tenant_id: "primary".to_string(),
                scope: TenantScopeMode::Allowlist,
                allow_tenants: Some(vec!["tenant_a".to_string(), "tenant_b".to_string()]),
                project_dir: project_dir.clone(),
                project_id: Some("allowlist_project".to_string()),
                memd_command: "memd".to_string(),
                memd_data_dir: None,
                write_agent_files: false,
            },
        )
        .await
        .unwrap();

        let tenant_scope: Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tenant_scope["scope"], "allowlist");
        let read_tenants = tenant_scope["read_tenants"].as_array().unwrap();
        assert_eq!(read_tenants.len(), 3);
        assert!(read_tenants.iter().any(|v| v == "primary"));
        assert!(read_tenants.iter().any(|v| v == "tenant_a"));
        assert!(read_tenants.iter().any(|v| v == "tenant_b"));
    }

    #[tokio::test]
    async fn init_global_scope_discovers_tenants_from_data_dir() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(data_dir.join("tenants").join("shared_a")).unwrap();
        std::fs::create_dir_all(data_dir.join("tenants").join("shared_b")).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        run_cli(
            &store,
            None,
            CliCommand::Init {
                tenant_id: "primary".to_string(),
                scope: TenantScopeMode::Global,
                allow_tenants: None,
                project_dir: project_dir.clone(),
                project_id: Some("global_project".to_string()),
                memd_command: "memd".to_string(),
                memd_data_dir: Some(data_dir.clone()),
                write_agent_files: false,
            },
        )
        .await
        .unwrap();

        let tenant_scope: Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tenant_scope["scope"], "global");
        let read_tenants = tenant_scope["read_tenants"].as_array().unwrap();
        assert!(read_tenants.iter().any(|v| v == "primary"));
        assert!(read_tenants.iter().any(|v| v == "shared_a"));
        assert!(read_tenants.iter().any(|v| v == "shared_b"));
    }

    // --- Item 4: export-markdown --data-dir auto-discovery ---

    #[tokio::test]
    async fn init_local_scope_persists_data_dir_in_tenant_scope() {
        // Pins the behaviour-change introduced for Item 4: `data_dir`
        // is now recorded in `tenant_scope.json` for every scope mode,
        // not just `global`, so `memd export-markdown` can auto-discover
        // it without forcing the user to pass `--data-dir`.
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        run_cli(
            &store,
            None,
            CliCommand::Init {
                tenant_id: "t_local".to_string(),
                scope: TenantScopeMode::Local,
                allow_tenants: None,
                project_dir: project_dir.clone(),
                project_id: Some("p".to_string()),
                memd_command: "memd".to_string(),
                memd_data_dir: Some(PathBuf::from("/tmp/memd-data-local")),
                write_agent_files: false,
            },
        )
        .await
        .unwrap();

        let tenant_scope: Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tenant_scope["scope"], "local");
        assert_eq!(tenant_scope["data_dir"], "/tmp/memd-data-local");
    }

    #[test]
    fn discover_project_data_dir_returns_none_when_no_memd_dir() {
        let dir = tempdir().unwrap();
        assert!(discover_project_data_dir_from(dir.path()).is_none());
    }

    #[test]
    fn discover_project_data_dir_returns_data_dir_from_tenant_scope() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/abs/path/to/data"}"#,
        )
        .unwrap();
        let discovered = discover_project_data_dir_from(dir.path()).unwrap();
        assert_eq!(discovered, PathBuf::from("/abs/path/to/data"));
    }

    #[test]
    fn discover_project_data_dir_returns_none_when_field_missing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"]}"#,
        )
        .unwrap();
        assert!(discover_project_data_dir_from(dir.path()).is_none());
    }

    #[test]
    fn discover_project_data_dir_returns_none_on_malformed_json() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/tenant_scope.json"), "{not json").unwrap();
        assert!(discover_project_data_dir_from(dir.path()).is_none());
    }

    #[test]
    fn discover_project_data_dir_walks_up_to_nearest_ancestor() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        let nested = project.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(project.join(".memd")).unwrap();
        std::fs::write(
            project.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/discovered"}"#,
        )
        .unwrap();
        let discovered = discover_project_data_dir_from(&nested).unwrap();
        assert_eq!(discovered, PathBuf::from("/discovered"));
    }

    #[test]
    fn discover_project_data_dir_resolves_relative_path_against_memd_parent() {
        // When `data_dir` in the JSON is a relative path, resolve it
        // relative to the directory containing `.memd/`, not relative
        // to the caller's CWD. This matches what `memd init` intends
        // when a user passes a project-relative `--data-dir`.
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".memd")).unwrap();
        std::fs::write(
            project.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"subdir/data"}"#,
        )
        .unwrap();
        let discovered = discover_project_data_dir_from(&project).unwrap();
        assert_eq!(discovered, project.join("subdir").join("data"));
    }

    #[test]
    fn resolve_export_markdown_data_dirs_prefers_explicit_arg() {
        // When --data-dir is explicit, the guard checks ONLY that path
        // (single-element vec). The caller's declared intent overrides
        // any ambient discovery and the home default.
        let explicit = PathBuf::from("/explicit/path");
        let resolved = resolve_export_markdown_data_dirs(Some(&explicit)).unwrap();
        assert_eq!(resolved, vec![explicit]);
    }

    #[test]
    fn resolve_export_markdown_data_dirs_from_uses_discovery_alongside_home_default() {
        // Regression for Codex Item 4 HIGH: when --data-dir is absent,
        // discovery must AUGMENT the home default, not replace it. An
        // ancestor config with `data_dir` = `/foo` must not silently
        // turn off the guard for `$HOME/.memd/data`.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/discovered/data"}"#,
        )
        .unwrap();
        let resolved = resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
        let home_default = dirs::home_dir().unwrap().join(".memd").join("data");
        assert!(
            resolved.contains(&PathBuf::from("/discovered/data")),
            "expected discovered path in list, got {:?}",
            resolved
        );
        assert!(
            resolved.contains(&home_default),
            "expected home default in list, got {:?}",
            resolved
        );
    }

    #[test]
    fn resolve_export_markdown_data_dirs_from_explicit_beats_discovery() {
        // Explicit --data-dir is a single-element vec; neither
        // discovery nor home default is appended. The caller takes
        // responsibility for the path they asked the guard to check.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/not-used"}"#,
        )
        .unwrap();
        let explicit = PathBuf::from("/explicit/wins");
        let resolved =
            resolve_export_markdown_data_dirs_from(Some(&explicit), Some(dir.path())).unwrap();
        assert_eq!(resolved, vec![explicit]);
    }

    #[test]
    fn resolve_export_markdown_data_dirs_from_falls_back_to_home_when_no_project() {
        let dir = tempdir().unwrap();
        let resolved = resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
        let home_default = dirs::home_dir().unwrap().join(".memd").join("data");
        assert_eq!(resolved, vec![home_default]);
    }

    #[test]
    fn discover_project_data_dir_inner_broken_config_stops_walk() {
        // Regression for Codex Item 4 MEDIUM #2: an inner project
        // whose `.memd/tenant_scope.json` is missing `data_dir` must
        // NOT silently inherit the outer project's value. Discovery
        // treats the first-found `.memd/tenant_scope.json` as the
        // project boundary.
        let dir = tempdir().unwrap();
        let outer = dir.path().join("outer");
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(outer.join(".memd")).unwrap();
        std::fs::create_dir_all(inner.join(".memd")).unwrap();
        // Outer has a valid config…
        std::fs::write(
            outer.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/outer-data"}"#,
        )
        .unwrap();
        // …but the inner project's config is missing data_dir.
        std::fs::write(
            inner.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"]}"#,
        )
        .unwrap();
        assert!(
            discover_project_data_dir_from(&inner).is_none(),
            "inner broken config must stop walk and not return outer's data_dir"
        );
    }

    #[test]
    fn resolve_data_dir_absolutizes_relative_explicit_arg() {
        // Regression for Codex Item 4 MEDIUM #3: `memd init` must
        // persist an absolute path even when the caller passed a
        // relative `--memd-data-dir`. Without this, later auto-
        // discovery would reinterpret the relative value against the
        // project root, which differs from the user's CWD at init
        // time.
        let relative = PathBuf::from("rel/data");
        let resolved = resolve_data_dir(Some(&relative)).unwrap();
        assert!(
            resolved.is_absolute(),
            "resolved must be absolute; got {}",
            resolved.display()
        );
        assert!(
            resolved.ends_with("rel/data"),
            "resolved must still end in the supplied segments; got {}",
            resolved.display()
        );
    }

    #[test]
    fn resolve_data_dir_leaves_absolute_explicit_arg_unchanged() {
        let absolute = PathBuf::from("/already/abs/data");
        let resolved = resolve_data_dir(Some(&absolute)).unwrap();
        assert_eq!(resolved, absolute);
    }

    // --- Item 3: G3 symlink hardening ---

    #[test]
    fn reject_if_any_symlink_inside_outdir_accepts_regular_files() {
        // Baseline — a normal file tree under outdir passes.
        let dir = tempdir().unwrap();
        let outdir = dir.path().to_path_buf();
        std::fs::create_dir_all(outdir.join("a/b")).unwrap();
        std::fs::write(outdir.join("a/b/c.md"), "content").unwrap();
        let target = outdir.join("a/b/c.md");
        reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap();
    }

    #[test]
    fn reject_if_any_symlink_inside_outdir_tolerates_nonexistent_components() {
        // Non-existent components are fine — create_dir_all will
        // materialise them freshly, so they can't be symlinks.
        let dir = tempdir().unwrap();
        let outdir = dir.path().to_path_buf();
        let target = outdir.join("never").join("existed").join("yet.md");
        reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reject_if_any_symlink_inside_outdir_refuses_leaf_symlink() {
        // Attacker-planted leaf symlink inside outdir must be refused.
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let outdir = dir.path().join("outdir");
        std::fs::create_dir_all(outdir.join("a/b")).unwrap();
        let victim = dir.path().join("victim.md");
        std::fs::write(&victim, "pre-existing victim content").unwrap();
        symlink(&victim, outdir.join("a/b/leaf.md")).unwrap();

        let target = outdir.join("a/b/leaf.md");
        let err = reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap_err();
        assert!(
            matches!(err, crate::error::MemdError::ValidationError(_)),
            "expected ValidationError, got {err:?}"
        );
        // Critical: the victim file must NOT have been touched.
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "pre-existing victim content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reject_if_any_symlink_inside_outdir_refuses_intermediate_symlink() {
        // Attacker-planted directory symlink mid-path must be refused.
        // Without the guard, create_dir_all would happily step through
        // the symlink and std::fs::write would hit the attacker's dir.
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let outdir = dir.path().join("outdir");
        std::fs::create_dir_all(&outdir).unwrap();
        let victim_dir = dir.path().join("victim_dir");
        std::fs::create_dir_all(&victim_dir).unwrap();
        symlink(&victim_dir, outdir.join("sub")).unwrap();

        let target = outdir.join("sub").join("x.md");
        let err = reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap_err();
        assert!(matches!(err, crate::error::MemdError::ValidationError(_)));
        assert!(
            !target.exists() || !victim_dir.join("x.md").exists(),
            "victim dir must not have been written into",
        );
    }

    #[cfg(unix)]
    #[test]
    fn reject_if_any_symlink_inside_outdir_permits_symlinked_outdir_itself() {
        // The outdir ITSELF is allowed to be a symlink — users may
        // legitimately point `--outdir` at a symlinked exports dir
        // they own. We only refuse symlinks planted BELOW outdir.
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let real_outdir = dir.path().join("real");
        std::fs::create_dir_all(&real_outdir).unwrap();
        let symlink_outdir = dir.path().join("linked");
        symlink(&real_outdir, &symlink_outdir).unwrap();

        let target = symlink_outdir.join("sub").join("x.md");
        reject_if_any_symlink_inside_outdir(&target, &symlink_outdir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reject_if_any_symlink_inside_outdir_fails_closed_on_permission_denied() {
        // Regression for Codex Item 3 LOW: abnormal filesystem states
        // (PermissionDenied, ELOOP, other I/O errors) must fail closed,
        // not silently skip the guard. An attacker-crafted directory
        // mode that denies symlink_metadata access must not become a
        // way to bypass the check.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let outdir = dir.path().join("outdir");
        std::fs::create_dir_all(outdir.join("locked")).unwrap();
        // Make the "locked" directory unreadable so symlink_metadata on
        // its children fails with EACCES, not ENOENT.
        std::fs::set_permissions(
            outdir.join("locked"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        let target = outdir.join("locked").join("inner").join("x.md");
        let result = reject_if_any_symlink_inside_outdir(&target, &outdir);

        // Restore perms so tempdir cleanup works regardless of outcome.
        std::fs::set_permissions(
            outdir.join("locked"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();

        let err = result.expect_err("must fail closed on EACCES");
        assert!(matches!(err, crate::error::MemdError::ValidationError(_)));
    }
}
