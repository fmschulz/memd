//! CLI mode for direct tool invocation
//!
//! Provides command-line interface for manual testing and debugging
//! without MCP protocol overhead.

use std::path::{Path, PathBuf};

use clap::{ArgAction, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use crate::error::Result;
use crate::store::metadata::MetadataStore;
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, ChunkType, MemoryChunk, ProjectId, Source, TenantId};

/// Export output format.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ExportFormat {
    /// Human-readable Markdown.
    Markdown,
    /// Pretty JSON array of chunks.
    Json,
    /// JSON lines (one chunk per line).
    Jsonl,
}

/// Read-scope mode for tenant memory access guardrails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantScopeMode {
    /// Only read from the current tenant.
    Local,
    /// Read from all discovered tenants in the configured data directory.
    Global,
    /// Read only from explicitly allowed tenants.
    Allowlist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TenantScopeConfig {
    primary_tenant: String,
    write_tenant: String,
    scope: TenantScopeMode,
    /// Optional explicit allowlist (used when scope=allowlist)
    #[serde(default)]
    allow_tenants: Vec<String>,
    /// Effective read tenants for retrieval
    #[serde(default)]
    read_tenants: Vec<String>,
    /// Data directory used for global tenant discovery
    #[serde(skip_serializing_if = "Option::is_none")]
    data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectScopeConfig {
    tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default)]
    read_tenants: Vec<String>,
    memd_url: String,
    project_dir: String,
}

