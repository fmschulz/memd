# Scientific Knowledge Artifact Schema

This directory documents the task-oriented knowledge artifact schema used by `memd`.

## Purpose

The task schema exists to make agent reporting consistent across sessions and across different agents using the same tenant.

The system is designed so later agents can reliably recover:

- task goal
- motivation
- hypothesis
- scientific or technical question
- tool choice
- parameters
- inputs and outputs
- what worked
- what failed
- evidence
- validation
- uncertainty
- follow-up actions

That consistency is the main reason the schema exists. It is not just a richer note format.

## Canonical Artifact Envelope

The source of truth is the canonical task artifact envelope implemented in:

- [task_memory/mod.rs](../../../crates/memd/src/task_memory/mod.rs)

Current artifact kinds:

- `task_start`
- `task_progress`
- `run_start`
- `run_finish`
- `evidence`
- `review`
- `revision`
- `verification`
- `decision`
- `digest`
- `task_finish`

Important canonical fields include:

- `artifact_id`
- `artifact_kind`
- `task_id`
- `parent_task_id`
- `tenant_id`
- `project_id`
- `agent_id`
- `session_id`
- `status`
- `artifact_role`
- `challenge_id`
- `thread_id`
- `reply_to_artifact_id`
- `relation_kind`
- `goal`
- `motivation`
- `hypothesis`
- `scientific_question`
- `summary`
- `blockers`
- `what_worked`
- `what_failed`
- `validation`
- `uncertainty`
- `followups`
- `dataset_refs`
- `entity_refs`
- `related_artifact_ids`
- `contributors`
- `tool_name`
- `tool_version`
- `command`
- `parameters`
- `inputs`
- `outputs`
- `metrics`
- `why_chosen`
- `confidence`
- `requested_action`
- `verification_status`
- `promotion_state`
- `digest_key`
- `source_updated_at_ms`
- `compute_budget`
- `cost_actual`
- `data_access_level`
- `policy_tags`
- `allowed_tools`
- `approval_state`
- `provenance`
- `timestamp_created`
- `timestamp_observed`

## Normalized Metadata Tables

The canonical envelope is projected into normalized SQLite side tables in:

- [sqlite.rs](../../../crates/memd/src/store/metadata/sqlite.rs)

Current tables:

- `task_artifacts`
- `tasks`
- `task_events`
- `runs`
- `evidence`
- `datasets`
- `entities`
- `task_datasets`
- `task_entities`
- `artifact_links`
- `artifact_relations`
- `artifact_contributors`
- `challenges`

These tables exist to support exact task-aware filters and joins without coupling retrieval to every schema field.

## Retrieval Projection

Canonical artifacts are projected into ordinary retrieval chunks. The projection layer lives in:

- [task_memory/mod.rs](../../../crates/memd/src/task_memory/mod.rs)

Current projection kinds:

- `task_goal`
- `task_summary`
- `run`
- `evidence`
- `decision`
- `digest`
- `worked`
- `failed`
- `validation`

The projection layer is intentionally separate from the canonical envelope:

- canonical artifact = source of truth
- retrieval chunk = search-optimized derived text

## Summary-First Digests

Persisted digest artifacts currently use these `artifact_role` values:

- `project_brief`
- `task_resume`
- `failure_library`
- `decision_library`
- `evidence_library`
- `highlight_library`

These digests are stored as canonical `digest` artifacts and projected into ordinary retrieval chunks instead of being kept in a separate store.

The current implementation also tracks:

- `promotion_state` with values `raw`, `summarized`, `canonical`, and `verified`
- stable digest identities keyed by role and scope
- `source_updated_at_ms` so regenerated digests can reflect source freshness

`task.resume` reuses the real `task_id` for its digest so `resume_task` retrieval stays aligned with canonical task filters.

## Trust Boundary

`memd` now exposes an explicit trust vocabulary at the MCP boundary:

- `semantic_candidate`: retrieved by similarity without canonical artifact grounding
- `canonical_record`: linked to a non-digest canonical artifact
- `compiled_digest_hint`: linked to a digest artifact that still requires re-grounding
- `verified_record`: linked to an explicit verification artifact or other verified record

The intended workflow is:

1. use semantic search or digest helpers to generate candidates
2. use `artifact.verify` to ground a claim against canonical artifacts
3. trust the supporting canonical artifact IDs, not digest text on its own

## Exact Filters

`task.search` currently supports exact filters over the normalized side tables for:

- `task_id`
- `artifact_kind`
- `status`
- `challenge_id`
- `thread_id`
- `reply_to_artifact_id`
- `artifact_role`
- `dataset_name`
- `dataset_version`
- `entity_name`
- `entity_type`
- `tool_name`
- `project_id`
- `agent_id`
- `session_id`
- `requested_action`
- `verification_status`
- `relation_kind`

