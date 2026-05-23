# CLI reference

Everything `memd` does is a subcommand on the same binary. Commands fall into
three groups: **agent-facing** (the bread-and-butter retrieval and writes),
**administrative** (init, maintenance, stats, exports), and **structured
operations** (`memd call <operation>` and `memd batch --jsonl`).

## Agent-facing commands

| Command | Purpose |
| --- | --- |
| `memd agent-context` | Prefetch bounded context to a file with audit logs. |
| `memd search` | Direct compact search. |
| `memd add` | Store summaries, traces, evidence, decisions. |
| `memd warm start \| status \| stop` | Manage the private local warm worker used by `--warm auto\|required`. |
| `memd batch --jsonl` | Run structured operation calls from JSONL in one loaded process; `--stream` keeps stdin/stdout open for benchmark clients. |
| `memd get`, `memd delete`, `memd stats` | Inspect and maintain chunks. |
| `memd export`, `memd export-markdown`, `memd export-omf`, `memd import-omf` | Portable local memory operations. |
| `memd init` | Write `.memd/` scope files and CLI guardrail blocks. |
| `memd doctor` | Diagnose binary discovery, data directory, global agent rules, Claude Code SessionStart hook, and current project scope. |
| `memd memory-md` | Refresh project-root `memory.md` with the strongest takeaways for session-start use; pass `--cross-tenant` to add a Cross-Tenant Takeaways section sourced from `kind:consolidated, priority>=8` chunks across other tenants. |
| `memd consolidate` | Call the configured LLM (Claude Haiku or Codex Spark, selected by `MEMD_CONSOLIDATOR`) to rewrite recent chunks into deduplicated `kind:consolidated` lessons. Sources are soft-tombstoned via `ChunkStatus::Superseded` (never deleted). Add `--promote-to-shared` to copy multi-project lessons into the `MEMD_SHARED_TENANT` tenant for cross-project transfer. |
| `memd session-start` | Auto-create a minimal `.memd/project_scope.json` when missing, refresh `memory.md` synchronously, then spawn a background consolidation when enough chunks have accumulated. Wired into Claude Code via the bundled skill installer; a Codex hook template lives at `memd-skill/examples/codex_session_start_hook.json`. |
| `memd eval-counterfactual` | Replay a JSONL benchmark file; write an overlap@k / rank-shift report under `evals/bench/reports/`. Monitors whether `kind:consolidated` lessons are load-bearing in retrieval. |
| `memd maintenance` | Disk hygiene: sweep orphan HNSW snapshots, report what changed. |

## Structured operations (`memd call`)

`memd call <operation> --json …` exposes the historical operation surface
through the executable without starting a separate integration process. This is
the compatibility path for advanced scripts that need structured task,
artifact, context, code, or debug operations before every operation gets a
dedicated first-class subcommand.

### `memory.*` — raw searchable content

- `memory.add` — single chunk (code, doc, note; code chunks with a real
  `source.path` are parsed into the structural index).
- `memory.add_batch` — many chunks in one call.
- `memory.search` — hybrid retrieval with optional `mode`, `project_id`,
  compact/token-budgeted output, and event sibling expansion.
- `memory.get`, `memory.delete`, `memory.stats`, `memory.health`, `memory.metrics`.
- `memory.compact` — explicit digest refresh; supports `digest_modes` and `force_digest_rebuild`.
- `memory.dream` — dry-run-first retention and compaction planning; safely
  retires duplicate digest projections on apply and writes a traceable report.
  Exact duplicate raw chunks are reported by health but not auto-retired by
  the safe profile.

Conversation-style chunks can carry caller-supplied `event:<id>` tags along
with `entry:factual` or `entry:relational`. Passing
`expand_event_siblings: true` to `memory.search` keeps the ranked result list
unchanged and attaches bounded same-tenant/same-project chunks that share the
matched event tag under each result's `expanded_siblings` field.

### `task.*` — structured work

- `task.start` (only `goal` required), `task.progress`, `task.finish`.
- `task.run_start` / `task.run_finish` for substantive runs.
- `task.add_evidence` for concrete evidence against a task.
- `task.get`, `task.search`, `task.resume`.

### `artifact.*` — focused collaboration tools (v0.4)

The single 50-field `artifact.create` has been split into four focused tools
with tight schemas:

- `artifact.review` — request a review; attach summary and requested action.
- `artifact.revision` — supersede a prior artifact with `superseded_by` lineage.
- `artifact.decision` — choose between alternatives with `why_chosen`.
- `artifact.verification` — distinct-writer countersignature; with a different
  `agent_id` than the parent's and `supports_claim = true` it promotes the
  underlying claim to `VerifiedRecord` trust.

Inspection and retrieval:

- `artifact.get`, `artifact.search`, `artifact.list_thread`.
- `artifact.find_related` (retrieval helper; former `artifact.verify` alias
  is deprecated but still works).
- `artifact.find_failures`, `artifact.find_decisions`, `artifact.find_evidence`,
  `artifact.find_highlights`.

`artifact.create` remains available for backwards compatibility with a
deprecation warning. Digest artifacts are system-generated and cannot be
forged through `artifact.create`.

`artifact.search` defaults to the full legacy response. Passing `compact: true`
adds `budget_info`; `include_artifact: false` and `include_matched_text: false`
return only identifiers, summaries, ranking, and trust/grounding metadata so a
caller can fetch selected records with `artifact.get`.

### `code.*` — structural navigation

`code.find_definition`, `code.find_references`, `code.find_callers`,
`code.find_imports`. Index source by calling `memory.add` with `type = "code"`
and a real `source.path`.

### `context.*` — summary-first retrieval

`context.brief_project`, `context.find_relevant_context`,
`context.get_hot_context`, `context.get_files_for_subsystem`,
`context.list_subsystems`, `context.suggest_agent`.

`context.find_relevant_context` can prepend hot-context chunks when
`include_hot` is true. That legacy hot pre-scan is bounded by a short
wall-clock budget so large tenants still fall through to normal retrieval
instead of blocking the whole lookup on a full payload scan.

## Warm and batch execution

For sustained local use, the warm worker keeps the store and indexes hot
across CLI calls:

```bash
memd warm start
memd agent-context --warm required --tenant-id quickstart --query "…"
memd warm stop
```

Flags:

- `--warm auto` (default) — try the worker, fall back to cold.
- `--warm off` — force cold load in this process.
- `--warm required` — fail if the worker isn't reachable.

For scripts that need many structured operations in one loaded process:

```bash
memd batch --jsonl requests.jsonl
memd batch --jsonl - --stream
```

Each JSONL line should contain `{"tool":"memory.search","arguments":{…}}`;
the command emits one JSON result row per input line.

See [Quick start](quickstart.md) for end-to-end examples and
[Configuration](configuration.md) for the environment variables that change
defaults.