/// CLI subcommands for memory operations
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Add a memory chunk
    Add {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Text content of the chunk
        #[arg(long)]
        text: String,

        /// Type of chunk (code, doc, trace, decision, plan, research, message, summary, other)
        #[arg(long, value_parser = parse_chunk_type)]
        chunk_type: ChunkType,

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Optional tags (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,

        /// Optional source URI
        #[arg(long)]
        source_uri: Option<String>,

        /// Optional source path
        #[arg(long)]
        source_path: Option<String>,
    },

    /// Search memory chunks
    Search {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Search query
        #[arg(long)]
        query: String,

        /// Maximum number of results
        #[arg(long, default_value = "10")]
        k: usize,
    },

    /// Get a chunk by ID
    Get {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Chunk identifier (UUID)
        #[arg(long)]
        chunk_id: String,
    },

    /// Delete a chunk (soft delete)
    Delete {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Chunk identifier (UUID)
        #[arg(long)]
        chunk_id: String,
    },

    /// Show statistics for a tenant
    Stats {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,
    },

    /// Export all tenant chunks in a human-readable or machine-readable format
    Export {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Export format
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Output file path (defaults to stdout when omitted)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Pagination size for chunk collection
        #[arg(long, default_value_t = 500)]
        page_size: usize,
    },

    /// Export tenant chunks as a tree of markdown files.
    ///
    /// Uses G1's `render_markdown_tree` (one file per `(project, chunk_type)`
    /// bucket) and writes each `(path, content)` under `<outdir>` on the
    /// user's machine. Refuses to write if the normalised `<outdir>` is a
    /// descendant of memd's data directory — the CLI runs outside the
    /// daemon trust boundary, but writing a markdown tree into `$MEMD_DATA_DIR`
    /// would silently corrupt segment / SQLite layouts.
    ExportMarkdown {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Output directory. Created if missing. Must not be inside memd's
        /// data directory.
        #[arg(long)]
        outdir: PathBuf,

        /// Optional project filter
        #[arg(long)]
        project_id: Option<String>,

        /// Include history-tier rows (default: live-only)
        #[arg(long, default_value_t = false, action = ArgAction::Set)]
        include_history: bool,

        /// Data directory used for the containment guard. Defaults to
        /// `~/.memd/data`; typically set by a wrapper when the daemon's
        /// data dir lives elsewhere.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },

    /// Export tenant memory as an OMF 1.0 JSON document.
    ///
    /// Writes to `--output` if provided, else stdout. Runs on the user's
    /// machine outside the MCP daemon trust boundary; the opened path is
    /// honoured as-is (no data-dir containment guard — CLI callers are
    /// already on the writer side).
    ExportOmf {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Optional project filter
        #[arg(long)]
        project_id: Option<String>,

        /// Output file path (defaults to stdout when omitted)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Include history-tier rows (default: live-only)
        #[arg(long, default_value_t = false, action = ArgAction::Set)]
        include_history: bool,

        /// Include rows whose status is Superseded
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        include_superseded: bool,

        /// Include rows whose status is Expired (or whose expires_at_ms has passed)
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        include_expired: bool,
    },

    /// Import an OMF 1.0 JSON document into a tenant.
    ///
    /// Reads the document from `--input` (or stdin if `-` / omitted).
    /// Use `--dry-run` for a read-only preview that reports counts
    /// without writing.
    ImportOmf {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Input file path. `-` or omitted reads from stdin.
        #[arg(long)]
        input: Option<PathBuf>,

        /// Include items whose top-level status is "archived" or "expired"
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        include_archived: bool,

        /// Optional trigram Jaccard threshold. Absent = exact-canonical only.
        #[arg(long)]
        fuzzy_threshold: Option<f32>,

        /// Preview only — compute counts without writing.
        #[arg(long, default_value_t = false, action = ArgAction::Set)]
        dry_run: bool,
    },

    /// Initialize memd guardrails and MCP config snippets for agent workflows
    Init {
        /// Tenant identifier to enforce in generated policies
        #[arg(long)]
        tenant_id: String,

        /// Tenant read scope mode
        #[arg(long, value_enum, default_value = "local")]
        scope: TenantScopeMode,

        /// Comma-separated tenant allowlist (required with --scope allowlist)
        #[arg(long, value_delimiter = ',')]
        allow_tenants: Option<Vec<String>>,

        /// Project directory where guardrail files will be written
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Explicit project identifier to pin for this repository
        #[arg(long)]
        project_id: Option<String>,

        /// Legacy stdio memd command retained for backward compatibility
        #[arg(long, default_value = "memd")]
        memd_command: String,

        /// Optional data directory used for tenant scope discovery and docs
        #[arg(long)]
        memd_data_dir: Option<PathBuf>,

        /// URL for the shared local memd HTTP daemon
        #[arg(long, default_value = "http://127.0.0.1:8787/mcp")]
        memd_url: String,

        /// Update Codex MCP config file
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        install_codex: bool,

        /// Update Claude Code MCP config file
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        install_claude: bool,

        /// Optional override path for Codex MCP config
        #[arg(long)]
        codex_config_path: Option<PathBuf>,

        /// Optional override path for Claude MCP config
        #[arg(long)]
        claude_config_path: Option<PathBuf>,

        /// Write/refresh AGENTS.md and CLAUDE.md guardrail sections
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        write_agent_files: bool,
    },
}

impl CliCommand {
    /// Whether this command needs an initialized backing store.
    pub fn requires_store(&self) -> bool {
        !matches!(self, CliCommand::Init { .. })
    }
}

/// Parse chunk type from string
fn parse_chunk_type(s: &str) -> std::result::Result<ChunkType, String> {
    match s.to_lowercase().as_str() {
        "code" => Ok(ChunkType::Code),
        "doc" => Ok(ChunkType::Doc),
        "trace" => Ok(ChunkType::Trace),
        "decision" => Ok(ChunkType::Decision),
        "plan" => Ok(ChunkType::Plan),
        "research" => Ok(ChunkType::Research),
        "message" => Ok(ChunkType::Message),
        "summary" => Ok(ChunkType::Summary),
        "other" => Ok(ChunkType::Other),
        _ => Err(format!(
            "invalid chunk type '{}', must be one of: code, doc, trace, decision, plan, research, message, summary, other",
            s
        )),
    }
}

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
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let results = store.search(&tenant, &query, k).await?;

            info!(count = results.len(), "search complete");

            let output: Vec<_> = results
                .iter()
                .map(|c| {
                    json!({
                        "chunk_id": c.chunk_id.to_string(),
                        "text": c.text,
                        "chunk_type": c.chunk_type.to_string(),
                        "timestamp_created": c.timestamp_created,
                        "tags": c.tags,
                    })
                })
                .collect();

            println!("{}", serde_json::to_string_pretty(&output)?);
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
            let effective_data_dir = resolve_data_dir(data_dir.as_deref())?;
            let outdir_abs = normalize_absolute(&outdir);
            let data_dir_abs = normalize_absolute(&effective_data_dir);
            if path_is_inside(&outdir_abs, &data_dir_abs) {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "refusing to write markdown export into memd data directory: \
                     outdir={} data_dir={}",
                    outdir_abs.display(),
                    data_dir_abs.display()
                )));
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
                    ps, &tenant, &meta.chunk_id,
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
            // stays fully read-only, matching preview_omf_import's MCP
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
            memd_command: _memd_command,
            memd_data_dir,
            install_codex,
            install_claude,
            codex_config_path,
            claude_config_path,
            write_agent_files,
            memd_url,
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
            let claude_snippet = build_claude_snippet(&memd_url);
            let codex_snippet = build_codex_snippet(&memd_url);
            let guardrail_block = render_guardrail_block(&scope_config);

            let guardrail_path = memd_dir.join("memory_guardrails.md");
            let claude_snippet_path = memd_dir.join("mcp_config_claude.json");
            let codex_snippet_path = memd_dir.join("mcp_config_codex.toml");
            let tenant_scope_path = memd_dir.join("tenant_scope.json");
            let project_scope_path = memd_dir.join("project_scope.json");
            let project_scope = ProjectScopeConfig {
                tenant_id: tenant.to_string(),
                project_id,
                read_tenants: scope_config.read_tenants.clone(),
                memd_url: memd_url.clone(),
                project_dir: project_dir.display().to_string(),
            };

            std::fs::write(&guardrail_path, &guardrail_block)?;
            std::fs::write(
                &claude_snippet_path,
                format!("{}\n", serde_json::to_string_pretty(&claude_snippet)?),
            )?;
            std::fs::write(
                &codex_snippet_path,
                format!("{}\n", toml::to_string_pretty(&codex_snippet)?),
            )?;
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

            let mut installed_codex_path = None;
            if install_codex {
                let target = if let Some(path) = codex_config_path {
                    path
                } else {
                    default_codex_config_path()?
                };
                upsert_codex_config(&target, &memd_url)?;
                installed_codex_path = Some(target);
            }

            let mut installed_claude_path = None;
            if install_claude {
                let target = if let Some(path) = claude_config_path {
                    path
                } else {
                    default_claude_config_path()?
                };
                upsert_claude_config(&target, &memd_url)?;
                installed_claude_path = Some(target);
            }

            let result = json!({
                "tenant_id": tenant.to_string(),
                "project_dir": project_dir,
                "generated": {
                    "guardrail_markdown": guardrail_path,
                        "claude_mcp_snippet": claude_snippet_path,
                        "codex_mcp_snippet": codex_snippet_path,
                        "tenant_scope": tenant_scope_path,
                        "project_scope": project_scope_path
                    },
                "scope": scope_config,
                "updated_files": updated_files,
                "installed_mcp_configs": {
                    "codex": installed_codex_path,
                    "claude": installed_claude_path
                }
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

/// Read an OMF document payload for `memd import-omf`.
///
/// `None` or a path of `-` reads from stdin; any other path reads the
/// file contents. Errors surface as `ValidationError` so the CLI's top-
/// level error reporting treats them as user-correctable input issues
/// rather than storage failures.
fn read_omf_input(path: Option<&Path>) -> Result<String> {
    let raw = match path {
        None => read_stdin_to_string()?,
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => read_stdin_to_string()?,
        Some(p) => std::fs::read_to_string(p).map_err(|e| {
            crate::error::MemdError::ValidationError(format!(
                "failed to read {}: {e}",
                p.display()
            ))
        })?,
    };
    if raw.trim().is_empty() {
        return Err(crate::error::MemdError::ValidationError(
            "OMF input is empty".to_string(),
        ));
    }
    Ok(raw)
}

fn read_stdin_to_string() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|e| {
        crate::error::MemdError::ValidationError(format!("failed to read stdin: {e}"))
    })?;
    Ok(buf)
}

