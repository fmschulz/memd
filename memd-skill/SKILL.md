---
name: memd
description: Use when coding agents or AI scientists need a shared local MCP knowledge base to preserve cross-session memory, structured task history, and artifact-based collaboration across Codex and Claude.
---

# memd

Shared knowledge artifacts over MCP for coding agents and AI scientists.

Use the same shared local `memd` daemon URL for Codex CLI and Claude Code when you want one machine to host a shared knowledge base across sessions.

For one trusted machine or trust domain, prefer one stable shared `tenant_id` for collaborating agents and use `project_id` for project scoping. If older same-project history exists under another local tenant, configure explicit `server.project_aliases` and inspect `scope_expansion` / per-hit `origin` metadata rather than relying on broad fallback.

## When to Use

Use `memd` when agents need to:

- preserve context across sessions and across different agents
- search what other agents already tried in the same project
- recover goals, motivation, hypotheses, parameters, and evidence instead of just raw notes
- avoid repeating failed approaches
- share progress on long-running engineering or scientific tasks
- exchange critique, revisions, and verification artifacts around the same thread
- index codebases and codified context alongside task artifacts

Bundled binary:

- [bin/linux-x64/memd](bin/linux-x64/memd)

For a stronger default install that upserts stricter `memd` usage rules into
`~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`, run:

- [install_memd_enforcement.sh](install_memd_enforcement.sh)

## Core Idea

`memd` now has three complementary write surfaces:

1. `memory.*`
   Use for raw chunks:
   - codebase indexing
   - codified context
   - ad hoc notes
   - documentation fragments

2. `task.*`
   Use for structured knowledge artifacts:
   - what the task is trying to do
   - why it matters
   - what was tried
   - which tool/parameters were used
   - what worked
   - what failed
   - what evidence supports the conclusion
   - what uncertainty remains

3. `artifact.*`
   Use for artifact-native collaboration:
   - critique and review
   - revisions and counterproposals
   - verification checkpoints
   - thread inspection
   - optional safety metadata

This distinction matters. `memory.add` is flexible. `task.*` captures task lifecycle. `artifact.*` captures the exchange layer around that work.

For summary-first retrieval and onboarding, the current tool surface also persists digest artifacts and exposes dedicated read helpers. Use `context.brief_project`, `task.resume`, `artifact.find_failures`, `artifact.find_decisions`, `artifact.find_evidence`, and `artifact.find_highlights` when you want a project brief, task resume, or project/tenant library instead of only raw search hits. `memory.search`, `task.search`, and `artifact.search` also accept `mode` with `brief_project`, `resume_task`, `find_failures`, `find_decisions`, `find_evidence`, or `find_highlights` to bias retrieval toward those persisted digests.

Trust boundary:

- semantic search and digest helpers produce candidates
- canonical non-digest artifacts are the default anchor for retrieval
- digest artifacts are compiled hints that still require independent review
- `artifact.find_related` is a retrieval helper that surfaces canonical
  artifacts overlapping a claim — it does NOT itself establish trust. A
  hit is only supporting evidence after an independent reviewer (distinct
  `agent_id`) confirms it. The legacy `artifact.verify` alias is
  deprecated and forwards to `artifact.find_related` with a warning.

## Tool Surface

`memd` exposes 55 MCP tools.

### Generic Memory

- `memory.search`
- `memory.add`
- `memory.add_batch`
- `memory.get`
- `memory.delete`
- `memory.feedback`
- `memory.stats`
- `memory.health`
- `memory.metrics`
- `memory.compact`
- `memory.dream`
- `memory.supersede`
- `memory.set_expiry`
- `memory.find_near_duplicates`
- `memory.export_markdown`
- `memory.export_omf`
- `memory.preview_omf_import`
- `memory.import_omf`
- `memory.consolidate_episode`

### Task Knowledge Artifacts

