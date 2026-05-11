use std::path::Path;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tracing::warn;

use crate::error::{MemdError, Result};
use crate::maintenance::DreamParams;
use crate::mcp::handlers::{
    handle_artifact_create, handle_artifact_find_decisions, handle_artifact_find_evidence,
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
    handle_task_run_start, handle_task_search, handle_task_start, AddBatchParams, AddParams,
    ArtifactCreateParams, ArtifactGetParams, ArtifactLibraryParams, ArtifactListThreadParams,
    ArtifactVerifyParams, CompactParams, ConsolidateEpisodeParams,
    ContextFindRelevantContextParams, ContextGetFilesForSubsystemParams,
    ContextGetHotContextParams, ContextListSubsystemsParams, ContextSearchDocumentsParams,
    ContextSuggestAgentParams, DeleteParams, ExportMarkdownParams, ExportOmfParams, FeedbackParams,
    FindCallersParams, FindDefinitionParams, FindErrorsParams, FindImportsParams,
    FindNearDuplicatesParams, FindReferencesParams, FindToolCallsParams, GetParams, HealthParams,
    ImportOmfParams, MetricsParams, PreviewOmfImportParams, ProjectBriefParams, SearchParams,
    SetExpiryParams, StatsParams, SupersedeParams, TaskAddEvidenceParams, TaskFinishParams,
    TaskGetParams, TaskProgressParams, TaskResumeParams, TaskRunFinishParams, TaskRunStartParams,
    TaskSearchParams, TaskStartParams,
};
use crate::mcp::McpError;
use crate::metrics::MetricsCollector;
use crate::store::{Store, TenantManager};
use crate::structural::{
    CallGraphIndexer, CallGraphSymbolRecord, StructuralStore, SymbolIndexer, SymbolQueryService,
    TraceQueryService,
};
use crate::types::TenantId;

pub(super) fn parse_call_arguments(json_arg: Option<&str>, input: Option<&Path>) -> Result<Value> {
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

pub(super) async fn cli_call_tool<S: Store>(
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
            let (response, events) =
                handle_memory_import_omf(store, tenant_manager, params).await?;
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