/// Clean + absolutize a path without requiring it (or any parent) to
/// exist. Textually resolves `.` and `..`, and prefixes
/// `std::env::current_dir()` for relative inputs. `std::Path::canonicalize`
/// is not used because it errors on non-existent paths — we need the
/// check to run *before* `memd export-markdown <outdir>` has created
/// `<outdir>`, so that a user can't slip past the containment guard
/// by pointing at a path that doesn't yet exist.
fn normalize_absolute(p: &Path) -> PathBuf {
    use std::path::Component;
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Walk up one component, but never past the root.
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

/// Test whether `child` is the same as `parent` or a descendant of it.
///
/// Both paths must already be normalised (see `normalize_absolute`).
/// On Windows the comparison is case-insensitive to match the
/// filesystem's own semantics — `C:\Users\me\.memd` and
/// `c:\USERS\me\.MEMD` refer to the same directory, so the lexical
/// guard must refuse both (Codex G3 review MEDIUM). On Unix the
/// comparison stays case-sensitive.
fn path_is_inside(child: &Path, parent: &Path) -> bool {
    #[cfg(windows)]
    {
        let c = child.to_string_lossy().to_lowercase();
        let p = parent.to_string_lossy().to_lowercase();
        Path::new(&c).starts_with(Path::new(&p))
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

fn absolutize_project_dir(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    Ok(home.join(".memd").join("data"))
}

fn discover_tenants(data_dir: &Path) -> Result<Vec<String>> {
    let tenants_dir = data_dir.join("tenants");
    if !tenants_dir.exists() {
        return Ok(Vec::new());
    }

    let mut tenants = Vec::new();
    for entry in std::fs::read_dir(&tenants_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if TenantId::new(&name).is_ok() {
            tenants.push(name);
        }
    }
    tenants.sort();
    tenants.dedup();
    Ok(tenants)
}

fn normalize_allow_tenants(raw: &[String]) -> Result<Vec<String>> {
    let mut tenants = Vec::new();
    for tenant in raw {
        let trimmed = tenant.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = TenantId::new(trimmed)?.to_string();
        tenants.push(normalized);
    }
    tenants.sort();
    tenants.dedup();
    Ok(tenants)
}

fn build_tenant_scope_config(
    primary_tenant: &str,
    scope: TenantScopeMode,
    allow_tenants: Option<&[String]>,
    data_dir: &Path,
) -> Result<TenantScopeConfig> {
    let mut config = TenantScopeConfig {
        primary_tenant: primary_tenant.to_string(),
        write_tenant: primary_tenant.to_string(),
        scope,
        allow_tenants: Vec::new(),
        read_tenants: vec![primary_tenant.to_string()],
        data_dir: None,
    };

    match scope {
        TenantScopeMode::Local => {
            if allow_tenants.is_some() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants is only valid with --scope allowlist".to_string(),
                ));
            }
        }
        TenantScopeMode::Allowlist => {
            let Some(raw) = allow_tenants else {
                return Err(crate::error::MemdError::ValidationError(
                    "--scope allowlist requires --allow-tenants".to_string(),
                ));
            };
            let normalized = normalize_allow_tenants(raw)?;
            if normalized.is_empty() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants must include at least one valid tenant".to_string(),
                ));
            }

            let mut read_tenants = vec![primary_tenant.to_string()];
            for tenant in &normalized {
                if tenant != primary_tenant {
                    read_tenants.push(tenant.clone());
                }
            }

            config.allow_tenants = normalized;
            config.read_tenants = read_tenants;
        }
        TenantScopeMode::Global => {
            if allow_tenants.is_some() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants is not supported with --scope global".to_string(),
                ));
            }

            let mut discovered = discover_tenants(data_dir)?;
            if !discovered.iter().any(|t| t == primary_tenant) {
                discovered.push(primary_tenant.to_string());
            }
            discovered.sort();
            discovered.dedup();

            config.read_tenants = discovered;
            config.data_dir = Some(data_dir.display().to_string());
        }
    }

    Ok(config)
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
        offset = offset.saturating_add(page.len());
        chunks.extend(page);
    }

    Ok(chunks)
}

