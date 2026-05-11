//! CLI mode for direct operation invocation
//!
//! Provides command-line interface for manual testing and debugging
//! through the local executable.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Subcommand};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::error::{MemdError, Result};
use crate::maintenance::DreamParams;
use crate::mcp::McpError;
use crate::mcp::handlers::{
    AddBatchParams, AddParams, ArtifactCreateParams, ArtifactGetParams, ArtifactLibraryParams,
    ArtifactListThreadParams, ArtifactVerifyParams, CompactParams, ConsolidateEpisodeParams,
    ContextFindRelevantContextParams, ContextGetFilesForSubsystemParams,
    ContextGetHotContextParams, ContextListSubsystemsParams, ContextSearchDocumentsParams,
    ContextSuggestAgentParams, DeleteParams, ExportMarkdownParams, ExportOmfParams,
    FeedbackParams, FindCallersParams, FindDefinitionParams, FindErrorsParams, FindImportsParams,
    FindNearDuplicatesParams, FindReferencesParams, FindToolCallsParams, GetParams, HealthParams,
    ImportOmfParams, MetricsParams, PreviewOmfImportParams, ProjectBriefParams, QueryMode,
    SearchParams, SetExpiryParams, StatsParams, SupersedeParams, TaskAddEvidenceParams,
    TaskFinishParams, TaskGetParams, TaskProgressParams, TaskResumeParams, TaskRunFinishParams,
    TaskRunStartParams, TaskSearchParams, TaskStartParams, handle_artifact_create,
    handle_artifact_find_decisions, handle_artifact_find_evidence,
    handle_artifact_find_failures, handle_artifact_find_highlights, handle_artifact_get,
    handle_artifact_list_thread, handle_artifact_search, handle_artifact_verify,
    handle_context_brief_project, handle_context_find_relevant_context,
    handle_context_get_files_for_subsystem, handle_context_get_hot_context,
    handle_context_list_subsystems, handle_context_search_documents, handle_context_suggest_agent,
    handle_find_callers, handle_find_definition, handle_find_errors, handle_find_imports,
    handle_find_references, handle_find_tool_calls, handle_memory_add, handle_memory_add_batch,
    handle_memory_compact, handle_memory_consolidate_episode, handle_memory_delete,
    handle_memory_dream, handle_memory_export_markdown, handle_memory_export_omf,
    handle_memory_feedback, handle_memory_find_near_duplicates, handle_memory_get,
    handle_memory_health, handle_memory_import_omf, handle_memory_metrics,
    handle_memory_preview_omf_import, handle_memory_search, handle_memory_set_expiry,
    handle_memory_stats, handle_memory_supersede, handle_task_add_evidence, handle_task_finish,
    handle_task_get, handle_task_progress, handle_task_resume, handle_task_run_finish,
    handle_task_run_start, handle_task_search, handle_task_start,
};
use crate::metrics::MetricsCollector;
use crate::structural::{
    CallGraphIndexer, CallGraphSymbolRecord, StructuralStore, SymbolIndexer, SymbolQueryService,
    TraceQueryService,
};
use crate::store::metadata::MetadataStore;
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, ChunkType, MemoryChunk, ProjectId, Source, TenantId};

/// Export output format.
#[derive(Debug, Clone, Copy, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Retrieval intent for CLI search/orchestration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum CliQueryMode {
    Generic,
    BriefProject,
    ResumeTask,
    FindFailures,
    FindDecisions,
    FindEvidence,
    FindHighlights,
}

/// Warm-worker behavior for agent-facing CLI retrieval and local tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmMode {
    /// Use the local warm worker, starting it if needed; fall back to cold CLI if startup fails.
    Auto,
    /// Always run in the current CLI process.
    Off,
    /// Require a local warm worker; fail if it cannot be started or reached.
    Required,
}

/// Optional post-retrieval reranker for CLI search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchReranker {
    /// Keep the built-in retrieval order.
    None,
    /// Use MemReranker-4B only when the optional runtime is available.
    Auto,
    /// Require MemReranker-4B; fail instead of falling back.
    #[value(name = "memreranker-4b")]
    #[serde(rename = "memreranker-4b")]
    MemReranker4B,
}

/// Administrative warm-worker commands.
#[derive(Debug, Clone, Subcommand)]
pub enum WarmCommand {
    /// Start the local warm worker if it is not already running.
    Start,
    /// Report whether the local warm worker is reachable.
    Status,
    /// Ask the local warm worker to stop.
    Stop,
}

/// Process identity for a local warm worker.
#[derive(Debug, Clone)]
pub struct WarmProcessConfig {
    pub data_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub embedding_model: String,
    pub search_variant: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchRerankerOptions {
    reranker: SearchReranker,
    model: String,
    device: String,
    batch_size: usize,
    timeout_seconds: u64,
    python: String,
}

impl From<CliQueryMode> for QueryMode {
    fn from(value: CliQueryMode) -> Self {
        match value {
            CliQueryMode::Generic => QueryMode::Generic,
            CliQueryMode::BriefProject => QueryMode::BriefProject,
            CliQueryMode::ResumeTask => QueryMode::ResumeTask,
            CliQueryMode::FindFailures => QueryMode::FindFailures,
            CliQueryMode::FindDecisions => QueryMode::FindDecisions,
            CliQueryMode::FindEvidence => QueryMode::FindEvidence,
            CliQueryMode::FindHighlights => QueryMode::FindHighlights,
        }
    }
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
    interface: String,
    cli_command: String,
    agent_context_output: String,
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

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Use memory.search compact shaping instead of the legacy raw chunk array
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        compact: bool,

        /// Approximate result token budget; also enables compact shaping
        #[arg(long)]
        token_budget: Option<usize>,

        /// Retrieval intent for digest-biased searches
        #[arg(long, value_enum, default_value = "generic")]
        mode: CliQueryMode,

        /// Omit chunk text from compact output
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        no_text: bool,

        /// Include linked canonical artifacts in compact output
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        include_artifact: bool,

        /// Output format
        #[arg(long, value_enum, default_value = "json")]
        format: ExportFormat,

        /// Output file path (defaults to stdout)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Optional high-quality post-retrieval reranker
        #[arg(long, value_enum, default_value = "none")]
        reranker: SearchReranker,

        /// Hugging Face model id for the optional MemReranker path
        #[arg(long, default_value = "IAAR-Shanghai/MemReranker-4B")]
        reranker_model: String,

        /// Device for the optional MemReranker path: auto, cuda, cuda:0, or cpu
        #[arg(long, default_value = "auto")]
        reranker_device: String,

        /// Batch size for optional MemReranker inference
        #[arg(long, default_value = "1")]
        reranker_batch_size: usize,

        /// Timeout in seconds for optional MemReranker model load and inference
        #[arg(long, default_value = "120")]
        reranker_timeout_seconds: u64,

        /// Python executable used by the optional MemReranker path
        #[arg(long, default_value = "python3")]
        reranker_python: String,