These filters are resolved first, then the candidate set is reranked for retrieval.

`memory.search`, `task.search`, and `artifact.search` also accept `mode` with `generic`, `brief_project`, `resume_task`, `find_failures`, `find_decisions`, `find_evidence`, and `find_highlights` to bias candidate planning toward the corresponding digests and canonical summaries.

`memory.search` and `artifact.search` support compact response shaping with `compact: true` or `token_budget`. Compact output uses the response packer after ranking and visibility filtering, preserves identifiers and trust/grounding metadata, and can omit large `text`, `matched_text`, and full `artifact` fields so callers can fetch only selected records.

`memory.compact` can explicitly refresh project brief and failure/decision/evidence/highlight library digests through `project_id`, `digest_modes`, and `force_digest_rebuild`.

`memory.dream` plans tenant/project-scoped retention and compaction work. It defaults to `dry_run: true`; safe apply mode retires duplicate digest projections through lifecycle metadata, can refresh digests, and records a traceable `dream_report` artifact. Exact duplicate raw chunks and non-digest task artifacts are reported by health but remain report-only under the current safe strategy. Append-only segment rewrite is reported as blocked until recovery-safe physical rewrite support is implemented.

`memory.stats` uses aggregate counts and reports `active_chunks`, `deleted_chunks`, `total_chunks`, and active/deleted/all chunk-type maps. `memory.health` adds a read-only tenant/project report for duplicate canonical text, index coverage, payload-size percentiles, recent latency tails, and explicit alias scope information. `include_examples` controls whether duplicate previews are returned; `duplicate_limit` limits only those previews, not the aggregate duplicate group, row, or byte counts.

`context.find_relevant_context` can prepend hot-context chunks, but the hot pre-scan is wall-clock bounded so broad lookups on large tenants still continue through normal retrieval. List-style retrieval scans skip stale unreadable segment rows with a warning; strict point reads through `memory.get` still surface storage errors.

## Conversation Event Tags

Raw conversational chunks can use ordinary tags for event binding without a
schema migration:

- `event:<event_id>` groups factual and relational entries from one observed event.
- `entry:factual`, `entry:relational`, and `entry:synthesis` distinguish raw facts, relations, and derived summaries.
- `speaker:<id>` and `turn:<n>` are optional caller-owned labels.

`memory.search` keeps default ranking behavior unchanged. When callers pass
`expand_event_siblings: true`, each ranked hit that has an `event:<id>` tag can
include an `expanded_siblings` array containing bounded same-tenant/same-project
chunks with the same event tag. These siblings are context for the hit, not
additional ranked hits.

## Durability

Task artifacts are WAL-backed. The relevant implementation is in:

- [format.rs](../../../crates/memd/src/store/wal/format.rs)
- [writer.rs](../../../crates/memd/src/store/wal/writer.rs)
- [reader.rs](../../../crates/memd/src/store/wal/reader.rs)
- [persistent.rs](../../../crates/memd/src/store/persistent.rs)

This means canonical task side tables can be rebuilt during recovery, rather than depending on best-effort metadata writes.

## Agent Guardrails

Agents should follow this contract:

1. Search first.
2. Use `task.start` before substantive work.
3. Use `task.progress` only for meaningful checkpoints.
4. Use `task.run_start` / `task.run_finish` around substantive runs.
5. Use `task.add_evidence` when a concrete result matters.
6. Use `task.finish` to record worked/failed/validation/uncertainty/followups.
7. Use `artifact.create` when the important event is critique, revision, verification, or thread-level coordination rather than a task lifecycle step.
8. Use compact `memory.search` / `artifact.search` first for broad retrieval, then `memory.get` or `artifact.get` for selected full records.
9. Use `memory.dream` in dry-run mode before applying retention or compaction actions.
10. Use `artifact.search` / `artifact.list_thread` when the artifact itself is the unit of exchange.

This is how `memd` enforces consistent reporting across agents in the same tenant. For one trusted machine or trust domain, agents should prefer a stable shared `tenant_id` and use `project_id`, `thread_id`, and `task_id` for narrower scopes. Cross-tenant project recovery is explicit: configure same-project `server.project_aliases` for known historical scopes and inspect `scope_expansion` plus per-hit `origin` metadata in search responses.

## Documentation Status

This README documents the implemented schema at a high level. It does not yet provide:

- a formal external schema spec file
- migration version history
- generated schema diagrams

Those can be added later if needed, but the current behavior is documented here and in the source files above.