fn export_format_name(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Markdown => "markdown",
        ExportFormat::Json => "json",
        ExportFormat::Jsonl => "jsonl",
    }
}

fn render_export(
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

fn build_claude_snippet(memd_url: &str) -> Value {
    json!({
        "mcpServers": {
            "memd": {
                "type": "http",
                "url": memd_url
            }
        }
    })
}

fn build_codex_snippet(memd_url: &str) -> toml::Value {
    let mut memd_table = toml::map::Map::new();
    memd_table.insert("url".to_string(), toml::Value::String(memd_url.to_string()));

    let mut mcp_servers = toml::map::Map::new();
    mcp_servers.insert("memd".to_string(), toml::Value::Table(memd_table));

    let mut root = toml::map::Map::new();
    root.insert("mcp_servers".to_string(), toml::Value::Table(mcp_servers));
    toml::Value::Table(root)
}

fn render_guardrail_block(scope_config: &TenantScopeConfig) -> String {
    let mut out = String::new();
    out.push_str("<!-- memd-guardrails:start -->\n");
    out.push_str("## memd Memory Guardrails\n\n");
    out.push_str("Use memd for persistent memory in this repository.\n\n");
    out.push_str(&format!(
        "- Required write `tenant_id`: `{}`\n",
        scope_config.write_tenant
    ));
    out.push_str(&format!(
        "- Read scope mode: `{}`\n",
        match scope_config.scope {
            TenantScopeMode::Local => "local",
            TenantScopeMode::Global => "global",
            TenantScopeMode::Allowlist => "allowlist",
        }
    ));
    out.push_str(&format!(
        "- Effective read tenants: `{}`\n",
        scope_config.read_tenants.join(", ")
    ));
    out.push_str(
        "- Preferred model: for one trusted machine or trust domain, use one stable shared write tenant and narrow retrieval with `project_id`, `thread_id`, and `task_id`.\n",
    );
    out.push_str(
        "- If `.memd/project_scope.json` exists, use its pinned `tenant_id` and `project_id` instead of inferring from the directory name.\n",
    );
    out.push_str(
        "- Hard rule: do not send a final answer without memory retrieval + memory write.\n\n",
    );
    out.push_str("### Mandatory Per-Task Protocol\n\n");
    out.push_str("1. Retrieve first:\n");
    out.push_str("   - For each tenant in the effective read set, call `context.find_relevant_context` or `memory.search`.\n");
    out.push_str("   - Merge and deduplicate context before implementation.\n");
    if scope_config.scope == TenantScopeMode::Global {
        out.push_str("   - In global mode, the tenant list is a snapshot from init-time data directory discovery. Re-run `memd init` to refresh.\n");
    }
    out.push_str("2. Implement using retrieved context.\n");
    out.push_str("3. Persist before final response:\n");
    out.push_str(
        "   - Write only to the required write tenant using `memory.add` or `memory.add_batch`.\n",
    );
    out.push_str("4. If memd is unavailable:\n");
    out.push_str(
        "   - Explicitly report memory persistence failure and stop before final answer.\n\n",
    );
    out.push_str("### Suggested Memory Write Template\n\n");
    out.push_str("Use `chunk_type: \"summary\"` and tags such as:\n");
    out.push_str("- `ctx:doc`\n");
    out.push_str("- `ctx:subsystem:<name>`\n");
    out.push_str("- `ctx:file:<path>`\n");
    out.push_str("- `session:<id>`\n");
    out.push_str("<!-- memd-guardrails:end -->\n");
    out
}

fn upsert_guardrail_file(path: &Path, guardrail_block: &str) -> Result<()> {
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

fn read_or_init_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(path)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(trimmed)?)
}

