# CLI reference

Everything `memd` does is a subcommand on the same binary. Commands fall into
three groups: **agent-facing** (the retrieval and write commands agents run every session),
**administrative** (init, maintenance, stats, exports), and **structured
operations** (`memd call <operation>` and `memd batch --jsonl`).

## Agent-facing commands

For write-quality expectations and cleanup safety, see the
[Operational contract](operational-contract.md).

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
| `memd doctor` | Diagnose binary discovery (incl. PATH-binary version skew), the resolved `--data-dir`, warm-worker reachability and worker-vs-CLI version skew, global agent rules, Claude Code SessionStart hook, and current project scope; `--strict` exits 2 when any check fails. |
| `memd memory-md` | Refresh project-root `memory.md` with the strongest takeaways for session-start use. |
| `memd eval-memory-md` | Gate startup-memory quality with `--min-useful-ratio` and `--max-generated-wrappers`. |
| `memd eval-retrieval` | Gate retrieval quality with precision, hit-rate, recall, and MRR thresholds. |
| `memd eval-write-quality` | Gate write admission, duplicate reuse, storage growth, and retention compaction. |
| `memd audit` | Report tenant/project storage shape, generated-wrapper noise, alias groups, unreadable active rows, and routine progress summaries that still lack an expiry; `--strict` exits 2 when `unreadable_active_chunks > 0`. |
| `memd report` | Usefulness and self-diagnosis report from the usage ledger and store metadata; `--strict` exits 2 on any `[warn]` line. |
| `memd cleanup-plan` | Generate a non-destructive cleanup approval report with archive/purge command previews and post-cleanup pass criteria. |
| `memd purge` | Dry-run or archive-first cleanup of hidden rows; `--apply` verifies the archive before mutation, and `--include-unreadable-active` previews active metadata rows whose segment payload cannot be loaded. |
| `memd purge-archive` | Read-only verification for `memd purge --archive` files: validates format/counts/payload flags, emits SHA-256, and can enforce expected tenant/project. |
| `memd consolidate` | Call the configured LLM (Claude Haiku or Codex Spark, selected by `MEMD_CONSOLIDATOR`) to rewrite recent chunks into deduplicated `kind:consolidated` lessons. Sources are soft-tombstoned via `ChunkStatus::Superseded` (never deleted). With an explicit `--tenant-id` and no `--project-id` the run is tenant-wide; the resulting lessons surface via tenant-wide search and memory-md machine-wide takeaways. |
| `memd session-start` | Auto-create a minimal `.memd/project_scope.json` when missing, refresh `memory.md` synchronously, then spawn a background consolidation when enough chunks have accumulated. Wired into Claude Code via the bundled skill installer; a Codex hook template lives at `memd-skill/examples/codex_session_start_hook.json`. |
| `memd eval-counterfactual` | Replay a JSONL benchmark file; write an overlap@k / rank-shift report under `evals/bench/reports/`. Monitors whether `kind:consolidated` lessons are load-bearing in retrieval. |
| `memd maintenance` | Disk hygiene: sweep orphan HNSW snapshots, report what changed. |

- `memory-md --explain-output <path>` writes a JSON candidate audit with query
  source, score components, tags, and display/filter decisions.
- `eval-retrieval` gates with `--min-precision-at-k`,
  `--min-hit-rate-at-k`, `--min-known-recall-at-k`, and `--min-mrr`.
- `eval-write-quality` gates with `--min-rejection-or-downgrade-rate`,
  `--min-duplicate-reuse-rate`, `--max-total-chunks`, `--max-disk-bytes`, and
  `--require-retention-compaction`.

## Structured operations (`memd call`)

`memd call <operation> --json ...` exposes the historical operation surface
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
across CLI calls. Warm-routable write commands route through the worker by
default, so the worker is the normal single-writer path:

```bash
memd warm start
memd agent-context --warm required --tenant-id quickstart --query "..."
memd warm stop
```

Flags:

- `--warm auto` (default) — use the local worker, starting it if needed; fall
  back to the current CLI process if startup or connection fails.
- `--warm off` — always run in the current CLI process.
- `--warm required` — require a local worker; fail if it cannot be started or
  reached, and hard-error on cold-only variants.

Routable commands: see [Shared topology](shared-topology.md).

For scripts that need many structured operations in one loaded process:

```bash
memd batch --jsonl requests.jsonl
memd batch --jsonl - --stream
```

Each JSONL line should contain `{"tool":"memory.search","arguments":{...}}`;
the command emits one JSON result row per input line.

See [Quick start](quickstart.md) for end-to-end examples and
[Configuration](configuration.md) for the environment variables that change
defaults.
