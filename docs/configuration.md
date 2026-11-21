# Configuration

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `MEMD_DEFAULT_TENANT` | `default` | Fallback tenant for operations without `tenant_id`. |
| `MEMD_SQLITE_POOL_MAX` | `16` | Max SQLite connections in the pool. |
| `MEMD_CROSS_ENCODER_DISABLE` | unset | When `1`, skip ONNX cross-encoder initialization. |
| `ORT_DYLIB_PATH` | unset | Override ONNX Runtime shared library location. |
| `MEMD_CONSOLIDATOR` | `auto` | LLM backend for `memd consolidate`: `claude`, `codex`, `auto`, `mock`. |
| `MEMD_CONSOLIDATION_MODEL` | `sonnet` | Claude consolidation model; overrides `consolidation_model` in the config file. |
| `MEMD_WARM_START_TIMEOUT_MS` | `600000` | Startup budget for a live worker. On expiry the starter kills and reaps its child before cold fallback. |
| `MEMD_WARM_CLIENT_TIMEOUT_MS` | `30000` | Deadline for short worker requests. Maintenance, imports, batches, generic operation calls, and reranked searches wait for completion. |
| `MEMD_WARM_IDLE_TIMEOUT_SECS` | `1800` | Warm worker exits after this many seconds without requests, releasing the writer lock; `0` disables. |
| `MEMD_WRITER_LOCK_TIMEOUT_MS` | `10000` | Total retry budget for taking the data-dir writer lock on direct writes. |
| `MEMD_USAGE_LEDGER` | on | `off`, `0`, `false`, or `no` disables usage-event recording. |
| `MEMD_USAGE_RETENTION_DAYS` | `90` | Usage-ledger TTL in days; older events are swept opportunistically. |

## Worker environment

Warm-routed commands execute inside the worker process. Environment variables
such as `MEMD_CONSOLIDATOR`, `MEMD_CONSOLIDATION_MODEL`, `MEMD_USAGE_LEDGER`, and
`MEMD_USAGE_RETENTION_DAYS` are resolved from the worker's environment, not
from the invoking shell. To apply a change, restart the worker with
`memd warm stop`; the next warm-routed command auto-starts a fresh one.
`MEMD_WRITER_LOCK_TIMEOUT_MS` applies to the process taking the lock, either a
direct-write CLI process or worker startup.

Long socket paths use a private directory with mode `0700` at
`$XDG_RUNTIME_DIR/memd-warm`, or a uid-specific directory in the system
temporary directory when `XDG_RUNTIME_DIR` is unset. Worker counts use this
same root. Startup rejects a root owned by another user or replaced by a symlink.

Claude consolidation runs in a fresh temporary directory with built-in tools,
MCP servers, skills, and user/project settings disabled. Memory strings are
escaped JSON data in the prompt. Tests use a stub CLI and make no model calls.

## Config file

Default location: `~/.config/memd/config.toml`. Override with `--config <path>`.

Full annotated reference:

```toml
# memd configuration
#
# Copy to ~/.config/memd/config.toml and customize.

# Directory for tenant data storage.
# Each tenant gets a subdirectory: {data_dir}/tenants/{tenant_id}/
# Supports ~ for home directory expansion.
data_dir = "~/.memd/data"

# Claude consolidation model alias or explicit model name.
# MEMD_CONSOLIDATION_MODEL takes precedence.
consolidation_model = "sonnet"

# Logging level: trace, debug, info, warn, error.
log_level = "info"

# Log format: json (recommended for production) or pretty (development).
log_format = "json"

[server]
# Compatibility/scope routing. The table name remains [server] for existing
# config files; this CLI-only binary exposes no network or stdio server mode.
allow_cross_tenant_project_fallback = false

[[server.project_aliases]]
tenant_id = "lab"
project_id = "memd"
aliases = [
  { tenant_id = "legacy", project_id = "memd", reason = "migrated history" },
  { tenant_id = "shared", reason = "shared lessons" },
]
```

Unknown keys are silently ignored because the config structs do not deny
unknown fields.

Retrieval variant is not a config key. Use the global CLI flag
`--search-variant` with `hybrid-feature`, `hybrid-cross-encoder`,
`dense-only`, or `bm25-only`. When it is not given, the default is
model-conditional: `dense-only` for `--embedding-model bge-base`, and
`hybrid-feature` for `all-minilm` and other models. An explicit
`--search-variant` always takes precedence. `bm25-only` opens the sparse index
without loading a dense model and preserves lexical rank by disabling metadata
and query-text feature reranking. Stored feedback can still adjust the served
order after lexical retrieval.

## Embedding models

Select the model with `--embedding-model`: `all-minilm` (default, 384 dimensions),
`bge-small` (BAAI/bge-small-en-v1.5, 384 dimensions), or `bge-base`
(BAAI/bge-base-en-v1.5, 768 dimensions). BGE uses CLS pooling, unit normalisation,
and the query-only prefix `Represent this sentence for searching relevant passages: `.
`bge-small` keeps the `hybrid-feature` search default.

The model loads on the first call that embeds text, not when a store opens.
Commands that never embed, such as `get`, `stats` and `doctor`, do not load it,
and a missing or broken model file is an error of the call that needed the
embedding. One copy of the weights serves all concurrent work. Each forward
pass runs on one thread and a call that embeds many texts runs one pass per
core, up to 32, with at most 8 texts per pass; `MEMD_EMBED_BATCH_SIZE` can
lower that limit.

Switching models on an existing store requires a writable open with
`MEMD_BACKFILL_HNSW_ON_STARTUP=1`. This rebuilds dense indexes from segments;
equal vector dimensions do not make models interchangeable. Unmarked existing
384-d stores are read as `all-minilm`, and 768-d stores as `bge-base`.

## Project alias compatibility

Cross-tenant project aliasing is **off by default**. Enable it only when
consolidating mis-routed history; every widened hit produces a warning log.
Same-tenant project scoping is the recommended default — keep separate trust
domains in separate data directories or under explicit tenant conventions.

## Optional reranker assets

The cross-encoder reranker and MemReranker-4B paths have their own cache and
runtime configuration. See [Optional rerankers](reranking.md).