fn backup_existing(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string());
    let backup = path.with_file_name(format!("{name}.bak"));
    std::fs::copy(path, &backup)?;
    Ok(Some(backup))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = backup_existing(path)?;
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn read_or_init_toml(path: &Path) -> Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let raw = std::fs::read_to_string(path)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    Ok(toml::from_str(trimmed)?)
}

fn write_toml(path: &Path, value: &toml::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = backup_existing(path)?;
    std::fs::write(path, format!("{}\n", toml::to_string_pretty(value)?))?;
    Ok(())
}

fn default_codex_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    Ok(home.join(".codex").join("config.toml"))
}

fn default_claude_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    Ok(home.join(".claude.json"))
}

fn upsert_claude_config(path: &Path, memd_url: &str) -> Result<()> {
    let mut root = read_or_init_json(path)?;
    if !root.is_object() {
        root = json!({});
    }
    let root_obj = root.as_object_mut().expect("root object");
    let mcp_servers = root_obj
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    if !mcp_servers.is_object() {
        *mcp_servers = json!({});
    }
    let servers_obj = mcp_servers.as_object_mut().expect("mcpServers object");
    servers_obj.insert(
        "memd".to_string(),
        json!({
            "type": "http",
            "url": memd_url
        }),
    );
    write_json(path, &root)
}

