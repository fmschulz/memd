# Observability

`memd` has four observability surfaces: the usage ledger and `memd report`
(including the session-start health header), stats/health/metrics operations,
structured tracing logs, and per-search hit counters in
`.memd/data/hit_counts.jsonl`.

## Usage ledger

The `usage_events` table lives inside `metadata.db`. It records operational
events for self-diagnosis and report generation.

| Recorded ops | Notes |
| --- | --- |
| `add`, `search`, `agent_context`, `get`, `delete`, `purge`, `consolidate`, `import_omf`, `report` | Written by the ops layer for growth, retrieval, and learning-digest analytics. |

Recording is best-effort. Ledger failures are debug-logged only and never fail
or slow the user operation. ReadOnly store opens skip recording entirely, so
direct `--warm off` reads such as `search` and `agent-context`, and the
always-cold `memd get`, record nothing; default warm-routed operations record
through the worker.

| Variable | Default | Effect |
| --- | --- | --- |
| `MEMD_USAGE_LEDGER` | on | `off`, `0`, `false`, or `no` disables usage-event recording. |
| `MEMD_USAGE_RETENTION_DAYS` | `90` | Events older than this are swept opportunistically, at most hourly. |

Worker-scoped environment variables are resolved from the worker process; see
[Configuration](configuration.md#worker-environment).

Search query text is never stored verbatim. The ledger stores only a 16-hex
`q_hash` for distinct-query analytics. The hash is not stable across Rust
releases, so distinctness comparisons are meaningful only among events written
by the same `memd` build.

## memd report

`memd report` renders a usefulness and self-diagnosis report from the usage
ledger and store metadata.
On a 10k-chunk store the report renders in 656 ms (dev machine, 2026-06).

| Flag | Behavior |
| --- | --- |
| `--tenant-id` | Scope to one tenant; omit to scan every known tenant. |
| `--project-id` | Restrict to one project. |
| `--since Nd\|Nh` | Window in days or hours; default `7d`. |
| `--format markdown\|json` | Output format; default `markdown`. |
| `--strict` | Exit code 2 when any `[warn]` self-diagnosis line is present. |
| `--top N` | Max learning-digest entries; default `5`. |
| `--output <path>` | Write to a file; default is stdout. |
| `--warm <auto\|off\|required>` | Routes through the worker by default. |

Markdown and JSON contain the same data:

- `## Growth` — admitted, downgraded, and rejected writes with reasons; bytes
  added; deletes, imports, and purges; expired and superseded chunks in the
  window; store totals.
- `## Learning digest` — consolidated and high-priority counts, including
  `high_priority_in_window`, plus entries with their `Agent action:` lines.
- `## Retrieval usefulness` — searches, hit rate, distinct queries by hash,
  distinct chunks served, top-served chunks, and zero-hit share.
- `## Self-diagnosis` — summary lines with no prefix, plus `[warn]`-prefixed
  lines only when warnings exist.

`hit_rate` is the share of search events that returned at least one result
(`1 - zero_hits/searches`). Vector search returns top-k on any non-empty store,
so `zero_hits` is approximately an empty-store or empty-scope signal and
`hit_rate` is inflated until a relevance threshold exists. Relevance-threshold
based usefulness measurement is future work.

Eval commands such as `memd eval-counterfactual` execute real searches through
the same ops layer. Their events land in the ledger and inflate search counts
in the report window; tagging or filtering eval-origin events is future work.

## Session-start state and health

`memd memory-md` starts with a single scope line (generation date, tenant,
project) followed by `## Memory health`. It deliberately does not restate task,
handoff, or git state: those live in `tasks/todo.md`, `docs/handoffs/`, and git,
which an agent reads directly. `memory.md` carries only facts no repo file
holds.

Candidates come from one tenant-wide store scan, partitioned by project, rather
than from a fixed set of search queries. Takeaways whose distinctive tokens are
already contained in a repo file (`tasks/**`, `docs/handoffs/`, `README.md`,
`CLAUDE.md`, `AGENTS.md`) are suppressed with reason `covered_by_repo:<path>`.
The project and machine-wide candidates feed one bounded union before
section assignment and display truncation. Exact chunk IDs, consolidation
lineage, and high-confidence topic duplicates are collapsed across that union.
If the active project's item competes with a machine-wide match, the project
item is retained.

`## Memory health` summarizes lines derived from `memd report`.
This is best-effort and is silently skipped if report generation fails. If it
reports `memory degraded`, inspect the store with
`memd audit --format markdown` or `memd report --strict`; the warning means
active metadata rows exist whose segment payloads could not be read.

## Stats, health, metrics

- `memory.stats` reports uncapped `active_chunks`, `deleted_chunks`,
  `total_chunks`, and active/deleted/all chunk-type maps. The legacy
  `chunk_types` field remains the active-count map.
- `memory.health` is a read-only tenant/project report for duplicate canonical
  text, index coverage, canonical/artifact payload sizes, recent latency
  tails, and warnings. When `include_examples` is true, `duplicate_limit`
  limits only the number of example groups returned; aggregate duplicate
  counts and ratios still cover the full requested scope.
- `memory.dream` can turn health findings into a bounded maintenance plan. It
  defaults to `dry_run: true`; apply mode uses lifecycle retirement and
  sparse-index pruning for duplicate digest projection chunks, while
  append-only segment rewrite remains explicitly blocked until
  recovery-safe rewrite support exists. Non-digest exact duplicates remain
  report-only.
- `memory.metrics` surfaces per-operation, per-reason rejection counts,
  cache hit rates, HNSW state snapshots, and estimated serialized payload
  size by operation.

Token usage is estimated from serialized request/response bytes; exact
whole-agent or provider billing tokens still require agent or API usage
capture. See
[`token_overhead.md`](scientific-task-memory/benchmark-results/token_overhead.md)
for the benchmark parser and pilot measurement protocol.

## Tracing logs

A `tracing` subscriber emits structured JSON logs when `RUST_LOG` is set.
Every rejected operation increments `MetricsCollector::record_rejection`.

Deprecation warnings for `artifact.create` (mega-schema),
`context.search_context_documents`, and the `artifact.verify` alias log at
`warn!` so migration can be tracked.

## Hit counters

Every CLI search appends one JSONL record per returned chunk to
`.memd/data/hit_counts.jsonl`. The `memory.md` priority formula consumes a
per-chunk 30-day aggregate (1 h TTL cache) — frequently retrieved chunks get
up to +8, chunks with no hits older than 30 days get −2.

`memd eval-counterfactual` measures whether `kind:consolidated` chunks change
ranks versus a same-pass filtered baseline.

## Startup memory explanations

When `memory.md` looks noisy, generate an explanation report:

```bash
memd memory-md \
  --project-dir . \
  --output memory.md \
  --explain-output .memd/memory-explain.json
```

The JSON report lists each retrieved candidate with source query, raw rank,
score components, tags, generated-digest status, quality flags, topic key,
matched sources, structured project state, agent-usefulness metrics, and the
final display/filter decision. This is the fastest way to see whether
generated wrappers, continuation fragments, duplicate topics, stale records, or
below-threshold candidates are affecting startup context.
