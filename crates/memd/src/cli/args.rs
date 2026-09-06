use std::path::PathBuf;

use clap::{ArgAction, Subcommand};
use serde::{Deserialize, Serialize};

use crate::mcp::handlers::QueryMode;
use crate::types::ChunkType;

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

/// Report output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    /// Human-readable Markdown.
    Markdown,
    /// Pretty JSON report.
    Json,
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
pub(super) struct SearchRerankerOptions {
    pub(super) reranker: SearchReranker,
    pub(super) model: String,
    pub(super) device: String,
    pub(super) batch_size: usize,
    pub(super) timeout_seconds: u64,
    pub(super) python: String,
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
pub(super) struct TenantScopeConfig {
    pub(super) primary_tenant: String,
    pub(super) write_tenant: String,
    /// Data directory used by export-markdown containment discovery
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProjectScopeConfig {
    pub(super) tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) project_id: Option<String>,
    pub(super) interface: String,
    pub(super) cli_command: String,
    pub(super) agent_context_output: String,
    pub(super) project_dir: String,
}

/// CLI subcommands for memory operations
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Add a memory chunk
    Add {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,

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

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Search memory chunks
    Search {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,

        /// Search query. Also accepts a bare positional argument.
        #[arg(long, required_unless_present = "query_positional")]
        query: Option<String>,

        /// Search query given positionally (`memd search "<query>"`).
        #[arg(value_name = "QUERY", conflicts_with = "query")]
        query_positional: Option<String>,

        /// Maximum number of results
        #[arg(long, visible_alias = "limit", default_value = "10")]
        k: usize,

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Use memory.search compact shaping instead of the legacy raw chunk array
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        compact: bool,

        /// Collapse results sharing a source URI to the best-ranked one, so
        /// fragments of one document don't crowd out other relevant sources
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        dedupe_by_source: bool,

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

        /// Include superseded chunks (hidden by default) — for
        /// provenance lookups of consolidated lessons.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        include_superseded: bool,

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
        tenant_id: Option<String>,

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Optional non-sensitive task identifier attached as plaintext.
        #[arg(long)]
        task_id: Option<String>,

        /// Optional non-sensitive thread identifier attached as plaintext.
        #[arg(long)]
        thread_id: Option<String>,

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

    /// Refresh a project `memory.md` file with the highest-priority takeaways.
    ///
    /// This is intended for agent session startup. It distills up to 10
    /// project-scoped fact-library items from memd, writes them to `memory.md`,
    /// and prints a JSON summary. Up to 2 machine-wide fact-library items are
    /// included by default; tune with `--global-limit` (0 disables).
    MemoryMd {
        /// Tenant identifier. Defaults to `.memd/project_scope.json` when present.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier. Defaults to `.memd/project_scope.json`.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory containing `.memd/project_scope.json`
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Output path. Relative paths are resolved under `--project-dir`.
        #[arg(long, default_value = "memory.md")]
        output: PathBuf,

        /// Maximum project-specific takeaways to keep, capped at 10
        #[arg(long, default_value_t = 10)]
        project_limit: usize,

        /// Maximum machine-wide fact-library items to keep, capped at 10.
        /// Defaults to 2; 0 disables the section.
        #[arg(long, default_value_t = 2)]
        global_limit: usize,

        /// Unused since scan-first selection considers every stored
        /// chunk; kept for CLI compatibility.
        #[arg(long, default_value_t = 40)]
        candidate_k: usize,

        /// Optional JSON report explaining retrieved candidates,
        /// priority score components, and display/filter decisions.
        #[arg(long)]
        explain_output: Option<PathBuf>,
    },

    /// Evaluate default `memory.md` startup quality against fixed thresholds.
    EvalMemoryMd {
        /// Tenant identifier. Defaults to `.memd/project_scope.json` when present.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier. Defaults to `.memd/project_scope.json`.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory containing `.memd/project_scope.json`.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Output path for the generated memory.md under evaluation.
        #[arg(long, default_value = "memory.md")]
        output: PathBuf,

        /// Maximum project-specific takeaways to evaluate, capped at 10.
        #[arg(long, default_value_t = 10)]
        project_limit: usize,

        /// Unused since scan-first selection considers every stored
        /// chunk; kept for CLI compatibility.
        #[arg(long, default_value_t = 40)]
        candidate_k: usize,

        /// Number of displayed project takeaways included in useful-ratio scoring.
        #[arg(long, default_value_t = 10)]
        top_n: usize,

        /// Minimum useful displayed-item ratio required for success.
        #[arg(long, default_value_t = 0.8)]
        min_useful_ratio: f64,

        /// Maximum generated wrapper records allowed in displayed project items.
        #[arg(long, default_value_t = 0)]
        max_generated_wrappers: usize,

        /// Enable structured startup-briefing usefulness checks.
        #[arg(long)]
        agent_usefulness: bool,

        /// Optional JSON file with local project expectations.
        #[arg(long)]
        gold_file: Option<PathBuf>,
    },

    /// Evaluate fixed retrieval queries with known useful chunk IDs.
    EvalRetrieval {
        /// Tenant identifier.
        #[arg(long)]
        tenant_id: String,

        /// Optional project identifier.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory (resolves default queries and reports).
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Path to JSONL queries with `query` and `useful_chunk_ids`.
        #[arg(long)]
        queries: Option<PathBuf>,

        /// Top-k retrieval cutoff for precision and hit-rate metrics.
        #[arg(long, default_value_t = 5)]
        k: usize,

        /// Minimum mean precision@k required for success. Defaults to 0
        /// because bundled sparse judgments often cannot reach a fixed
        /// precision threshold at k.
        #[arg(long, default_value_t = 0.0)]
        min_precision_at_k: f64,

        /// Minimum fraction of queries with at least one useful hit.
        #[arg(long, default_value_t = 1.0)]
        min_hit_rate_at_k: f64,

        /// Minimum mean recall over known useful chunk IDs.
        #[arg(long, default_value_t = 0.0)]
        min_known_recall_at_k: f64,

        /// Minimum mean reciprocal rank of the first useful hit.
        #[arg(long, default_value_t = 0.0)]
        min_mrr: f64,
    },

    /// Evaluate synthetic write admission, dedupe, and storage-growth behavior.
    EvalWriteQuality {
        /// Project directory used only for the eval report output path.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Minimum low-value write rejection/downgrade rate.
        #[arg(long, default_value_t = 1.0)]
        min_rejection_or_downgrade_rate: f64,

        /// Minimum exact duplicate reuse rate.
        #[arg(long, default_value_t = 1.0)]
        min_duplicate_reuse_rate: f64,

        /// Maximum chunks a synthetic session may leave in its isolated store.
        #[arg(long, default_value_t = 6)]
        max_total_chunks: usize,

        /// Maximum isolated persistent-store byte growth for the synthetic session.
        #[arg(long, default_value_t = 5_000_000)]
        max_disk_bytes: u64,

        /// Require the synthetic retention/compaction stage to expire at least one row.
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        require_retention_compaction: bool,
    },

    /// Consolidate recent memory chunks into deduplicated lessons.
    ///
    /// Builds a working region from chunks written/retrieved since the
    /// last run, asks the configured LLM consolidator to rewrite them,
    /// and stages hidden `kind:consolidated` candidates for review.
    /// Backend is chosen by `MEMD_CONSOLIDATOR`.
    Consolidate {
        /// Tenant identifier. Defaults to `.memd/project_scope.json`
        /// or `.memd/config.json` when present.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier. Defaults to the scope file
        /// only when --tenant-id is also omitted.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory containing the `.memd` scope/state files.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Maximum chunks in the consolidation working region.
        #[arg(long, default_value_t = 50)]
        max_region: usize,

        /// Build and print the prompt without calling the LLM.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        dry_run: bool,

        /// Run consolidation in a detached background process.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        background: bool,

        /// Consolidate even when the region is below the threshold.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        force: bool,

        /// Promote candidates after deterministic validation. Without this,
        /// consolidation stops at a hidden validated candidate.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        promote: bool,

        /// Preserve the former immediate-promotion behavior for one release.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue, conflicts_with = "promote")]
        legacy_immediate: bool,

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Accept or reject a validated staged consolidation run.
    ConsolidateReview {
        /// Consolidation run UUID returned by `memd consolidate`.
        run_id: Option<String>,

        /// List validated runs awaiting review.
        #[arg(long, action = ArgAction::SetTrue, conflicts_with_all = ["accept", "reject"])]
        list: bool,

        /// Maximum staged runs returned by --list.
        #[arg(long, default_value_t = 100)]
        limit: usize,

        /// Promote the validated candidates and apply their lineage policy.
        #[arg(long, action = ArgAction::SetTrue, conflicts_with_all = ["reject", "list"])]
        accept: bool,

        /// Reject the run and keep every candidate hidden.
        #[arg(long, action = ArgAction::SetTrue, conflicts_with_all = ["accept", "list"])]
        reject: bool,
    },

    /// Record a verified task outcome against one retrieval episode.
    ///
    /// Only chunks that were rendered in the named episode may be attributed.
    /// Agent self-reports are retained for audit but never train ranking.
    Outcome {
        /// Retrieval episode UUID returned by search or agent-context.
        episode_id: String,

        /// Tenant identifier. Defaults to the current project scope.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Outcome: passed, accepted, corrected, failed, abandoned, or
        /// verifier_error when the verifier produced no verdict.
        #[arg(long)]
        outcome: String,

        /// Verifier: user, automated_test, external_tool, task_system, or agent_self_report.
        #[arg(long)]
        verifier: String,

        /// Rendered chunks that materially helped (comma-separated).
        #[arg(long, value_delimiter = ',')]
        used: Vec<String>,

        /// Rendered chunks that caused harm or required correction (comma-separated).
        #[arg(long, value_delimiter = ',')]
        harmful: Vec<String>,

        /// Non-sensitive reference to a durable test, task, or tool result.
        #[arg(long)]
        evidence: Option<String>,

        /// Event time as Unix epoch milliseconds; defaults to now.
        #[arg(long)]
        event_time_ms: Option<i64>,

        /// Route this write through the local warm worker.
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Scan Codex session logs for verified memd retrieval usage.
    ///
    /// Detects tool-call outputs that rendered a retrieval episode, then
    /// credits served chunks whose distinctive literals appear in later
    /// tool-call inputs (commands or patches) of the same session. Writes
    /// `external_tool` outcome events; `.memd/data/outcome_scan_state.json`
    /// keeps re-runs from writing duplicate events.
    OutcomeScan {
        /// Project directory containing `.memd/project_scope.json`.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Session log directory, scanned recursively for `*.jsonl` files.
        /// Defaults to `~/.codex/sessions`.
        #[arg(long)]
        sessions_dir: Option<PathBuf>,

        /// Only scan session files modified within this many days.
        #[arg(long, default_value_t = 90)]
        since_days: u64,

        /// Report candidate outcome events without writing events or state.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        dry_run: bool,
    },

    /// Counterfactual retrieval eval (Phase 3).
    ///
    /// For each query in the benchmark file, runs retrieval twice
    /// (full bank vs. `kind:consolidated`-filtered) and reports the
    /// overlap@k loss and mean rank shift. Writes a Markdown report
    /// under `evals/bench/reports/`.
    EvalCounterfactual {
        /// Tenant identifier.
        #[arg(long)]
        tenant_id: String,

        /// Optional project identifier.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory (resolves default queries file).
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Path to the JSONL queries file. Defaults to
        /// `evals/bench/queries/counterfactual_queries.jsonl` under
        /// the project directory.
        #[arg(long)]
        queries: Option<PathBuf>,

        /// Top-k for retrieval comparison.
        #[arg(long, default_value_t = 5)]
        k: usize,
    },

    /// Compare served retrieval order with the outcome-v1 shadow policy.
    ///
    /// Query rows declare relevant and harmful chunk IDs. The command records
    /// normal privacy-safe retrieval episodes, reconstructs a source-deduped
    /// shadow top-k, and writes JSON plus Markdown counterfactual artifacts.
    EvalOutcomeRanking {
        /// Tenant identifier.
        #[arg(long)]
        tenant_id: String,

        /// Optional project identifier.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory used to resolve relative paths.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// JSONL query rows with id, query, relevant_chunk_ids, and harmful_chunk_ids.
        #[arg(long)]
        queries: PathBuf,

        /// Top-k for served-versus-shadow comparison.
        #[arg(long, default_value_t = 5)]
        k: usize,

        /// JSON report path. The command also writes a sibling Markdown file.
        #[arg(long)]
        report_json: PathBuf,
    },

    /// Session-start hook entry point.
    ///
    /// Refreshes `memory.md` synchronously and, when enough chunks
    /// have accumulated, spawns a detached background consolidation.
    /// Safe to wire into a SessionStart hook for every repo: a missing
    /// `.memd` scope is created automatically unless `MEMD_AUTO_SCOPE=0`
    /// or a `.memd-skip` marker disables startup for the repository.
    SessionStart {
        /// Project directory containing the `.memd` scope/state files.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,
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

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
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
        /// Embedding-model identity this worker serves, injected by the parent
        /// before dispatch (not parsed from argv). Reported in the ping
        /// identity so a client requesting a different model respawns it.
        #[arg(skip)]
        embedding_model: Option<String>,
        /// Search-variant identity this worker serves (injected, see above).
        #[arg(skip)]
        search_variant: Option<String>,
    },

    /// Get a chunk by ID
    Get {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,

        /// Chunk identifier (UUID). Also accepts a bare positional argument.
        #[arg(long, required_unless_present = "chunk_id_positional")]
        chunk_id: Option<String>,

        /// Chunk identifier given positionally (`memd get <chunk-id>`).
        #[arg(value_name = "CHUNK_ID", conflicts_with = "chunk_id")]
        chunk_id_positional: Option<String>,
    },

    /// Delete a chunk (soft delete)
    Delete {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,

        /// Chunk identifier (UUID)
        #[arg(long)]
        chunk_id: String,

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Show statistics for a tenant
    Stats {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,
    },

    /// Audit memory storage and signal quality by tenant/project.
    ///
    /// Reports storage footprint, scope distribution, generated-digest
    /// noise, duplicate health, age buckets, and project-id alias
    /// candidates. Use this before cleanup or retention changes.
    Audit {
        /// Optional tenant identifier. Omit to scan every known tenant.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier filter.
        #[arg(long)]
        project_id: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Exit with code 2 when unreadable_active_chunks > 0.
        #[arg(long)]
        strict: bool,

        /// Output file path (defaults to stdout).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Pagination size for chunk scans.
        #[arg(long, default_value_t = 1000)]
        page_size: usize,

        /// Maximum duplicate examples to request from persistent health snapshots.
        #[arg(long, default_value_t = 5)]
        duplicate_examples: usize,

        /// Maximum project rows to render per tenant.
        #[arg(long, default_value_t = 15)]
        top_projects: usize,
    },

    /// Usefulness & self-diagnosis report from the usage ledger and store metadata.
    Report {
        /// Optional tenant identifier. Omit to scan every known tenant.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier filter.
        #[arg(long)]
        project_id: Option<String>,

        /// Time window: Nd (days) or Nh (hours), e.g. 7d, 24h, 30d.
        #[arg(long, default_value = "7d")]
        since: String,

        /// Output format.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ReportFormat,

        /// Exit with code 2 when any [warn] self-diagnosis line is present.
        #[arg(long)]
        strict: bool,

        /// Max learning-digest entries.
        #[arg(long, default_value_t = 5)]
        top: usize,

        /// Output file path (defaults to stdout).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Warm-worker routing.
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Build a non-destructive cleanup approval plan.
    ///
    /// Classifies tenants/projects for archive review, hidden-row purge
    /// readiness, high generated-digest noise, and missing project scope.
    /// Emits command previews only; it never deletes or rewrites data.
    CleanupPlan {
        /// Optional tenant identifier. Omit to scan every known tenant.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Optional project identifier filter.
        #[arg(long)]
        project_id: Option<String>,

        /// Project directory used for generated verification commands.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Output format.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Output file path (defaults to stdout).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Archive directory shown in generated command previews.
        #[arg(long, default_value = "tasks/memd-cleanup-archive")]
        archive_dir: PathBuf,

        /// Only suggest hard-purge commands for hidden rows older than this grace period.
        #[arg(long, default_value_t = 30)]
        older_than_days: u64,

        /// Maximum hidden-row purge candidates to inspect per tenant.
        #[arg(long, default_value_t = 1000)]
        candidate_limit: usize,

        /// Pagination size for active chunk scans.
        #[arg(long, default_value_t = 1000)]
        page_size: usize,

        /// Maximum project rows to render per tenant.
        #[arg(long, default_value_t = 15)]
        top_projects: usize,
    },

    /// Archive and hard-purge old hidden chunks from metadata and indexes.
    ///
    /// Defaults to dry-run. Destructive cleanup requires `--apply` and
    /// `--archive`; eligible rows are soft-deleted and index-pruned before
    /// metadata rows are physically removed.
    Purge {
        /// Tenant identifier.
        #[arg(long)]
        tenant_id: String,

        /// Optional project identifier filter.
        #[arg(long)]
        project_id: Option<String>,

        /// Only purge hidden rows older than this grace period.
        #[arg(long, default_value_t = 30)]
        older_than_days: u64,

        /// Maximum candidate rows to process in this run.
        #[arg(long, default_value_t = 1000)]
        limit: usize,

        /// Also include live metadata rows whose segment payload cannot be read.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        include_unreadable_active: bool,

        /// Archive path written before any destructive `--apply` mutation.
        #[arg(long)]
        archive: Option<PathBuf>,

        /// Apply the purge. Omit for dry-run.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        apply: bool,

        /// Run SQLite metadata VACUUM after metadata rows are removed.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        vacuum_metadata: bool,

        /// Rewrite finalized segment files after metadata purge to reclaim tombstoned payload bytes.
        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        rewrite_segments: bool,

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Inspect and verify a purge archive without writing to the store.
    ///
    /// Use this after `memd purge --apply --archive <path>` writes an
    /// archive and before trusting any destructive cleanup result.
    PurgeArchive {
        /// Archive path written by `memd purge --archive`.
        #[arg(long)]
        archive: PathBuf,

        /// Optional tenant id that the archive must contain.
        #[arg(long)]
        expect_tenant_id: Option<String>,

        /// Optional project id that the archive must contain.
        #[arg(long)]
        expect_project_id: Option<String>,

        /// Optional minimum number of records required in the archive.
        #[arg(long)]
        min_records: Option<usize>,

        /// Output format.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Output file path (defaults to stdout).
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Export all tenant chunks in a human-readable or machine-readable format
    Export {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: Option<String>,

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
        tenant_id: Option<String>,

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
        tenant_id: Option<String>,

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
        tenant_id: Option<String>,

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

        /// Route this write through the local warm worker (auto starts one if needed; off runs locally and requires the writer lock; required fails when no worker is reachable)
        #[arg(long, value_enum, default_value = "auto")]
        warm: WarmMode,
    },

    /// Initialize memd CLI guardrails for agent workflows
    Init {
        /// Tenant identifier to enforce in generated policies
        #[arg(long)]
        tenant_id: String,

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

    /// Diagnose host wiring and per-repo scope.
    ///
    /// Reports the state of: `memd` binary discovery, data directory,
    /// global agent rules (Claude / Codex / Cursor), Claude
    /// `SessionStart` hook, and the current project's `.memd`
    /// scope. By default this is informational and exits 0; use
    /// `--strict` to fail when any doctor check fails.
    Doctor {
        /// Project directory to inspect for `.memd/project_scope.json`.
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Output format. `markdown` (default) renders a human-readable
        /// report with stable `[ok]`/`[--]` prefixes; `json` emits the
        /// raw structured report.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,

        /// Exit with code 2 when any doctor check fails.
        #[arg(long)]
        strict: bool,
    },

    /// Disk hygiene: sweep orphan HNSW snapshots and, with --aggressive,
    /// force-merge the global sparse index. Takes the data-directory writer
    /// lock. Output uses key:value lines for shell parsing.
    Maintenance {
        /// Data directory (defaults to the top-level --data-dir or
        /// ~/.memd/data).
        #[arg(long)]
        data_dir: Option<PathBuf>,

        /// Restrict the HNSW orphan sweep to one tenant directory. The
        /// aggressive sparse-index merge remains global.
        #[arg(long)]
        tenant_id: Option<String>,

        /// Report what would change without modifying disk.
        #[arg(long)]
        dry_run: bool,

        /// Sweep orphans and force-merge the global sparse index. This can
        /// temporarily require space for both the old and merged segments.
        #[arg(long)]
        aggressive: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreAccess {
    ReadOnly,
    Writer,
}

impl CliCommand {
    /// Fold positional aliases into their long-option fields so every
    /// downstream consumer sees a single canonical source. Clap cannot make
    /// one argument both positional and `--long`, so the positional arrives
    /// in a separate field; the two are mutually exclusive at parse time.
    pub fn normalize_positional_aliases(&mut self) {
        match self {
            CliCommand::Search {
                query: value @ None,
                query_positional: positional,
                ..
            }
            | CliCommand::Get {
                chunk_id: value @ None,
                chunk_id_positional: positional,
                ..
            } => {
                *value = positional.take();
            }
            _ => {}
        }
    }

    pub fn store_access(&self) -> StoreAccess {
        match self {
            CliCommand::Add { .. } => StoreAccess::Writer,
            CliCommand::Search { .. } => StoreAccess::ReadOnly,
            CliCommand::AgentContext { .. } => StoreAccess::ReadOnly,
            CliCommand::MemoryMd { .. } => StoreAccess::ReadOnly,
            CliCommand::EvalMemoryMd { .. } => StoreAccess::ReadOnly,
            CliCommand::EvalRetrieval { .. } => StoreAccess::ReadOnly,
            // Opens and mutates an isolated scratch PersistentStore.
            CliCommand::EvalWriteQuality { .. } => StoreAccess::Writer,
            CliCommand::Consolidate { .. } => StoreAccess::Writer,
            CliCommand::ConsolidateReview { .. } => StoreAccess::Writer,
            CliCommand::Outcome { .. } => StoreAccess::Writer,
            CliCommand::OutcomeScan { .. } => StoreAccess::Writer,
            CliCommand::EvalCounterfactual { .. } => StoreAccess::Writer,
            CliCommand::EvalOutcomeRanking { .. } => StoreAccess::Writer,
            CliCommand::SessionStart { .. } => StoreAccess::ReadOnly,
            CliCommand::Call { .. } => StoreAccess::Writer,
            CliCommand::Batch { .. } => StoreAccess::Writer,
            CliCommand::Warm { .. } => StoreAccess::Writer,
            CliCommand::WarmWorker { .. } => StoreAccess::Writer,
            CliCommand::Get { .. } => StoreAccess::ReadOnly,
            CliCommand::Delete { .. } => StoreAccess::Writer,
            CliCommand::Stats { .. } => StoreAccess::ReadOnly,
            CliCommand::Audit { .. } => StoreAccess::ReadOnly,
            CliCommand::Report { .. } => StoreAccess::ReadOnly,
            CliCommand::CleanupPlan { .. } => StoreAccess::ReadOnly,
            CliCommand::Purge { .. } => StoreAccess::Writer,
            CliCommand::PurgeArchive { .. } => StoreAccess::ReadOnly,
            CliCommand::Export { .. } => StoreAccess::ReadOnly,
            CliCommand::ExportMarkdown { .. } => StoreAccess::ReadOnly,
            CliCommand::ExportOmf { .. } => StoreAccess::ReadOnly,
            CliCommand::ImportOmf { .. } => StoreAccess::Writer,
            CliCommand::Init { .. } => StoreAccess::ReadOnly,
            CliCommand::Doctor { .. } => StoreAccess::ReadOnly,
            CliCommand::Maintenance { .. } => StoreAccess::Writer,
        }
    }

    /// Whether this command needs an initialized backing store.
    pub fn requires_store(&self) -> bool {
        !matches!(
            self,
            CliCommand::Init { .. } | CliCommand::Warm { .. } | CliCommand::Maintenance { .. }
        )
    }

    /// Warm mode for commands that can be served by the local warm worker.
    pub fn warm_mode(&self) -> Option<WarmMode> {
        match self {
            CliCommand::Search { warm, .. }
            | CliCommand::AgentContext { warm, .. }
            | CliCommand::Consolidate { warm, .. }
            | CliCommand::Outcome { warm, .. }
            | CliCommand::Call { warm, .. }
            | CliCommand::Batch { warm, .. }
            | CliCommand::Add { warm, .. }
            | CliCommand::Delete { warm, .. }
            | CliCommand::Purge { warm, .. }
            | CliCommand::Report { warm, .. }
            | CliCommand::ImportOmf { warm, .. } => Some(*warm),
            _ => None,
        }
    }
}

/// Parse chunk type from string
pub(super) fn parse_chunk_type(s: &str) -> std::result::Result<ChunkType, String> {
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
