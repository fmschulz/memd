# Scientific Task Memory Schema

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
- `tool_name`
- `tool_version`
- `command`
- `parameters`
- `inputs`
- `outputs`
- `metrics`
- `why_chosen`
- `confidence`
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

These tables exist to support exact task-aware filters and joins without coupling retrieval to every schema field.

## Retrieval Projection

Canonical artifacts are projected into ordinary retrieval chunks. The projection layer lives in:

- [task_memory/mod.rs](../../../crates/memd/src/task_memory/mod.rs)

Current projection kinds:

- `task_goal`
- `task_summary`
- `run`
- `evidence`
- `worked`
- `failed`
- `validation`

The projection layer is intentionally separate from the canonical envelope:

- canonical artifact = source of truth
- retrieval chunk = search-optimized derived text

## Exact Filters

`task.search` currently supports exact filters over the normalized side tables for:

- `task_id`
- `artifact_kind`
- `status`
- `dataset_name`
- `dataset_version`
- `entity_name`
- `entity_type`
- `tool_name`
- `project_id`
- `agent_id`
- `session_id`

These filters are resolved first, then the candidate set is reranked for retrieval.

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

This is how `memd` enforces consistent reporting across agents in the same tenant.

## Documentation Status

This README documents the implemented schema at a high level. It does not yet provide:

- a formal external schema spec file
- migration version history
- generated schema diagrams

Those can be added later if needed, but the current behavior is documented here and in the source files above.
