//! CLI mode for direct operation invocation
//!
//! Provides command-line interface for manual testing and debugging
//! through the local executable.

use serde_json::json;
use tracing::info;

use crate::error::{MemdError, Result};
use crate::store::metadata::MetadataStore;
use crate::store::writer_lock::acquire_writer_lock;
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, TenantId};

mod args;
mod audit;
mod batch;
mod call;
mod cleanup_plan;
mod consolidate;
mod doctor;
mod eval_counterfactual;
mod eval_outcome_ranking;
mod eval_retrieval;
mod eval_write_quality;
mod maintenance;
mod memory_md;
mod ops_bridge;
mod paths;
mod purge;
mod read_commands;
mod render;
mod report;
mod scope;
mod search;
mod session_start;
mod warm;
mod write_commands;

pub use args::{
    CliCommand, CliQueryMode, ExportFormat, ReportFormat, SearchReranker, StoreAccess, WarmCommand,
    WarmMode, WarmProcessConfig,
};
use args::{ProjectScopeConfig, SearchRerankerOptions, TenantScopeConfig};
use audit::{render_audit_report, run_audit, strict_should_fail, AuditOptions};
use batch::{read_batch_input, run_batch_jsonl, stream_batch_jsonl};
use call::parse_call_arguments;
use cleanup_plan::{render_cleanup_plan, run_cleanup_plan, CleanupPlanOptions};
use consolidate::{run_consolidate, run_consolidate_review, ConsolidateOptions};
use doctor::{failing_checks, run_doctor, DoctorOptions};
use eval_counterfactual::{run_eval_counterfactual, EvalCounterfactualOptions};
use eval_outcome_ranking::{run_eval_outcome_ranking, EvalOutcomeRankingOptions};
use eval_retrieval::{run_eval_retrieval, EvalRetrievalOptions};
use eval_write_quality::{run_eval_write_quality, EvalWriteQualityOptions};
use memory_md::{
    refresh_memory_md_with_health, run_memory_md_eval, MemoryMdEvalOptions, MemoryMdOptions,
};
use ops_bridge::cli_call_tool;
use paths::{
    absolutize_project_dir, normalize_absolute, path_is_inside, read_omf_input,
    read_stdin_to_string, reject_if_any_symlink_inside_outdir, resolve_data_dir,
    resolve_export_markdown_data_dirs,
};
use purge::{
    inspect_purge_archive, render_purge_archive_inspection, run_purge, PurgeArchiveInspectOptions,
    PurgeOptions,
};
use read_commands::collect_all_chunks;
use render::{
    render_agent_context, render_export, render_guardrail_block, render_search_payload,
    unwrap_content_payload, upsert_guardrail_file, write_cli_log, write_rendered,
};
use report::{cli_report_rendered, ReportOptions};
pub use scope::resolve_command_scope;
use search::{
    apply_search_reranker, cli_agent_context_payload, cli_search_payload, export_format_name,
    finalize_search_episode,
};
use session_start::{run_session_start, SessionStartOptions};
use warm::run_warm_worker;
pub use warm::{run_warm_admin, try_run_warm_client, warm_socket_path};
use write_commands::{
    cli_add_rendered, cli_delete_rendered, cli_import_omf_rendered, CliAddRenderOptions,
};

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
            warm: _,
        } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
            let rendered = cli_add_rendered(
                store,
                tenant_manager,
                CliAddRenderOptions {
                    tenant_id,
                    text,
                    chunk_type,
                    project_id,
                    tags,
                    source_uri,
                    source_path,
                },
            )
            .await?;
            print!("{rendered}");
        }

        CliCommand::Search {
            tenant_id,
            query,
            k,
            project_id,
            compact,
            dedupe_by_source,
            token_budget,
            mode,
            no_text,
            include_artifact,
            include_superseded,
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
            let tenant_id = scope::require_tenant(tenant_id)?;
            let episode_tenant_id = tenant_id.clone();
            let mut payload = cli_search_payload(
                store,
                tenant_id,
                project_id,
                query.clone(),
                k,
                compact,
                dedupe_by_source,
                token_budget,
                mode,
                no_text,
                include_artifact,
                include_superseded,
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
            finalize_search_episode(store, &episode_tenant_id, &payload).await?;
            write_rendered(output.as_deref(), &render_search_payload(&payload, format)?)?;
        }

        CliCommand::AgentContext {
            tenant_id,
            project_id,
            task_id,
            thread_id,
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
            let tenant_id = scope::require_tenant(tenant_id)?;
            let payload = cli_agent_context_payload(
                store,
                &tenant_id,
                project_id.as_deref(),
                task_id.as_deref(),
                thread_id.as_deref(),
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
            explain_output,
        } => {
            let result = refresh_memory_md_with_health(
                store,
                tenant_manager,
                MemoryMdOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    output,
                    project_limit,
                    global_limit,
                    candidate_k,
                    explain_output,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::EvalMemoryMd {
            tenant_id,
            project_id,
            project_dir,
            output,
            project_limit,
            candidate_k,
            top_n,
            min_useful_ratio,
            max_generated_wrappers,
            agent_usefulness,
            gold_file,
        } => {
            let result = run_memory_md_eval(
                store,
                tenant_manager,
                MemoryMdEvalOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    output,
                    project_limit,
                    candidate_k,
                    top_n,
                    min_useful_ratio,
                    max_generated_wrappers,
                    agent_usefulness,
                    gold_file,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::EvalRetrieval {
            tenant_id,
            project_id,
            project_dir,
            queries,
            k,
            min_precision_at_k,
            min_hit_rate_at_k,
            min_known_recall_at_k,
            min_mrr,
        } => {
            let result = run_eval_retrieval(
                store,
                EvalRetrievalOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    queries_path: queries,
                    k,
                    min_precision_at_k,
                    min_hit_rate_at_k,
                    min_known_recall_at_k,
                    min_mrr,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::EvalWriteQuality {
            project_dir,
            min_rejection_or_downgrade_rate,
            min_duplicate_reuse_rate,
            max_total_chunks,
            max_disk_bytes,
            require_retention_compaction,
        } => {
            let result = run_eval_write_quality(EvalWriteQualityOptions {
                project_dir,
                min_rejection_or_downgrade_rate,
                min_duplicate_reuse_rate,
                max_total_chunks,
                max_disk_bytes,
                require_retention_compaction,
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::Consolidate {
            tenant_id,
            project_id,
            project_dir,
            max_region,
            dry_run,
            background,
            force,
            promote,
            legacy_immediate,
            warm: _,
        } => {
            let result = run_consolidate(
                store,
                ConsolidateOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    max_region,
                    dry_run,
                    background,
                    force,
                    promote,
                    legacy_immediate,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::ConsolidateReview {
            run_id,
            list,
            limit,
            accept,
            reject,
        } => {
            let result =
                run_consolidate_review(store, run_id.as_deref(), list, limit, accept, reject)
                    .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::Outcome {
            episode_id,
            tenant_id,
            outcome,
            verifier,
            used,
            harmful,
            evidence,
            event_time_ms,
            warm: _,
        } => {
            let value = cli_call_tool(
                store,
                tenant_manager,
                "memory.record_outcome",
                json!({
                    "tenant_id": scope::require_tenant(tenant_id)?,
                    "episode_id": episode_id,
                    "outcome": outcome,
                    "verifier_type": verifier,
                    "used_chunk_ids": used,
                    "harmful_chunk_ids": harmful,
                    "evidence_reference": evidence,
                    "event_time_ms": event_time_ms,
                }),
            )
            .await
            .map_err(|error| MemdError::ProtocolError(error.to_string()))?;
            let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }

        CliCommand::EvalCounterfactual {
            tenant_id,
            project_id,
            project_dir,
            queries,
            k,
        } => {
            let result = run_eval_counterfactual(
                store,
                EvalCounterfactualOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    queries_path: queries,
                    k,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::EvalOutcomeRanking {
            tenant_id,
            project_id,
            project_dir,
            queries,
            k,
            report_json,
        } => {
            let result = run_eval_outcome_ranking(
                store,
                EvalOutcomeRankingOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    queries_path: queries,
                    k,
                    report_json,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::SessionStart { project_dir } => {
            let result = run_session_start(store, SessionStartOptions { project_dir }).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::Doctor {
            project_dir,
            format,
            strict,
        } => {
            let report = run_doctor(
                store,
                DoctorOptions {
                    project_dir,
                    // The resolved global --data-dir: doctor must
                    // diagnose the store this process actually uses.
                    data_dir: tenant_manager.map(|tm| tm.data_dir().to_path_buf()),
                    format,
                },
            )
            .await?;
            if strict && !failing_checks(&report).is_empty() {
                std::process::exit(2);
            }
        }

        CliCommand::Call {
            tool,
            json,
            input,
            output,
            warm: _,
        } => {
            let arguments = parse_call_arguments(json.as_deref(), input.as_deref())?;
            let arguments = scope::apply_operation_scope(
                arguments,
                &mut scope::OperationScopeCache::default(),
            )?;
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
            warm: _,
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

        CliCommand::WarmWorker {
            socket,
            embedding_model,
            search_variant,
        } => {
            run_warm_worker(
                store,
                tenant_manager,
                &socket,
                embedding_model.as_deref(),
                search_variant.as_deref(),
            )
            .await?;
        }

        CliCommand::Maintenance {
            data_dir,
            tenant_id,
            dry_run,
            aggressive,
        } => {
            // Resolution order (Codex Phase 5 HIGH): explicit subcommand
            // --data-dir wins, then the top-level --data-dir / config
            // (already resolved into tenant_manager), then the default
            // discovery. Without this chain, `memd --data-dir /x
            // maintenance` would silently operate on $HOME/.memd/data.
            let data_dir = match data_dir {
                Some(p) => p,
                None => match tenant_manager {
                    Some(tm) => tm.data_dir().to_path_buf(),
                    None => resolve_data_dir(None)?,
                },
            };
            std::fs::create_dir_all(&data_dir)?;
            let _writer_lock = acquire_writer_lock(&data_dir)?;
            let report = maintenance::run(&data_dir, tenant_id.as_deref(), dry_run, aggressive)?;
            let rendered = maintenance::render_report(&report, dry_run, aggressive);
            print!("{}", rendered);
        }

        CliCommand::Get {
            tenant_id,
            chunk_id,
        } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
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
            warm: _,
        } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
            let rendered = cli_delete_rendered(store, &tenant_id, &chunk_id).await?;
            print!("{rendered}");
        }

        CliCommand::Stats { tenant_id } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
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

        CliCommand::Audit {
            tenant_id,
            project_id,
            format,
            strict,
            output,
            page_size,
            duplicate_examples,
            top_projects,
        } => {
            let report = run_audit(
                store,
                tenant_manager,
                AuditOptions {
                    tenant_id,
                    project_id,
                    page_size,
                    duplicate_examples,
                    top_projects,
                },
            )
            .await?;
            write_rendered(output.as_deref(), &render_audit_report(&report, format)?)?;
            if strict && strict_should_fail(&report) {
                std::process::exit(2);
            }
        }

        CliCommand::Report {
            tenant_id,
            project_id,
            since,
            format,
            strict,
            top,
            output,
            warm: _,
        } => {
            let (rendered, warn_count) = cli_report_rendered(
                store,
                tenant_manager,
                ReportOptions {
                    tenant_id,
                    project_id,
                    since,
                    top,
                    format,
                    served_via_worker: false,
                },
            )
            .await?;
            write_rendered(output.as_deref(), &rendered)?;
            if strict && warn_count > 0 {
                std::process::exit(2);
            }
        }

        CliCommand::CleanupPlan {
            tenant_id,
            project_id,
            project_dir,
            format,
            output,
            archive_dir,
            older_than_days,
            candidate_limit,
            page_size,
            top_projects,
        } => {
            let report = run_cleanup_plan(
                store,
                tenant_manager,
                CleanupPlanOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    archive_dir,
                    older_than_days,
                    candidate_limit,
                    page_size,
                    top_projects,
                },
            )
            .await?;
            write_rendered(output.as_deref(), &render_cleanup_plan(&report, format)?)?;
        }

        CliCommand::Purge {
            tenant_id,
            project_id,
            older_than_days,
            limit,
            include_unreadable_active,
            archive,
            apply,
            vacuum_metadata,
            rewrite_segments,
            warm: _,
        } => {
            let result = run_purge(
                store,
                PurgeOptions {
                    tenant_id,
                    project_id,
                    older_than_days,
                    limit,
                    include_unreadable_active,
                    archive,
                    apply,
                    vacuum_metadata,
                    rewrite_segments,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        CliCommand::PurgeArchive {
            archive,
            expect_tenant_id,
            expect_project_id,
            min_records,
            format,
            output,
        } => {
            let report = inspect_purge_archive(PurgeArchiveInspectOptions {
                archive,
                expect_tenant_id,
                expect_project_id,
                min_records,
            })?;
            write_rendered(
                output.as_deref(),
                &render_purge_archive_inspection(&report, format)?,
            )?;
        }

        CliCommand::Export {
            tenant_id,
            format,
            output,
            page_size,
        } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
            let tenant = TenantId::new(&tenant_id)?;
            let page_size = page_size.clamp(1, 10_000);
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
            let tenant_id = scope::require_tenant(tenant_id)?;
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

            let files = crate::markdown_export::render_markdown_tree(&chunks);
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
            let tenant_id = scope::require_tenant(tenant_id)?;
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
            warm: _,
        } => {
            let tenant_id = scope::require_tenant(tenant_id)?;
            let raw = read_omf_input(input.as_deref())?;
            let rendered = cli_import_omf_rendered(
                store,
                tenant_manager,
                &tenant_id,
                &raw,
                include_archived,
                fuzzy_threshold,
                dry_run,
            )
            .await?;
            print!("{rendered}");
        }

        CliCommand::Init {
            tenant_id,
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
            // Always persist data_dir so `memd export-markdown` (and any
            // future CLI tool that needs the containment guard) can
            // auto-discover the daemon's data directory from a
            // nearest-ancestor `.memd/tenant_scope.json`.
            let scope_config = TenantScopeConfig {
                primary_tenant: tenant.to_string(),
                write_tenant: tenant.to_string(),
                data_dir: Some(effective_data_dir.display().to_string()),
            };
            let guardrail_block = render_guardrail_block(&scope_config, &memd_command);

            let guardrail_path = memd_dir.join("memory_guardrails.md");
            let tenant_scope_path = memd_dir.join("tenant_scope.json");
            let project_scope_path = memd_dir.join("project_scope.json");
            let project_scope = ProjectScopeConfig {
                tenant_id: tenant.to_string(),
                project_id,
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

#[cfg(test)]
mod tests;