        /// Use the local warm worker for this operation
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Build a bounded local context file for agents using CLI-only retrieval.
    ///
    /// This is the preferred CLI-only orchestration path: a controller runs
    /// retrieval before launching the agent, writes a compact context file
    /// into the workspace, and the agent reads that file during the solve.
    AgentContext {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Search query. May be repeated; results are merged and deduplicated.
        #[arg(long, required = true, action = ArgAction::Append)]
        query: Vec<String>,

        /// Maximum results per query before deduplication
        #[arg(long, default_value = "2")]
        k: usize,

        /// Approximate token budget per query
        #[arg(long, default_value = "700")]
        token_budget: usize,

        /// Retrieval intent for digest-biased searches
        #[arg(long, value_enum, default_value = "generic")]
        mode: CliQueryMode,

        /// Omit chunk text from compact output
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        no_text: bool,

        /// Include linked canonical artifacts in compact output
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        include_artifact: bool,

        /// Output format
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Output file path (defaults to stdout)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Optional directory for benchmark/audit JSON logs
        #[arg(long)]
        log_dir: Option<PathBuf>,

        /// Use the local warm worker for this operation
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Invoke a local memd operation by its historical tool name.
    ///
    /// This preserves the former structured operation surface without starting
    /// an external service. Pass a JSON object with `--json` or `--input`; omit both for
    /// `{}`. The result is unwrapped to the operation payload when possible.
    Call {
        /// Operation name, for example `memory.search` or `task.start`
        tool: String,

        /// JSON object containing operation arguments
        #[arg(long, conflicts_with = "input")]
        json: Option<String>,

        /// File containing a JSON object with operation arguments
        #[arg(long, conflicts_with = "json")]
        input: Option<PathBuf>,

        /// Output file path (defaults to stdout)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Use the local warm worker for this operation
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Run local operation calls from JSONL in one process.
    ///
    /// Each non-empty input line must be a JSON object with `tool` and
    /// optional `arguments`. Results are emitted as JSONL with one row
    /// per input line.
    Batch {
        /// JSONL input file. Omit or pass `-` to read stdin.
        #[arg(long)]
        jsonl: Option<PathBuf>,

        /// Keep stdin/stdout open and process one JSONL request per line.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        stream: bool,

        /// Keep processing after a failed input line or operation.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        continue_on_error: bool,

        /// Output file path (defaults to stdout)
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Manage the local warm worker used by `--warm auto|required`.
    Warm {
        #[command(subcommand)]
        command: WarmCommand,
    },

    /// Internal warm worker entrypoint. Not an agent-facing interface.
    #[command(hide = true)]
    WarmWorker {
        /// Unix socket path to listen on.
        #[arg(long)]
        socket: PathBuf,
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
    /// descendant of memd's data directory, because writing a markdown tree
    /// into `$MEMD_DATA_DIR` would silently corrupt segment / SQLite layouts.
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
    /// Writes to `--output` if provided, else stdout. The opened path is
    /// honoured as-is; CLI callers are already on the writer side, so there
    /// is no data-dir containment guard.
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

    /// Initialize memd CLI guardrails for agent workflows
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

        /// memd CLI command used in generated guardrails
        #[arg(long, default_value = "memd")]
        memd_command: String,

        /// Optional data directory used for tenant scope discovery and docs
        #[arg(long)]
        memd_data_dir: Option<PathBuf>,

        /// Write/refresh AGENTS.md and CLAUDE.md guardrail sections
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        write_agent_files: bool,
    },
}

impl CliCommand {
    /// Whether this command needs an initialized backing store.
    pub fn requires_store(&self) -> bool {
        !matches!(self, CliCommand::Init { .. } | CliCommand::Warm { .. })
    }

    /// Warm mode for commands that can be served by the local warm worker.
    pub fn warm_mode(&self) -> Option<WarmMode> {
        match self {
            CliCommand::Search { warm, .. }
            | CliCommand::AgentContext { warm, .. }
            | CliCommand::Call { warm, .. } => Some(*warm),
            _ => None,
        }
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
            let effective_data_dirs =
                resolve_export_markdown_data_dirs(data_dir.as_deref())?;
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

#[derive(Debug, Clone, Deserialize)]
struct BatchCallInput {
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
}

fn read_batch_input(path: Option<&Path>) -> Result<String> {
    match path {
        None => read_stdin_to_string(),
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => read_stdin_to_string(),
        Some(p) => Ok(std::fs::read_to_string(p)?),
    }
}

async fn run_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input: &str,
    continue_on_error: bool,
) -> Result<String> {
    let mut out = String::new();
    let mut processed = 0usize;

    for (line_number, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let index = processed;
        processed += 1;

        let request = match serde_json::from_str::<BatchCallInput>(line) {
            Ok(request) => request,
            Err(error) => {
                if !continue_on_error {
                    return Err(MemdError::ValidationError(format!(
                        "invalid JSONL request on line {}: {error}",
                        line_number + 1
                    )));
                }
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "error": format!("invalid JSONL request: {error}"),
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
                continue;
            }
        };

        let arguments = request.arguments.unwrap_or_else(|| json!({}));
        if !(arguments.is_object() || arguments.is_null()) {
            let message = "batch arguments must be a JSON object".to_string();
            if !continue_on_error {
                return Err(MemdError::ValidationError(message));
            }
            let row = json!({
                "ok": false,
                "index": index,
                "line": line_number + 1,
                "tool": request.tool,
                "error": message,
            });
            out.push_str(&serde_json::to_string(&row)?);
            out.push('\n');
            continue;
        }

        match cli_call_tool(store, tenant_manager, &request.tool, arguments).await {
            Ok(value) => {
                let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
                let row = json!({
                    "ok": true,
                    "index": index,
                    "line": line_number + 1,
                    "tool": request.tool,
                    "result": payload,
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
            }
            Err(error) => {
                if !continue_on_error {
                    return Err(MemdError::ProtocolError(error.to_string()));
                }
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "tool": request.tool,
                    "error": error.to_string(),
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
            }
        }
    }

    Ok(out)
}

async fn stream_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input_path: Option<&Path>,
    output_path: Option<&Path>,
    continue_on_error: bool,
) -> Result<()> {
    use std::io::{BufRead, BufReader, BufWriter, Write};

    let input: Box<dyn BufRead> = match input_path {
        None => Box::new(BufReader::new(std::io::stdin())),
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => {
            Box::new(BufReader::new(std::io::stdin()))
        }
        Some(p) => Box::new(BufReader::new(std::fs::File::open(p)?)),
    };
    let mut output: Box<dyn Write> = match output_path {
        None => Box::new(BufWriter::new(std::io::stdout())),
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Box::new(BufWriter::new(std::fs::File::create(path)?))
        }
    };

    let mut processed = 0usize;
    for (line_number, raw_line) in input.lines().enumerate() {
        let raw_line = raw_line?;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let index = processed;
        processed += 1;

        let rendered = match run_batch_jsonl(store, tenant_manager, line, continue_on_error).await {
            Ok(rendered) => {
                let mut row: Value = serde_json::from_str(rendered.trim())?;
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("index".to_string(), json!(index));
                    obj.insert("line".to_string(), json!(line_number + 1));
                }
                serde_json::to_string(&row)? + "\n"
            }
            Err(error) if continue_on_error => {
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "error": error.to_string(),
                });
                serde_json::to_string(&row)? + "\n"
            }
            Err(error) => return Err(error),
        };

        output.write_all(rendered.as_bytes())?;
        output.flush()?;
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WarmWireCommand {
    Search {
        tenant_id: String,
        query: String,
        k: usize,
        project_id: Option<String>,
        compact: bool,
        token_budget: Option<usize>,
        mode: CliQueryMode,
        no_text: bool,
        include_artifact: bool,
        format: ExportFormat,
        reranker: SearchRerankerOptions,
    },
    AgentContext {
        tenant_id: String,
        project_id: Option<String>,
        query: Vec<String>,
        k: usize,
        token_budget: usize,
        mode: CliQueryMode,
        no_text: bool,
        include_artifact: bool,
        format: ExportFormat,
    },
    Call {
        tool: String,
        arguments: Value,
    },
}

#[derive(Debug, Clone)]
struct WarmLocalOutputs {
    output: Option<PathBuf>,
    log_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WarmWireRequest {
    Ping,
    Shutdown,
    Command { command: WarmWireCommand },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WarmWireResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WarmWireResponse {
    fn ok_result(result: Value) -> Self {
        Self {
            ok: true,
            output: None,
            log_payload: None,
            result: Some(result),
            error: None,
        }
    }

    fn ok_output(output: String, log_payload: Option<Value>) -> Self {
        Self {
            ok: true,
            output: Some(output),
            log_payload,
            result: None,
            error: None,
        }
    }

    fn error(error: impl ToString) -> Self {
        Self {
            ok: false,
            output: None,
            log_payload: None,
            result: None,
            error: Some(error.to_string()),
        }
    }
}

pub fn warm_socket_path(config: &WarmProcessConfig) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(config.data_dir.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(config.embedding_model.as_bytes());
    hasher.update(b"\0");
    hasher.update(config.search_variant.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    config
        .data_dir
        .join("warm")
        .join(&hex[..16])
        .join("memd.sock")
}

fn warm_pid_path(config: &WarmProcessConfig) -> PathBuf {
    warm_socket_path(config).with_file_name("memd.pid")
}

fn warm_log_path(config: &WarmProcessConfig) -> PathBuf {
    warm_socket_path(config).with_file_name("worker.log")
}

fn warm_wire_command_from_cli(
    cmd: &CliCommand,
) -> Result<Option<(WarmWireCommand, WarmLocalOutputs)>> {
    let mapped = match cmd {
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
        } => (
            WarmWireCommand::Search {
                tenant_id: tenant_id.clone(),
                query: query.clone(),
                k: *k,
                project_id: project_id.clone(),
                compact: *compact,
                token_budget: *token_budget,
                mode: *mode,
                no_text: *no_text,
                include_artifact: *include_artifact,
                format: *format,
                reranker: SearchRerankerOptions {
                    reranker: *reranker,
                    model: reranker_model.clone(),
                    device: reranker_device.clone(),
                    batch_size: *reranker_batch_size,
                    timeout_seconds: *reranker_timeout_seconds,
                    python: reranker_python.clone(),
                },
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: None,
            },
        ),
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
        } => (
            WarmWireCommand::AgentContext {
                tenant_id: tenant_id.clone(),
                project_id: project_id.clone(),
                query: query.clone(),
                k: *k,
                token_budget: *token_budget,
                mode: *mode,
                no_text: *no_text,
                include_artifact: *include_artifact,
                format: *format,
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: log_dir.clone(),
            },
        ),
        CliCommand::Call {
            tool,
            json,
            input,
            output,
            warm: _,
        } => (
            WarmWireCommand::Call {
                tool: tool.clone(),
                arguments: parse_call_arguments(json.as_deref(), input.as_deref())?,
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: None,
            },
        ),
        _ => return Ok(None),
    };
    Ok(Some(mapped))
}

async fn execute_warm_wire_command<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    command: WarmWireCommand,
) -> Result<(String, Option<Value>)> {
    match command {
        WarmWireCommand::Search {
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
            reranker,
        } => {
            let mut payload = cli_search_payload(
                store,
                tenant_id.clone(),
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
            payload = apply_search_reranker(payload, &query, &reranker)?;
            Ok((render_search_payload(&payload, format)?, None))
        }
        WarmWireCommand::AgentContext {
            tenant_id,
            project_id,
            query,
            k,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
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
            let rendered = render_agent_context(&payload, format)?;
            Ok((rendered, Some(payload)))
        }
        WarmWireCommand::Call { tool, arguments } => {
            let value = cli_call_tool(store, tenant_manager, &tool, arguments)
                .await
                .map_err(|e| MemdError::ProtocolError(e.to_string()))?;
            let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
            Ok((serde_json::to_string_pretty(&payload)? + "\n", None))
        }
    }
}

pub async fn try_run_warm_client(config: &WarmProcessConfig, cmd: &CliCommand) -> Result<bool> {
    let Some(mode) = cmd.warm_mode() else {
        return Ok(false);
    };
    if mode == WarmMode::Off {
        return Ok(false);
    }
    let Some((wire_command, local_outputs)) = warm_wire_command_from_cli(cmd)? else {
        return Ok(false);
    };

    match warm_ping(config).await {
        Ok(_) => {}
        Err(error) => match warm_start(config).await {
            Ok(_) => {}
            Err(start_error) if mode == WarmMode::Auto => {
                warn!(
                    error = %error,
                    start_error = %start_error,
                    "warm worker unavailable; falling back to cold CLI"
                );
                return Ok(false);
            }
            Err(start_error) => {
                return Err(MemdError::ProtocolError(format!(
                    "warm worker required but unavailable: {error}; start failed: {start_error}"
                )));
            }
        },
    }

    let response = warm_request(
        &warm_socket_path(config),
        &WarmWireRequest::Command {
            command: wire_command,
        },
    )
    .await?;
    if !response.ok {
        return Err(MemdError::ProtocolError(
            response
                .error
                .unwrap_or_else(|| "warm worker command failed".to_string()),
        ));
    }
    if let Some(payload) = response.log_payload.as_ref() {
        write_cli_log(local_outputs.log_dir.as_deref(), "memd_search", payload)?;
    }
    let output = response.output.unwrap_or_default();
    write_rendered(local_outputs.output.as_deref(), &output)?;
    Ok(true)
}

pub async fn run_warm_admin(config: &WarmProcessConfig, command: WarmCommand) -> Result<()> {
    let payload = match command {
        WarmCommand::Start => warm_start(config).await?,
        WarmCommand::Status => match warm_ping(config).await {
            Ok(result) => json!({
                "status": "running",
                "socket": warm_socket_path(config),
                "result": result,
            }),
            Err(error) => json!({
                "status": "stopped",
                "socket": warm_socket_path(config),
                "error": error.to_string(),
            }),
        },
        WarmCommand::Stop => {
            match warm_request(&warm_socket_path(config), &WarmWireRequest::Shutdown).await {
                Ok(response) if response.ok => {
                    let result = response.result.unwrap_or(Value::Null);
                    let pid = result
                        .get("pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok());
                    let stopped = wait_for_warm_pid_exit(pid, Duration::from_secs(10)).await;
                    if stopped {
                        let _ = std::fs::remove_file(warm_pid_path(config));
                    }
                    json!({
                        "status": if stopped { "stopped" } else { "stopping" },
                        "socket": warm_socket_path(config),
                        "result": result,
                    })
                }
                Ok(response) => {
                    return Err(MemdError::ProtocolError(
                        response
                            .error
                            .unwrap_or_else(|| "warm worker stop failed".to_string()),
                    ));
                }
                Err(error) => json!({
                    "status": "not_running",
                    "socket": warm_socket_path(config),
                    "error": error.to_string(),
                }),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

async fn wait_for_warm_pid_exit(pid: Option<u32>, timeout: Duration) -> bool {
    let Some(pid) = pid else {
        return true;
    };
    let deadline = Instant::now() + timeout;
    loop {
        if !warm_pid_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn warm_pid_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

async fn warm_start(config: &WarmProcessConfig) -> Result<Value> {
    if let Ok(result) = warm_ping(config).await {
        return Ok(json!({
            "status": "already_running",
            "socket": warm_socket_path(config),
            "result": result,
        }));
    }

    let socket = warm_socket_path(config);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }

    let log_path = warm_log_path(config);
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(config_path) = &config.config_path {
        command.arg("--config").arg(config_path);
    }
    command
        .arg("--data-dir")
        .arg(&config.data_dir)
        .arg("--embedding-model")
        .arg(&config.embedding_model)
        .arg("--search-variant")
        .arg(&config.search_variant)
        .arg("warm-worker")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn()?;
    let pid = child.id();
    std::fs::write(warm_pid_path(config), format!("{pid}\n"))?;

    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
        .as_millis();
    for _ in 0..300 {
        match warm_ping(config).await {
            Ok(result) => {
                return Ok(json!({
                    "status": "started",
                    "pid": pid,
                    "socket": socket,
                    "log": log_path,
                    "startup_ms": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
                        .as_millis()
                        .saturating_sub(start),
                    "result": result,
                }));
            }
            Err(_) => {
                if let Some(status) = child.try_wait()? {
                    return Err(MemdError::ProtocolError(format!(
                        "warm worker exited before becoming ready: {status}; see {}",
                        log_path.display()
                    )));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    Err(MemdError::ProtocolError(format!(
        "warm worker did not become ready within 30s; see {}",
        log_path.display()
    )))
}

async fn warm_ping(config: &WarmProcessConfig) -> Result<Value> {
    let response = warm_request(&warm_socket_path(config), &WarmWireRequest::Ping).await?;
    if !response.ok {
        return Err(MemdError::ProtocolError(
            response
                .error
                .unwrap_or_else(|| "warm worker ping failed".to_string()),
        ));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

#[cfg(unix)]
async fn warm_request(socket: &Path, request: &WarmWireRequest) -> Result<WarmWireResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket).await?;
    let body = serde_json::to_vec(request)?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(not(unix))]
async fn warm_request(_socket: &Path, _request: &WarmWireRequest) -> Result<WarmWireResponse> {
    Err(MemdError::ProtocolError(
        "warm worker requires Unix domain sockets".to_string(),
    ))
}

#[cfg(unix)]
async fn run_warm_worker<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    socket: &Path,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    info!(socket = %socket.display(), "memd warm worker listening");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        let mut shutdown = false;
        let response = match serde_json::from_slice::<WarmWireRequest>(&bytes) {
            Ok(WarmWireRequest::Ping) => WarmWireResponse::ok_result(json!({
                "pid": std::process::id(),
                "socket": socket,
            })),
            Ok(WarmWireRequest::Shutdown) => {
                shutdown = true;
                WarmWireResponse::ok_result(json!({
                    "pid": std::process::id(),
                    "socket": socket,
                }))
            }
            Ok(WarmWireRequest::Command { command }) => {
                match execute_warm_wire_command(store, tenant_manager, command).await {
                    Ok((output, log_payload)) => WarmWireResponse::ok_output(output, log_payload),
                    Err(error) => WarmWireResponse::error(error),
                }
            }
            Err(error) => WarmWireResponse::error(format!("invalid warm request: {error}")),
        };
        let body = serde_json::to_vec(&response)?;
        stream.write_all(&body).await?;
        stream.shutdown().await?;
        if shutdown {
            break;
        }
    }

    let _ = std::fs::remove_file(socket);
    Ok(())
}

#[cfg(not(unix))]
async fn run_warm_worker<S: Store>(
    _store: &S,
    _tenant_manager: Option<&TenantManager>,
    _socket: &Path,
) -> Result<()> {
    Err(MemdError::ProtocolError(
        "warm worker requires Unix domain sockets".to_string(),
    ))
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

/// Refuse to follow any symlink planted inside `outdir_abs` along the
/// path to `full_target`. Closes the pre-existing-symlink escape where
/// an attacker creates `<outdir>/sub` → `/etc` before
/// `memd export-markdown` runs, so the subsequent
/// `std::fs::write(<outdir>/sub/<file>)` overwrites the attacker's
/// backing file instead of a fresh file under outdir (Item 3 from the
/// nanomem-features handoff).
///
/// Walks each already-existing component under `outdir_abs` and refuses
/// if any is a symlink. The outdir itself is NOT checked — a user may
/// legitimately point `--outdir` at a symlinked directory they own —
/// but anything *inside* outdir that predates the export must be a
/// regular file or directory, never a symlink. Non-existing segments
/// are fine; they'll be created by `create_dir_all`.
///
/// A small TOCTOU window remains between this check and the write.
/// Closing it fully on every platform would require `O_NOFOLLOW`,
/// which is Unix-only; memd's CLI is already a user-trusted surface
/// (the caller picks outdir), so narrowing the pre-planted-symlink
/// window is the practical fix.
fn reject_if_any_symlink_inside_outdir(
    full_target: &Path,
    outdir_abs: &Path,
) -> Result<()> {
    let rel = full_target.strip_prefix(outdir_abs).map_err(|_| {
        crate::error::MemdError::ValidationError(format!(
            "internal: target {} not inside outdir {}",
            full_target.display(),
            outdir_abs.display()
        ))
    })?;
    let mut current = outdir_abs.to_path_buf();
    for segment in rel.components() {
        current.push(segment.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "refusing to follow symlink inside outdir: {}",
                    current.display()
                )));
            }
            Ok(_) => continue,
            // NotFound is the expected "this component is about to be
            // created by create_dir_all" case; everything else
            // (PermissionDenied, ELOOP, transient I/O) is abnormal and
            // we fail closed rather than silently skipping the guard
            // (Codex Item 3 LOW: the helper must not fail open).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "cannot verify symlink status for {}: {e}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn absolutize_project_dir(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        // Absolutize so a relative `--memd-data-dir ./data` passed to
        // `memd init` from CWD=X is persisted to `tenant_scope.json`
        // as an absolute path (/path/to/X/data). If we kept relative
        // values, later auto-discovery would reinterpret them against
        // the project root (the dir that contains `.memd/`), which
        // differs from the user's CWD at init time and points the
        // guard at the wrong directory (Codex Item 4 MEDIUM).
        // `normalize_absolute` is textual (no canonicalize) so the
        // path is not required to exist yet.
        return Ok(normalize_absolute(path));
    }
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    Ok(home.join(".memd").join("data"))
}

/// Walk ancestors of `start` looking for `.memd/tenant_scope.json`.
///
/// Returns the `data_dir` value from the first hit, or `None` if no
/// such file exists anywhere in the walk. Relative `data_dir` values
/// are resolved against the directory that contains `.memd/` — which
/// is what `memd init` intends when a user opts into a project-local
/// data dir.
///
/// First-match-wins: once we find any `.memd/tenant_scope.json`, that
/// IS the project boundary. A malformed JSON, missing-`data_dir`, or
/// unreadable file stops the walk and returns `None` — the caller
/// falls back to `$HOME/.memd/data`, rather than silently inheriting
/// an outer project's config (Codex Item 4 MEDIUM). Silent on IO /
/// parse errors so a broken project config doesn't crash the CLI.
fn discover_project_data_dir_from(start: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        let scope_path = dir.join(".memd").join("tenant_scope.json");
        if scope_path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&scope_path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(raw) = value.get("data_dir").and_then(|v| v.as_str()) {
                        let candidate = PathBuf::from(raw);
                        return Some(if candidate.is_absolute() {
                            candidate
                        } else {
                            dir.join(candidate)
                        });
                    }
                }
            }
            // Found the boundary file but couldn't extract data_dir;
            // stop here rather than fall through to an outer project.
            return None;
        }
        current = dir.parent();
    }
    None
}

/// Core resolver for `memd export-markdown`'s containment-guard
/// data_dirs. Always returns the list of paths the guard must refuse
/// the outdir against — never a single path — so auto-discovery can't
/// weaken the pre-refactor `$HOME/.memd/data` default (Codex Item 4
/// HIGH).
///
/// Priority / composition:
/// 1. If `--data-dir` is explicit, the guard checks ONLY that path.
///    This is the caller's declared intent and overrides both
///    discovery and the home default.
/// 2. Otherwise, the list includes `$HOME/.memd/data` AND any
///    `data_dir` discovered from a nearest-ancestor
///    `.memd/tenant_scope.json`. The guard refuses an outdir that is
///    inside ANY of those candidates, so an untrusted ancestor config
///    can't mask the default-install guard.
///
/// Split from `resolve_export_markdown_data_dirs` so tests can drive
/// it with an explicit `start_dir` instead of coupling to CWD.
fn resolve_export_markdown_data_dirs_from(
    explicit: Option<&Path>,
    start_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    if let Some(path) = explicit {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(start) = start_dir {
        if let Some(discovered) = discover_project_data_dir_from(start) {
            candidates.push(discovered);
        }
    }
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    let home_default = home.join(".memd").join("data");
    if !candidates.contains(&home_default) {
        candidates.push(home_default);
    }
    Ok(candidates)
}

/// Resolve the data_dir candidates for `memd export-markdown`'s
/// containment guard. See `resolve_export_markdown_data_dirs_from` for
/// priority and composition semantics. This wrapper supplies
/// `std::env::current_dir()` as the discovery start point.
fn resolve_export_markdown_data_dirs(explicit: Option<&Path>) -> Result<Vec<PathBuf>> {
    let cwd = std::env::current_dir().ok();
    resolve_export_markdown_data_dirs_from(explicit, cwd.as_deref())
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
        // Always persist data_dir — not just in scope=global — so
        // `memd export-markdown` (and any future CLI tool that needs
        // the containment guard) can auto-discover the daemon's data
        // directory from a nearest-ancestor `.memd/tenant_scope.json`
        // without forcing every caller to pass `--data-dir` explicitly.
        data_dir: Some(data_dir.display().to_string()),
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

async fn cli_search_payload<S: Store>(
    store: &S,
    tenant_id: String,
    project_id: Option<String>,
    query: String,
    k: usize,
    compact: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
) -> Result<Value> {
    let payload = direct_memory_search_payload(
        store,
        tenant_id.as_str(),
        project_id.as_deref(),
        query.as_str(),
        k,
        compact,
        token_budget,
        mode,
        no_text,
        include_artifact,
    )
    .await?;
    let result_count = payload
        .get("results")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    info!(count = result_count, "search complete");
    Ok(payload)
}

const MEMRERANKER_HELPER: &str = r#"
import json
import re
import sys
import time


def token_count(text):
    return len(re.findall(r"[A-Za-z0-9_]+", text or ""))


def emit(payload, code=0):
    print(json.dumps(payload, ensure_ascii=False))
    raise SystemExit(code)


def main():
    request = json.load(sys.stdin)
    query = request.get("query") or ""
    results = request.get("results") or []
    model_id = request.get("model") or "IAAR-Shanghai/MemReranker-4B"
    device = (request.get("device") or "auto").strip()
    batch_size = max(1, int(request.get("batch_size") or 1))

    try:
        import torch
    except Exception as exc:
        emit({"ok": False, "error": f"import torch failed: {exc}"}, 2)

    if device == "auto":
        if torch.cuda.is_available():
            device = "cuda"
        else:
            emit({"ok": False, "fallback_reason": "CUDA is not available"})
    elif device.startswith("cuda") and not torch.cuda.is_available():
        emit({"ok": False, "fallback_reason": f"requested device {device} but CUDA is not available"})

    try:
        from sentence_transformers import CrossEncoder
    except Exception as exc:
        emit({"ok": False, "error": f"import sentence_transformers.CrossEncoder failed: {exc}"}, 2)

    pairs = [(query, str(item.get("text") or "")) for item in results]
    if not pairs:
        emit({"ok": True, "scores": [], "metadata": {"model": model_id, "device": device, "pair_count": 0}})

    load_start = time.perf_counter()
    try:
        model = CrossEncoder(model_id, device=device, trust_remote_code=True)
    except Exception as exc:
        emit({"ok": False, "error": f"load CrossEncoder failed: {exc}"}, 2)
    load_seconds = time.perf_counter() - load_start

    rerank_start = time.perf_counter()
    try:
        raw_scores = model.predict(pairs, batch_size=batch_size)
    except Exception as exc:
        emit({"ok": False, "error": f"CrossEncoder prediction failed: {exc}"}, 2)
    rerank_seconds = time.perf_counter() - rerank_start

    scores = [float(score) for score in raw_scores]
    doc_tokens = sum(token_count(item.get("text") or "") for item in results)
    query_tokens = token_count(query)
    metadata = {
        "model": model_id,
        "device": device,
        "batch_size": batch_size,
        "pair_count": len(pairs),
        "load_seconds": round(load_seconds, 3),
        "rerank_seconds": round(rerank_seconds, 3),
        "avg_rerank_seconds_per_pair": round(rerank_seconds / max(1, len(pairs)), 6),
        "estimated_doc_tokens": doc_tokens,
        "estimated_query_tokens_once": query_tokens,
        "estimated_query_tokens_repeated": query_tokens * len(pairs),
        "estimated_pair_tokens": doc_tokens + query_tokens * len(pairs),
    }
    if device.startswith("cuda"):
        try:
            metadata["cuda_device_name"] = torch.cuda.get_device_name(torch.cuda.current_device())
        except Exception:
            pass
    emit({"ok": True, "scores": scores, "metadata": metadata})


main()
"#;

fn apply_search_reranker(
    payload: Value,
    query: &str,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    if options.reranker == SearchReranker::None {
        return Ok(payload);
    }

    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        return Ok(attach_reranker_fallback(
            payload,
            "no results to rerank",
            options,
        ));
    }

    let has_text = results.iter().any(|result| {
        result
            .get("text")
            .and_then(Value::as_str)
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false)
    });
    if !has_text {
        return fallback_or_error(payload, "search results do not include text", options);
    }

    if memreranker_needs_cuda(&options.device) && !cuda_probe_available() {
        return fallback_or_error(payload, "CUDA GPU is not visible to the CLI", options);
    }

    let helper_input = json!({
        "query": query,
        "results": results
            .iter()
            .map(|result| json!({
                "chunk_id": result.get("chunk_id").and_then(Value::as_str).unwrap_or(""),
                "text": result.get("text").and_then(Value::as_str).unwrap_or(""),
            }))
            .collect::<Vec<_>>(),
        "model": &options.model,
        "device": &options.device,
        "batch_size": options.batch_size.max(1),
    });

    match run_memreranker_helper(&helper_input, options) {
        Ok(helper_output) => {
            if helper_output
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                apply_memreranker_output(payload, helper_output, options)
            } else {
                let reason = helper_output
                    .get("fallback_reason")
                    .or_else(|| helper_output.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("MemReranker helper did not apply");
                fallback_or_error(payload, reason, options)
            }
        }
        Err(error) => fallback_or_error(payload, &error.to_string(), options),
    }
}

fn fallback_or_error(
    payload: Value,
    reason: &str,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    if options.reranker == SearchReranker::Auto {
        Ok(attach_reranker_fallback(payload, reason, options))
    } else {
        Err(MemdError::ValidationError(format!(
            "MemReranker-4B requested but unavailable: {reason}"
        )))
    }
}

fn attach_reranker_fallback(
    mut payload: Value,
    reason: impl Into<String>,
    options: &SearchRerankerOptions,
) -> Value {
    payload["reranker"] = json!({
        "requested": options.reranker,
        "applied": false,
        "fallback": "built_in_search_order",
        "reason": reason.into(),
        "model": &options.model,
        "device": &options.device,
    });
    payload
}

fn apply_memreranker_output(
    mut payload: Value,
    helper_output: Value,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    let scores = helper_output
        .get("scores")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MemdError::ProtocolError("MemReranker helper returned no scores".to_string())
        })?;
    let results = payload
        .get_mut("results")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| MemdError::ProtocolError("search payload has no results".to_string()))?;
    if scores.len() != results.len() {
        return Err(MemdError::ProtocolError(format!(
            "MemReranker returned {} scores for {} results",
            scores.len(),
            results.len()
        )));
    }

    for (result, score) in results.iter_mut().zip(scores) {
        let score = score.as_f64().ok_or_else(|| {
            MemdError::ProtocolError("MemReranker score is not numeric".to_string())
        })?;
        let old_score = result.get("score").cloned().unwrap_or(Value::Null);
        if let Some(object) = result.as_object_mut() {
            object.insert("pre_rerank_score".to_string(), old_score);
            object.insert("reranker_score".to_string(), json!(score));
            object.insert("score".to_string(), json!(score));
        }
    }
    results.sort_by(|left, right| {
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut metadata = helper_output
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert("requested".to_string(), json!(options.reranker));
        object.insert("applied".to_string(), json!(true));
        object.insert("fallback".to_string(), Value::Null);
    }
    payload["reranker"] = metadata;
    Ok(payload)
}

fn run_memreranker_helper(input: &Value, options: &SearchRerankerOptions) -> Result<Value> {
    let timeout = format!("{}s", options.timeout_seconds.max(1));
    let mut child = Command::new("timeout")
        .arg(timeout)
        .arg(&options.python)
        .arg("-c")
        .arg(MEMRERANKER_HELPER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| MemdError::ProtocolError(format!("start MemReranker helper: {err}")))?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            MemdError::ProtocolError("MemReranker helper stdin unavailable".to_string())
        })?;
        stdin.write_all(serde_json::to_string(input)?.as_bytes())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| MemdError::ProtocolError(format!("wait for MemReranker helper: {err}")))?;
    if !output.status.success() {
        if !output.stdout.is_empty() {
            if let Ok(value) = serde_json::from_slice(&output.stdout) {
                return Ok(value);
            }
        }
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(MemdError::ProtocolError(format!(
            "MemReranker helper exited with {code}: stdout: {}; stderr: {}",
            trim_for_error(&stdout),
            trim_for_error(&stderr)
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|err| {
        MemdError::ProtocolError(format!(
            "parse MemReranker helper output: {err}; stderr: {}",
            trim_for_error(&String::from_utf8_lossy(&output.stderr))
        ))
    })
}

fn memreranker_needs_cuda(device: &str) -> bool {
    let device = device.trim().to_ascii_lowercase();
    device == "auto" || device.starts_with("cuda")
}

fn cuda_probe_available() -> bool {
    Command::new("nvidia-smi")
        .arg("-L")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn trim_for_error(text: &str) -> String {
    const MAX_LEN: usize = 1600;
    let text = text.trim();
    if text.chars().count() <= MAX_LEN {
        text.to_string()
    } else {
        let tail: String = text
            .chars()
            .rev()
            .take(MAX_LEN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("...{tail}")
    }
}

async fn cli_agent_context_payload<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    queries: &[String],
    k: usize,
    token_budget: usize,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
) -> Result<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut merged_results = Vec::new();
    let mut query_summaries = Vec::new();

    for query in queries {
        let payload = direct_memory_search_payload(
            store,
            tenant_id,
            project_id,
            query,
            k,
            true,
            Some(token_budget),
            mode,
            no_text,
            include_artifact,
        )
        .await?;
        let results = payload
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for result in &results {
            let Some(chunk_id) = result.get("chunk_id").and_then(Value::as_str) else {
                continue;
            };
            if seen.insert(chunk_id.to_string()) {
                merged_results.push(result.clone());
            }
        }
        query_summaries.push(json!({
            "query": query,
            "result_count": results.len(),
            "budget_info": payload.get("budget_info").cloned().unwrap_or(Value::Null),
        }));
    }

    Ok(json!({
        "tool": "memd.agent_context",
        "interface": "cli_prefetch",
        "retrieval_backend": "direct_store",
        "tenant_id": tenant_id,
        "project_id": project_id,
        "queries": query_summaries,
        "k_per_query": k,
        "token_budget_per_query": token_budget,
        "result_count": merged_results.len(),
        "results": merged_results,
    }))
}

async fn direct_memory_search_payload<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    query: &str,
    k: usize,
    compact: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
) -> Result<Value> {
    let params = SearchParams {
        tenant_id: tenant_id.to_string(),
        query: query.to_string(),
        project_id: project_id.map(ToString::to_string),
        k,
        mode: Some(mode.into()),
        compact,
        token_budget,
        include_text: no_text.then_some(false),
        include_artifact: include_artifact.then_some(true),
        ..Default::default()
    };
    let mcp_value = handle_memory_search(store, params)
        .await
        .map_err(|e| MemdError::ProtocolError(e.to_string()))?;
    unwrap_content_payload(mcp_value)
}

fn parse_call_arguments(json_arg: Option<&str>, input: Option<&Path>) -> Result<Value> {
    let value = if let Some(path) = input {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else if let Some(json_arg) = json_arg {
        serde_json::from_str(json_arg)?
    } else {
        json!({})
    };

    if value.is_object() || value.is_null() {
        Ok(value)
    } else {
        Err(MemdError::ValidationError(
            "call arguments must be a JSON object".to_string(),
        ))
    }
}

fn parse_tool_params<T: DeserializeOwned>(
    tool: &str,
    arguments: Value,
) -> std::result::Result<T, McpError> {
    serde_json::from_value(arguments)
        .map_err(|e| McpError::InvalidParams(format!("invalid {tool} params: {e}")))
}

struct CliStructuralRuntime {
    structural_store: Arc<StructuralStore>,
    symbol_indexer: Arc<SymbolIndexer>,
    call_graph_indexer: Arc<CallGraphIndexer>,
    symbol_query_service: Arc<SymbolQueryService>,
    trace_query_service: Arc<TraceQueryService>,
}

impl CliStructuralRuntime {
    fn open(data_dir: &Path) -> std::result::Result<Self, McpError> {
        let structural_store = Arc::new(
            StructuralStore::open(&data_dir.join("structural.db"))
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
        Ok(Self {
            structural_store: structural_store.clone(),
            symbol_indexer: Arc::new(SymbolIndexer::new(structural_store.clone())),
            call_graph_indexer: Arc::new(CallGraphIndexer::new(structural_store.clone())),
            symbol_query_service: Arc::new(SymbolQueryService::new(structural_store.clone())),
            trace_query_service: Arc::new(TraceQueryService::new(structural_store)),
        })
    }
}

fn ensure_structural_runtime<'a>(
    slot: &'a mut Option<CliStructuralRuntime>,
    tenant_manager: Option<&TenantManager>,
) -> std::result::Result<&'a CliStructuralRuntime, McpError> {
    if slot.is_none() {
        let tenant_manager = tenant_manager.ok_or_else(|| {
            McpError::ToolError("structural index requires a persistent data directory".to_string())
        })?;
        *slot = Some(CliStructuralRuntime::open(tenant_manager.data_dir())?);
    }
    Ok(slot.as_ref().expect("structural runtime initialized"))
}

fn maybe_index_structural_chunk(
    slot: &mut Option<CliStructuralRuntime>,
    tenant_manager: Option<&TenantManager>,
    tenant_id: &str,
    project_id: Option<&str>,
    chunk_type: &str,
    source_path: Option<&str>,
    text: &str,
) {
    if !chunk_type.eq_ignore_ascii_case("code") {
        return;
    }
    let Some(source_path) = source_path.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    let runtime = match ensure_structural_runtime(slot, tenant_manager) {
        Ok(runtime) => runtime,
        Err(error) => {
            warn!(
                tenant_id = tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because the local runtime is unavailable"
            );
            return;
        }
    };

    let path = Path::new(source_path);
    if crate::structural::detect_language(path).is_none() {
        return;
    }

    let tenant_id = match TenantId::new(tenant_id) {
        Ok(tenant_id) => tenant_id,
        Err(error) => {
            warn!(
                tenant_id = tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because tenant validation failed"
            );
            return;
        }
    };

    let parsed = match crate::structural::parse_file(path, text) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn!(
                tenant_id = %tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because parsing failed"
            );
            return;
        }
    };

    if let Err(error) = runtime.symbol_indexer.index_file(
        &tenant_id,
        project_id,
        source_path,
        &parsed.tree,
        text.as_bytes(),
        parsed.language,
    ) {
        warn!(
            tenant_id = %tenant_id,
            source_path = source_path,
            error = %error,
            "skipping structural indexing because symbol indexing failed"
        );
        return;
    }

    let file_symbols = match runtime
        .structural_store
        .find_symbols_by_file(&tenant_id, source_path)
    {
        Ok(symbols) => symbols,
        Err(error) => {
            warn!(
                tenant_id = %tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because symbol lookup failed"
            );
            return;
        }
    };

    let call_graph_symbols = file_symbols
        .iter()
        .filter_map(|symbol| {
            symbol.symbol_id.map(|symbol_id| CallGraphSymbolRecord {
                symbol_id,
                name: symbol.name.clone(),
                start_line: symbol.line_start,
                end_line: symbol.line_end,
            })
        })
        .collect::<Vec<_>>();

    if let Err(error) = runtime.call_graph_indexer.index_file(
        &tenant_id,
        source_path,
        &parsed.tree,
        text.as_bytes(),
        parsed.language,
        &call_graph_symbols,
    ) {
        warn!(
            tenant_id = %tenant_id,
            source_path = source_path,
            error = %error,
            "skipping structural indexing because call graph indexing failed"
        );
    }
}

async fn cli_call_tool<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    tool: &str,
    arguments: Value,
) -> std::result::Result<Value, McpError> {
    let metrics = MetricsCollector::default();
    let mut structural_runtime: Option<CliStructuralRuntime> = None;

    match tool {
        "memory.search" => {
            let params: SearchParams = parse_tool_params(tool, arguments)?;
            handle_memory_search(store, params).await
        }
        "memory.add" => {
            let params: AddParams = parse_tool_params(tool, arguments)?;
            let tenant_id = params.tenant_id.clone();
            let project_id = params.project_id.clone();
            let chunk_type = params.chunk_type.clone();
            let source_path = params
                .source
                .as_ref()
                .and_then(|source| source.path.as_deref())
                .map(str::to_string);
            let text = params.text.clone();
            let response = handle_memory_add(store, tenant_manager, params).await?;
            maybe_index_structural_chunk(
                &mut structural_runtime,
                tenant_manager,
                &tenant_id,
                project_id.as_deref(),
                &chunk_type,
                source_path.as_deref(),
                &text,
            );
            Ok(response)
        }
        "memory.add_batch" => {
            let params: AddBatchParams = parse_tool_params(tool, arguments)?;
            let tenant_id = params.tenant_id.clone();
            let chunks_to_index = params
                .chunks
                .iter()
                .map(|chunk| {
                    (
                        chunk.project_id.clone(),
                        chunk.chunk_type.clone(),
                        chunk.source.as_ref().and_then(|source| source.path.clone()),
                        chunk.text.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let response = handle_memory_add_batch(store, tenant_manager, params).await?;
            for (project_id, chunk_type, source_path, text) in chunks_to_index {
                maybe_index_structural_chunk(
                    &mut structural_runtime,
                    tenant_manager,
                    &tenant_id,
                    project_id.as_deref(),
                    &chunk_type,
                    source_path.as_deref(),
                    &text,
                );
            }
            Ok(response)
        }
        "task.start" => {
            let params: TaskStartParams = parse_tool_params(tool, arguments)?;
            handle_task_start(store, tenant_manager, params).await
        }
        "task.progress" => {
            let params: TaskProgressParams = parse_tool_params(tool, arguments)?;
            handle_task_progress(store, tenant_manager, params).await
        }
        "task.run_start" => {
            let params: TaskRunStartParams = parse_tool_params(tool, arguments)?;
            handle_task_run_start(store, tenant_manager, params).await
        }
        "task.run_finish" => {
            let params: TaskRunFinishParams = parse_tool_params(tool, arguments)?;
            handle_task_run_finish(store, tenant_manager, params).await
        }
        "task.add_evidence" => {
            let params: TaskAddEvidenceParams = parse_tool_params(tool, arguments)?;
            handle_task_add_evidence(store, tenant_manager, params).await
        }
        "task.finish" => {
            let params: TaskFinishParams = parse_tool_params(tool, arguments)?;
            handle_task_finish(store, tenant_manager, params).await
        }
        "task.get" => {
            let params: TaskGetParams = parse_tool_params(tool, arguments)?;
            handle_task_get(store, params).await
        }
        "task.search" => {
            let params: TaskSearchParams = parse_tool_params(tool, arguments)?;
            handle_task_search(store, params).await
        }
        "task.resume" => {
            let params: TaskResumeParams = parse_tool_params(tool, arguments)?;
            handle_task_resume(store, params).await
        }
        "artifact.create" => {
            let params: ArtifactCreateParams = parse_tool_params(tool, arguments)?;
            handle_artifact_create(store, tenant_manager, params).await
        }
        "artifact.review" | "artifact.revision" | "artifact.decision" | "artifact.verification" => {
            let kind = match tool {
                "artifact.review" => "review",
                "artifact.revision" => "revision",
                "artifact.decision" => "decision",
                "artifact.verification" => "verification",
                _ => unreachable!(),
            };
            let mut arguments = arguments;
            if let Some(obj) = arguments.as_object_mut() {
                if let Some(existing) = obj.get("artifact_kind") {
                    if existing.as_str() != Some(kind) {
                        return Err(McpError::InvalidParams(format!(
                            "{tool} forbids an overriding artifact_kind; got {existing}"
                        )));
                    }
                }
                obj.insert("artifact_kind".to_string(), Value::String(kind.to_string()));
            }
            let params: ArtifactCreateParams = parse_tool_params(tool, arguments)?;
            handle_artifact_create(store, tenant_manager, params).await
        }
        "artifact.get" => {
            let params: ArtifactGetParams = parse_tool_params(tool, arguments)?;
            handle_artifact_get(store, params).await
        }
        "artifact.search" => {
            let params: TaskSearchParams = parse_tool_params(tool, arguments)?;
            handle_artifact_search(store, params).await
        }
        "artifact.find_related" | "artifact.verify" => {
            let params: ArtifactVerifyParams = parse_tool_params(tool, arguments)?;
            handle_artifact_verify(store, params).await
        }
        "artifact.find_failures" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_failures(store, params).await
        }
        "artifact.find_decisions" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_decisions(store, params).await
        }
        "artifact.find_evidence" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_evidence(store, params).await
        }
        "artifact.find_highlights" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_highlights(store, params).await
        }
        "artifact.list_thread" => {
            let params: ArtifactListThreadParams = parse_tool_params(tool, arguments)?;
            handle_artifact_list_thread(store, params).await
        }
        "memory.get" => {
            let params: GetParams = parse_tool_params(tool, arguments)?;
            handle_memory_get(store, params).await
        }
        "memory.delete" => {
            let params: DeleteParams = parse_tool_params(tool, arguments)?;
            handle_memory_delete(store, params).await
        }
        "memory.feedback" => {
            let params: FeedbackParams = parse_tool_params(tool, arguments)?;
            handle_memory_feedback(store, params).await
        }
        "memory.stats" => {
            let params: StatsParams = parse_tool_params(tool, arguments)?;
            handle_memory_stats(store, tenant_manager, params).await
        }
        "memory.metrics" => {
            let params: MetricsParams = parse_tool_params(tool, arguments)?;
            let index_stats = store.get_index_stats(None);
            handle_memory_metrics(&metrics, index_stats, params)
        }
        "memory.health" => {
            let params: HealthParams = parse_tool_params(tool, arguments)?;
            handle_memory_health(store, &metrics, params).await
        }
        "memory.compact" => {
            let params: CompactParams = parse_tool_params(tool, arguments)?;
            handle_memory_compact(store, params).await
        }
        "memory.dream" => {
            let params: DreamParams = parse_tool_params(tool, arguments)?;
            handle_memory_dream(store, tenant_manager, params).await
        }
        "memory.supersede" => {
            let params: SupersedeParams = parse_tool_params(tool, arguments)?;
            let (response, event) = handle_memory_supersede(store, tenant_manager, params).await?;
            maybe_index_structural_chunk(
                &mut structural_runtime,
                tenant_manager,
                &event.tenant_id,
                event.project_id.as_deref(),
                &event.chunk_type,
                event.source_path.as_deref(),
                &event.text,
            );
            Ok(response)
        }
        "memory.set_expiry" => {
            let params: SetExpiryParams = parse_tool_params(tool, arguments)?;
            handle_memory_set_expiry(store, tenant_manager, params).await
        }
        "memory.find_near_duplicates" => {
            let params: FindNearDuplicatesParams = parse_tool_params(tool, arguments)?;
            handle_memory_find_near_duplicates(store, params).await
        }
        "memory.export_markdown" => {
            let params: ExportMarkdownParams = parse_tool_params(tool, arguments)?;
            handle_memory_export_markdown(store, params).await
        }
        "memory.export_omf" => {
            let params: ExportOmfParams = parse_tool_params(tool, arguments)?;
            handle_memory_export_omf(store, params).await
        }
        "memory.preview_omf_import" => {
            let params: PreviewOmfImportParams = parse_tool_params(tool, arguments)?;
            handle_memory_preview_omf_import(store, params).await
        }
        "memory.import_omf" => {
            let params: ImportOmfParams = parse_tool_params(tool, arguments)?;
            let (response, events) = handle_memory_import_omf(store, tenant_manager, params).await?;
            for event in &events {
                maybe_index_structural_chunk(
                    &mut structural_runtime,
                    tenant_manager,
                    &event.tenant_id,
                    event.project_id.as_deref(),
                    &event.chunk_type,
                    event.source_path.as_deref(),
                    &event.text,
                );
            }
            Ok(response)
        }
        "memory.consolidate_episode" => {
            let params: ConsolidateEpisodeParams = parse_tool_params(tool, arguments)?;
            handle_memory_consolidate_episode(store, params).await
        }
        "context.list_subsystems" => {
            let params: ContextListSubsystemsParams = parse_tool_params(tool, arguments)?;
            handle_context_list_subsystems(store, params).await
        }
        "context.get_files_for_subsystem" => {
            let params: ContextGetFilesForSubsystemParams = parse_tool_params(tool, arguments)?;
            handle_context_get_files_for_subsystem(store, params).await
        }
        "context.search_context_documents" => {
            let params: ContextSearchDocumentsParams = parse_tool_params(tool, arguments)?;
            handle_context_search_documents(store, params).await
        }
        "context.find_relevant_context" => {
            let params: ContextFindRelevantContextParams = parse_tool_params(tool, arguments)?;
            handle_context_find_relevant_context(store, params).await
        }
        "context.brief_project" => {
            let params: ProjectBriefParams = parse_tool_params(tool, arguments)?;
            handle_context_brief_project(store, params).await
        }
        "context.suggest_agent" => {
            let params: ContextSuggestAgentParams = parse_tool_params(tool, arguments)?;
            handle_context_suggest_agent(store, params).await
        }
        "context.get_hot_context" => {
            let params: ContextGetHotContextParams = parse_tool_params(tool, arguments)?;
            handle_context_get_hot_context(store, params).await
        }
        "code.find_definition" => {
            let params: FindDefinitionParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_definition(runtime.symbol_query_service.as_ref(), params)
        }
        "code.find_references" => {
            let params: FindReferencesParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_references(runtime.symbol_query_service.as_ref(), params)
        }
        "code.find_callers" => {
            let params: FindCallersParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_callers(runtime.symbol_query_service.as_ref(), params)
        }
        "code.find_imports" => {
            let params: FindImportsParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_imports(runtime.symbol_query_service.as_ref(), params)
        }
        "debug.find_tool_calls" => {
            let params: FindToolCallsParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_tool_calls(runtime.trace_query_service.as_ref(), params)
        }
        "debug.find_errors" => {
            let params: FindErrorsParams = parse_tool_params(tool, arguments)?;
            let runtime = ensure_structural_runtime(&mut structural_runtime, tenant_manager)?;
            handle_find_errors(runtime.trace_query_service.as_ref(), params)
        }
        _ => Err(McpError::MethodNotFound(format!("unknown tool '{tool}'"))),
    }
}

