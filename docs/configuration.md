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

`--embedding-model` is optional. The model of a run is, in this order:

1. the `--embedding-model` flag, when given: `all-minilm` (384 dimensions),
   `bge-small` (BAAI/bge-small-en-v1.5, 384 dimensions), or `bge-base`
   (BAAI/bge-base-en-v1.5, 768 dimensions);
2. otherwise the model the store records in its `embedding_model` marker. An
   unmarked store with dense vectors is read by vector width: 384-d is
   `all-minilm`, 768-d is `bge-base`. The width comes from the 24-byte header
   of `embeddings.bin`, so the lookup costs the same for any index size. The
   index loader validates the vectors when it loads them;
3. otherwise `bge-small`, for a new store that has neither.

A new store therefore uses `bge-small` and records it. An existing store keeps
its model after an upgrade: no flag, no error, no rebuild, and no download of
another model. A flag that differs from the store's model is an error.
BGE uses CLS pooling, unit normalisation, and the query-only prefix
`Represent this sentence for searching relevant passages: `. `bge-small` keeps
the `hybrid-feature` search default. The first embedding of a `bge-small`
store downloads 133 MB of model files; `all-minilm` needs 91 MB.

A flagless warm client and a flagless warm worker resolve the model from the
same store, so they agree. A client that names a different model than the
running worker serves is an error, as before.

The model loads on the first call that embeds text, not when a store opens.
Commands that never embed, such as `get`, `stats` and `doctor`, do not load it,
and a missing or broken model file is found by the first call that embeds. The
load is tried once per process. After a failure the store behaves as if dense
search were off: search answers from the keyword index and says so in
`scope_status`, and writes still index keywords. One copy of the weights serves
all concurrent work. Each forward
pass runs on one thread and a call that embeds many texts runs one pass per
core, up to 32, with at most 8 texts per pass; `MEMD_EMBED_BATCH_SIZE` can
lower that limit.

### Switch the model of an existing store

Switching is always an explicit act. It rebuilds the dense indexes from the
stored segments; equal vector dimensions do not make models interchangeable.
Two steps switch a store to `bge-small`:

1. Stop the warm worker, then run any writer command with the opt-in variable
   and the flag. The worker holds the writer lock, so `--warm off` makes the
   command open the store itself:

   ```bash
   memd warm stop
   MEMD_BACKFILL_HNSW_ON_STARTUP=1 memd --embedding-model bge-small --warm off \
     add --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" --chunk-type doc \
     --text "Switched this store to bge-small."
   ```

2. Drop the variable and the flag. Every later command resolves `bge-small`
   from the store.

Read-only commands such as `stats` or `search` refuse the switch. The
switching command loads the new model before it deletes anything. A machine
that cannot read or download the new files (for example offline) keeps its old
dense index and marker and prints `not switching the dense index to ...`. Run
the command again once the files are available.

The rebuild runs in the background of the writer. A command that exits during
it waits up to 10 seconds, saves what it indexed, and the next writer command
continues. The marker `<data_dir>/embedding_model` reads
`BAAI/bge-small-en-v1.5 rebuilding` until the rebuild finishes, then
`BAAI/bge-small-en-v1.5`. Until then, chunks that are not yet re-embedded are
found through the sparse index only. Do not leave
`MEMD_BACKFILL_HNSW_ON_STARTUP=1` set: it also backfills cold tenants at every
open. To go back, use the same two steps with `--embedding-model all-minilm`.

## Project alias compatibility

Cross-tenant project aliasing is **off by default**. Enable it only when
consolidating mis-routed history; every widened hit produces a warning log.
Same-tenant project scoping is the recommended default — keep separate trust
domains in separate data directories or under explicit tenant conventions.

## Optional reranker assets

The cross-encoder reranker and MemReranker-4B paths have their own cache and
runtime configuration. See [Optional rerankers](reranking.md).
