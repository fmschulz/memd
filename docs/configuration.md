# Configuration

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `MEMD_DEFAULT_TENANT` | `default` | Fallback tenant for operations without `tenant_id`. |
| `MEMD_SQLITE_POOL_MAX` | `16` | Max SQLite connections in the pool. |
| `MEMD_CROSS_ENCODER_DISABLE` | unset | When `1`, skip ONNX cross-encoder initialization. |
| `ORT_DYLIB_PATH` | unset | Override ONNX Runtime shared library location. |
| `MEMD_CONSOLIDATOR` | `auto` | LLM backend for `memd consolidate`: `claude`, `codex`, `auto`, `mock`. |
| `MEMD_WARM_IDLE_TIMEOUT_SECS` | `1800` | Warm worker exits after this many seconds without requests, releasing the writer lock; `0` disables. |
| `MEMD_WRITER_LOCK_TIMEOUT_MS` | `10000` | Total retry budget for taking the data-dir writer lock on direct writes. |
| `MEMD_USAGE_LEDGER` | on | `off`, `0`, `false`, or `no` disables usage-event recording. |
| `MEMD_USAGE_RETENTION_DAYS` | `90` | Usage-ledger TTL in days; older events are swept opportunistically. |

## Worker environment

Warm-routed commands execute inside the worker process. Environment variables
such as `MEMD_CONSOLIDATOR`, `MEMD_USAGE_LEDGER`, and
`MEMD_USAGE_RETENTION_DAYS` are resolved from the worker's environment, not
from the invoking shell. To apply a change, restart the worker with
`memd warm stop`; the next warm-routed command auto-starts a fresh one.
`MEMD_WRITER_LOCK_TIMEOUT_MS` applies to the process taking the lock, either a
direct-write CLI process or worker startup.

## Config file

Default location: `~/.config/memd/config.toml`. Override with `--config <path>`.

Full annotated reference:

```toml
# memd configuration
#
# Copy to ~/.config/memd/config.toml and customize.

# Directory for tenant data storage.
# Each tenant gets a subdirectory: {data_dir}/{tenant_id}/
# Supports ~ for home directory expansion.
data_dir = "~/.memd/data"

# Logging level: trace, debug, info, warn, error.
log_level = "info"

# Log format: json (recommended for production) or pretty (development).
log_format = "json"

[server]
# Compatibility/scope routing. The table name remains [server] for existing
# config files; the binary no longer exposes network or stdio server mode.
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
`--search-variant` with `hybrid-feature` (default), `hybrid-cross-encoder`,
`dense-only`, or `bm25-only`.

## Project alias compatibility

Cross-tenant project aliasing is **off by default**. Enable it only when
consolidating mis-routed history; every widened hit produces a warning log.
Same-tenant project scoping is the recommended default — keep separate trust
domains in separate data directories or under explicit tenant conventions.

## Optional reranker assets

The cross-encoder reranker and MemReranker-4B paths have their own cache and
runtime configuration. See [Optional rerankers](reranking.md).