fn unwrap_content_payload(value: Value) -> Result<Value> {
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

fn render_search_payload(payload: &Value, format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Json => Ok(serde_json::to_string_pretty(payload)? + "\n"),
        ExportFormat::Jsonl => render_results_jsonl(payload),
        ExportFormat::Markdown => render_memory_markdown(payload, "memd search"),
    }
}

fn render_agent_context(payload: &Value, format: ExportFormat) -> Result<String> {
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
                out.push_str("\n");
                out.push_str(text.trim());
                out.push_str("\n");
            }
        }
        out.push('\n');
    }
    Ok(out)
}

fn write_rendered(path: Option<&Path>, rendered: &str) -> Result<()> {
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

fn write_cli_log(log_dir: Option<&Path>, prefix: &str, payload: &Value) -> Result<()> {
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

fn render_guardrail_block(scope_config: &TenantScopeConfig, memd_command: &str) -> String {
    let mut out = String::new();
    out.push_str("<!-- memd-guardrails:start -->\n");
    out.push_str("## memd CLI Memory Guardrails\n\n");
    out.push_str("Use the `memd` CLI for persistent memory in this repository.\n\n");
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
    out.push_str("- Hard rule: do not send a final substantive answer without CLI memory retrieval and a CLI memory write.\n\n");
    out.push_str("### Mandatory CLI Protocol\n\n");
    out.push_str("1. Retrieve first with `memd agent-context` or `memd search`.\n");
    out.push_str(&format!(
        "   - Default context file command: `{memd_command} agent-context --tenant-id {} --query \"<task>\" --k 2 --token-budget 700 --format markdown --output .memd/context.md --log-dir .memd/search-logs`.\n",
        scope_config.write_tenant
    ));
    out.push_str(&format!(
        "   - Direct search command: `{memd_command} search --tenant-id {} --query \"<task>\" --compact --token-budget 2000 --format markdown`.\n",
        scope_config.write_tenant
    ));
    if scope_config.scope == TenantScopeMode::Global {
        out.push_str("   - In global mode, the tenant list is a snapshot from init-time data directory discovery. Re-run `memd init` to refresh.\n");
    }
    out.push_str("2. Implement using retrieved context.\n");
    out.push_str("3. Persist before final response with `memd add`.\n");
    out.push_str(
        "   - Write only to the required write tenant; include `--project-id` when known and tags such as `kind:progress`, `kind:evidence`, `kind:decision`, or `kind:finish`.\n",
    );
    out.push_str("4. If memd is unavailable:\n");
    out.push_str(
        "   - Explicitly report memory persistence failure and stop before final answer.\n\n",
    );
    out.push_str("### Suggested CLI Write Template\n\n");
    out.push_str(&format!(
        "`{memd_command} add --tenant-id {} --project-id <project> --chunk-type summary --tags session:<id>,kind:progress --text \"<what changed and why it matters>\"`\n\n",
        scope_config.write_tenant
    ));
    out.push_str("Use tags such as:\n");
    out.push_str("- `ctx:doc`\n");
    out.push_str("- `ctx:subsystem:<name>`\n");
    out.push_str("- `ctx:file:<path>`\n");
    out.push_str("- `session:<id>`\n");
    out.push_str("- `kind:progress|run|evidence|decision|finish`\n");
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

    #[tokio::test]
    async fn batch_jsonl_runs_multiple_calls_through_one_store() {
        let store = MemoryStore::new();
        let input = r#"
{"tool":"memory.add","arguments":{"tenant_id":"batch_tenant","project_id":"batch_project","type":"doc","text":"batch marker one"}}
{"tool":"memory.stats","arguments":{"tenant_id":"batch_tenant"}}
"#;

        let rendered = run_batch_jsonl(&store, None, input, false)
            .await
            .unwrap();
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
        std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            "{not json",
        )
        .unwrap();
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
        let resolved =
            resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
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
            resolve_export_markdown_data_dirs_from(Some(&explicit), Some(dir.path()))
                .unwrap();
        assert_eq!(resolved, vec![explicit]);
    }

    #[test]
    fn resolve_export_markdown_data_dirs_from_falls_back_to_home_when_no_project() {
        let dir = tempdir().unwrap();
        let resolved =
            resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
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