- `task.start`
- `task.progress`
- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`
- `task.get`
- `task.search`
- `task.resume`

### Canonical Artifacts

- `artifact.create`
- `artifact.review`
- `artifact.revision`
- `artifact.decision`
- `artifact.verification`
- `artifact.get`
- `artifact.search`
- `artifact.find_related` (formerly `artifact.verify` — the alias still works but is deprecated)
- `artifact.verify` (deprecated alias for `artifact.find_related`)
- `artifact.find_failures`
- `artifact.find_decisions`
- `artifact.find_evidence`
- `artifact.find_highlights`
- `artifact.list_thread`

### Context

- `context.list_subsystems`
- `context.get_files_for_subsystem`
- `context.search_context_documents`
- `context.find_relevant_context`
- `context.brief_project`
- `context.suggest_agent`
- `context.get_hot_context`

### Structural

- `code.find_definition`
- `code.find_references`
- `code.find_callers`
- `code.find_imports`

### Debug

- `debug.find_tool_calls`
- `debug.find_errors`

## Required Agent Contract

Agents should follow this lifecycle whenever the work is substantive.

### 0. Retrieve before refusal

Before saying any of the following:

- the task is impossible
- the answer is not possible to determine
- the work is blocked on missing context
- you cannot proceed
- you need to ask the user for information that might already exist in shared memory

you MUST consult `memd` first.

Minimum pre-refusal check:

- if `project_id` is known and you need orientation, call `context.brief_project`
- if `task_id` is known and you need task-local history, call `task.resume` or `task.get`
- call at least one search surface appropriate to the question:
  - `artifact.search` when prior artifacts or decisions are likely
  - `task.search` when prior work structure matters
  - `memory.search` when broader context is needed
  - digest helpers such as `artifact.find_failures`, `artifact.find_decisions`, `artifact.find_evidence`, or `artifact.find_highlights` when the task matches those intents

If the question is trust-sensitive, use `artifact.find_related` to surface candidate supporting artifacts before concluding that no record exists — then review them yourself. A retrieval hit is not grounding.

If `memd` returns nothing useful, say that explicitly:

- checked `memd`
- which surface you checked
- that no relevant record was found

If you have not checked `memd`, you are not allowed to conclude impossible, blocked, or unknowable for substantive work.

### 1. Start the task

Use `task.start` before substantive work.

Required concepts:

- `goal`
- `motivation`
- `hypothesis`
- `scientific_question`
- `dataset_refs`
- `expected_outputs`

### 2. Record meaningful checkpoints

Use `task.progress` only when something materially changed.

Required concepts:

- `summary`
- `blockers`
- `failed_attempts`
- `next_step`

Do not log every shell command. Log changes in understanding.

### 3. Record each substantive run

Before a meaningful run:

- `task.run_start`

Required concepts:

- `tool_name`
- `command`
- `why_chosen`
- `parameters`
- `inputs`

After the run:

- `task.run_finish`

Required concepts:

- `status`
- `outputs`
- `metrics`
- `notes`

### 4. Record concrete evidence

Use `task.add_evidence` when a result materially supports or weakens a claim.

Required concepts:

- `summary`
- `evidence_kind`
- `supports_claim`
- metric name/value when available

### 5. Finish the task

Use `task.finish` when the task reaches a meaningful stopping point.

Required concepts:

- `what_worked`
- `what_failed`
- `validation`
- `uncertainty`
- `followups`
- `confidence`

### 6. Retrieve what others did

Use:

- `task.get` to inspect the canonical artifact history for one task
- `task.search` to search across task artifacts with exact filters and linked canonical artifacts
- `task.resume` to generate or refresh a persisted task resume digest
- `artifact.get` to inspect one canonical artifact by `artifact_id`
- `artifact.search` to search canonical artifacts rather than only retrieval chunks
- `artifact.find_related` to surface canonical artifacts overlapping a claim; review the matched artifacts yourself before trusting the claim (the legacy `artifact.verify` alias still works)
- `artifact.find_failures` to retrieve a digest-backed failure library
- `artifact.find_decisions` to retrieve explicit and inferred decisions
- `artifact.find_evidence` to retrieve digest-backed evidence highlights
- `artifact.find_highlights` to retrieve ranked, high-uplift lessons for future agents
- `artifact.list_thread` to inspect the full collaboration thread around an artifact
- `context.brief_project` to generate or refresh a persisted project brief digest
- `memory.search` to search broader raw memory and context

`memory.compact` can also regenerate project brief and failure/decision/evidence/highlight digests explicitly via `project_id`, `digest_modes`, and `force_digest_rebuild`.

Use `memory.dream` first with its default dry run when a project has duplicate digest projections or retention/compaction pressure. Apply mode should stay project-scoped and conservative: it retires duplicate digest projections through lifecycle metadata, can refresh digests, and emits a `dream_report` artifact for traceability. Exact duplicate raw chunks and non-digest artifacts are health findings, not automatic safe-cleanup targets. Segment rewrite requests are expected to report blocked until recovery-safe rewrite support lands.

### 7. Record critique and verification explicitly

Use `artifact.create` when the important event is not a lifecycle checkpoint but an exchange artifact:

- critique
- revision
- verification
- challenge/thread coordination
- human guidance injected into the shared record

Useful fields:

- `artifact_kind`
- `artifact_role`
- `challenge_id`
- `thread_id`
- `reply_to_artifact_id`
- `relation_kind`
- `contributors`
- `requested_action`
- `verification_status`

Optional safety metadata:

- `compute_budget`
- `cost_actual`
- `data_access_level`
- `policy_tags`
- `allowed_tools`
- `approval_state`

Those safety fields are optional in the current local prototype.

## Guardrails for Agents

These are the operating rules this skill expects:

- Always search before starting work.
- Do not repeat known failed approaches unless you have a reason.
- Use the same `tenant_id` when agents should share knowledge.
- Prefer one stable shared tenant per trusted machine or trust domain and use `project_id` for narrower project scoping.
- Use `task.*` for work with intent, rationale, parameters, evidence, and outcomes.
- Use `artifact.*` when another agent or scientist needs to critique, revise, verify, or inspect a thread directly.
- Use `memory.add` / `memory.add_batch` for raw chunks and code indexing.
- Log meaningful milestones, not every command.
- Every substantive run must include parameters, why chosen, and outputs.
- Every finished task must include what worked, what failed, validation, uncertainty, and followups.

## How Shared Multi-Agent Memory Works

If multiple agents write to the same tenant, later agents can search and recover:

- what another agent was trying to achieve
- why a method was chosen
- which parameters were already tried
- which runs failed
- which evidence supported a conclusion
- which critiques and revisions were already recorded
- who contributed to the artifact thread
- what work remains

That is the point of the artifact schema: consistency across agents and scientists.

If older history was accidentally written under another local tenant on the same daemon, project-scoped retrieval can recover it only through configured same-project aliases. Future writes should still converge on one shared tenant.

## Retrieval Patterns

### Start with `memory.search` (the primary surface)

`memory.search` is the default search entry point. It accepts a `mode`
parameter that biases results:

- `generic` — unbiased hybrid (dense + sparse + reranker)
- `brief_project` — favour project briefs and onboarding summaries
- `resume_task` — favour task-resume digests
- `find_failures` / `find_decisions` / `find_evidence` / `find_highlights` — bias toward that library digest

Default to compact retrieval for broad searches:

- start with `compact=true` and a conservative `token_budget` such as 2000-4000
- keep `include_artifact=false` and use `include_text=false` when IDs are enough
- fetch full selected chunks with `memory.get` only after the compact pass
- record notable duplicate, payload, or latency findings with `memory.health` in task artifacts
- when requesting duplicate examples, remember `duplicate_limit` limits only previews; aggregate duplicate ratios still cover the full tenant/project scope
- run `memory.dream` as a dry run before applying retention or compaction cleanup

Examples:

- "Find architecture notes about auth middleware" → `memory.search`
- "What went wrong on this project?" → `memory.search` with `mode=find_failures`
- "Summarise the last week of work on project X" → `memory.search` with `mode=brief_project`

### Reach for `task.search` / `artifact.search` when the *shape* matters

These tools return the same underlying hits but with enriched output
structures (task/artifact bodies, thread links, grounding refs). Prefer
them when your downstream code needs that shape:

- `task.search` — task-centric filtering (by tool name, dataset, status, confidence)
- `artifact.search` — artifact-native filtering (by role, kind, reply-to, challenge)

For `artifact.search`, use `compact=true`, `include_artifact=false`, and
`include_matched_text=false` for discovery. Then call `artifact.get` for the
small set of artifact IDs you actually need to inspect.

If you just want "find the thing that matches my query", stick with
`memory.search`.

### `context.search_context_documents` is deprecated

It still works and still returns context-document-specific metadata, but
`memory.search` with appropriate tag filters covers the same ground. New
integrations should skip it.

## Minimal Example

```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "task.start",
    "arguments": {
      "tenant_id": "oncogene-screen",
      "project_id": "crispr-screen",
      "goal": "Identify candidate regulators of the phenotype",
      "motivation": "Previous screens showed the phenotype but not the mechanism",
      "hypothesis": "A small set of transcriptional regulators explains the signal",
      "scientific_question": "Which regulators are most strongly associated with the phenotype?",
      "dataset_refs": [
        {"name": "screen_counts", "version": "v3"}
      ],
      "expected_outputs": [
        "ranked candidate list",
        "validation summary"
      ]
    }
  },
  "id": 1
}
```

Then:

- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`

If the next important event is a critique or verification artifact, use:

- `artifact.create`
- `artifact.search`
- `artifact.list_thread`

## Codebase Indexing Pattern

For raw repository chunks:

1. Start an indexing task with `task.start`
2. Use `task.run_start` / `task.run_finish` for the indexing job
3. Store file chunks via `memory.add_batch`
4. Finish the indexing task with `task.finish`

This keeps the code chunks searchable while also recording the indexing job’s rationale and results.

## Example Guides

- [session_tracking.md](examples/session_tracking.md)
- [decision_tracking.md](examples/decision_tracking.md)
- [codebase_indexing.md](examples/codebase_indexing.md)

## Practical Rule

If another agent would later need to know why you did something, what parameters you used, or what failed, that information belongs in a `task.*` artifact, not only in a generic `memory.add` chunk.
