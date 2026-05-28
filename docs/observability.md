# Observability

`memd` exposes operational signal through three surfaces: stats/health/metrics
operations, structured tracing logs, and hit-counter telemetry written to
`.memd/data/hit_counts.jsonl`.

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

`memd eval-counterfactual` measures whether `kind:consolidated` chunks are
actually moving ranks versus a same-pass filtered baseline.

## Startup Memory Explanations

When `memory.md` looks noisy, generate an explanation report:

```bash
memd memory-md \
  --project-dir . \
  --output memory.md \
  --explain-output .memd/memory-explain.json
```

The JSON report lists each retrieved candidate with source query, raw rank,
score components, tags, generated-digest status, matched sources, and the final
display/filter decision. This is the fastest way to see whether generated
wrappers, stale records, or below-threshold candidates are affecting startup
context.