fn upsert_codex_config(path: &Path, memd_url: &str) -> Result<()> {
    let mut root = read_or_init_toml(path)?;
    if !root.is_table() {
        root = toml::Value::Table(toml::map::Map::new());
    }

    let root_table = root.as_table_mut().expect("root table");
    let mcp_servers = root_table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !mcp_servers.is_table() {
        *mcp_servers = toml::Value::Table(toml::map::Map::new());
    }

    let servers_table = mcp_servers.as_table_mut().expect("mcp_servers table");
    let mut memd_table = toml::map::Map::new();
    memd_table.insert("url".to_string(), toml::Value::String(memd_url.to_string()));
    servers_table.insert("memd".to_string(), toml::Value::Table(memd_table));

    write_toml(path, &root)
}

fn render_markdown_export(chunks: &[MemoryChunk], tenant: &TenantId) -> String {
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
    async fn init_writes_guardrails_and_mcp_configs() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let codex_path = dir.path().join("codex.json");
        let claude_path = dir.path().join("claude.json");

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
                memd_url: "http://127.0.0.1:8787/mcp".to_string(),
                install_codex: true,
                install_claude: true,
                codex_config_path: Some(codex_path.clone()),
                claude_config_path: Some(claude_path.clone()),
                write_agent_files: true,
            },
        )
        .await
        .unwrap();

        let guardrails =
            std::fs::read_to_string(project_dir.join(".memd/memory_guardrails.md")).unwrap();
        assert!(guardrails.contains("demo_tenant"));
        assert!(guardrails.contains("context.find_relevant_context"));
        assert!(guardrails.contains("memory.add"));
        assert!(guardrails.contains("Read scope mode: `local`"));
        assert!(guardrails.contains(".memd/project_scope.json"));

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

        let agents = std::fs::read_to_string(project_dir.join("AGENTS.md")).unwrap();
        assert!(agents.contains("memd-guardrails:start"));

        let claude_cfg: Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_path).unwrap()).unwrap();
        assert_eq!(
            claude_cfg["mcpServers"]["memd"]["type"].as_str(),
            Some("http")
        );
        assert_eq!(
            claude_cfg["mcpServers"]["memd"]["url"].as_str(),
            Some("http://127.0.0.1:8787/mcp")
        );

        let codex_cfg: toml::Value =
            toml::from_str(&std::fs::read_to_string(&codex_path).unwrap()).unwrap();
        assert_eq!(
            codex_cfg["mcp_servers"]["memd"]["url"].as_str(),
            Some("http://127.0.0.1:8787/mcp")
        );
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
                    memd_url: "http://127.0.0.1:8787/mcp".to_string(),
                    install_codex: false,
                    install_claude: false,
                    codex_config_path: None,
                    claude_config_path: None,
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
                memd_url: "http://127.0.0.1:8787/mcp".to_string(),
                install_codex: false,
                install_claude: false,
                codex_config_path: None,
                claude_config_path: None,
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
                memd_url: "http://127.0.0.1:8787/mcp".to_string(),
                install_codex: false,
                install_claude: false,
                codex_config_path: None,
                claude_config_path: None,
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
}
