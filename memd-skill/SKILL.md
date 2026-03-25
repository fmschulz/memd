---
name: memd
description: Use when coding agents or AI scientists need a shared local MCP knowledge base to preserve cross-session memory, structured task history, and artifact-based collaboration across Codex and Claude.
---

# memd

Shared knowledge artifacts over MCP for coding agents and AI scientists.

Use the same shared local `memd` daemon URL for Codex CLI and Claude Code when you want one machine to host a shared knowledge base across sessions.

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

## Tool Surface

`memd` exposes 40 MCP tools.

### Generic Memory

- `memory.search`
- `memory.add`
- `memory.add_batch`
- `memory.get`
- `memory.delete`
- `memory.feedback`
- `memory.stats`
- `memory.metrics`
- `memory.compact`
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
- `artifact.get`
- `artifact.search`
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
- `artifact.find_failures` to retrieve a digest-backed failure library
- `artifact.find_decisions` to retrieve explicit and inferred decisions
- `artifact.find_evidence` to retrieve digest-backed evidence highlights
- `artifact.find_highlights` to retrieve ranked, high-uplift lessons for future agents
- `artifact.list_thread` to inspect the full collaboration thread around an artifact
- `context.brief_project` to generate or refresh a persisted project brief digest
- `memory.search` to search broader raw memory and context

`memory.compact` can also regenerate project brief and failure/decision/evidence/highlight digests explicitly via `project_id`, `digest_modes`, and `force_digest_rebuild`.

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

## Retrieval Patterns

### Use `task.search` when you know the shape of the answer

Examples:

- “Find failed MMseqs runs for this task”
- “Show evidence artifacts for dataset `rna_seq`”
- “Find tasks where tool `blast` was used”
- “Show completed tasks in project `oncogene_screen` with uncertainty about replicate quality”

### Use `artifact.search` when the artifact itself is the answer

Examples:

- “Find critique artifacts for this thread”
- “Show pending verification artifacts in challenge `artifact_protocol`”
- “Find revisions replying to a given artifact”
- “Show thread artifacts contributed by the PI”

### Use `memory.search` when you want broader context

Examples:

- “Find architecture notes about auth middleware”
- “Show indexed code related to PostgreSQL connection handling”
- “Search codified context docs for incident response”

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
