//! Local operation handlers and compatibility internals.
//!
//! The agent-facing integration path is the `memd` CLI. This module keeps
//! the shared operation handlers used by `memd call`, `memd batch`, and the
//! direct CLI commands.

pub mod dedup;
pub mod digest_sweeper;
pub mod error;
pub mod handlers;
pub mod markdown_export;
pub mod post_write_hooks;

pub use crate::ops::{
    configure_operation_routing, handle_artifact_create, handle_artifact_find_decisions,
    handle_artifact_find_evidence, handle_artifact_find_failures, handle_artifact_find_highlights,
    handle_artifact_get, handle_artifact_list_thread, handle_artifact_search,
    handle_artifact_verify, handle_context_brief_project, handle_context_find_relevant_context,
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
    handle_task_run_start, handle_task_search, handle_task_start, AddBatchParams, AddBatchResult,
    AddParams, AddResult, AgentSuggestion, ArtifactCreateParams, ArtifactGetParams,
    ArtifactGetResult, ArtifactLibraryParams, ArtifactListThreadParams, ArtifactSearchHit,
    ArtifactSearchResult, ArtifactThreadResult, ArtifactVerifyParams, ArtifactVerifyResult,
    BatchChunkParams, BudgetInfo, CacheStatsResult, ChunkResult, ChunkTypeHealthResult,
    CitationResult, CompactParams, CompactionStatsResult, ConsolidateEpisodeParams,
    ConsolidateEpisodeResult, ContextFindRelevantContextParams, ContextFindRelevantContextResult,
    ContextGetFilesForSubsystemParams, ContextGetFilesForSubsystemResult,
    ContextGetHotContextParams, ContextGetHotContextResult, ContextListSubsystemsParams,
    ContextListSubsystemsResult, ContextSearchDocumentsParams, ContextSearchDocumentsResult,
    ContextSuggestAgentParams, ContextSuggestAgentResult, DecisionSearchViewResult, DedupConfig,
    DedupSpec, DeleteParams, DeleteResult, DiskStatsResult, ErrorResultResponse,
    EvidenceSearchViewResult, ExportMarkdownParams, ExportOmfParams, FeedbackParams,
    FeedbackResult, FindCallersParams, FindCallersResult, FindDefinitionParams,
    FindDefinitionResult, FindErrorsParams, FindErrorsResult, FindImportsParams, FindImportsResult,
    FindNearDuplicatesParams, FindReferencesParams, FindReferencesResult, FindToolCallsParams,
    FindToolCallsResult, GroundingRef, GroundingStatus, HealthParams, HealthScopeResult,
    HighlightSearchViewResult, HotTierStatsResult, ImportInfoResult, ImportOmfParams,
    LatencyHealthResult, MemoryHealthResult, MetricsParams, OriginScope, PreviewOmfImportParams,
    ProjectBriefParams, ProjectBriefResult, QueryMode, RepairInfo, ScopeExpansion, SearchFilters,
    SearchParams, SearchResult, SetExpiryParams, SourceParams, SourceResult, StatsParams,
    StatsResult, SubsystemSummary, SupersedeParams, SymbolLocationResult, TaskAddEvidenceParams,
    TaskArtifactResult, TaskContributorParams, TaskDatasetRefParams, TaskEntityRefParams,
    TaskFinishParams, TaskGetParams, TaskGetResult, TaskProgressParams, TaskProvenanceParams,
    TaskResumeParams, TaskResumeResult, TaskRunFinishParams, TaskRunStartParams,
    TaskSearchFiltersParams, TaskSearchParams, TaskStartParams, TierDebugInfo, TieredMetricsResult,
    TieredStatsResult, TimeRange, VerificationHint,
};
pub use digest_sweeper::{spawn_digest_sweeper, DigestSweeperHandle};
pub use error::McpError;
pub use post_write_hooks::PostWriteEvent;
