# Configuration

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `MEMD_DEFAULT_TENANT` | `default` | Fallback tenant for operations without `tenant_id`. |
| `MEMD_SQLITE_POOL_MAX` | `16` | Max SQLite connections in the pool. |
| `MEMD_DIGEST_SWEEP_INTERVAL_SEC` | `10` | Background digest-sweeper interval; `0` disables. |
| `MEMD_CROSS_ENCODER_DISABLE` | unset | When `1`, skip ONNX cross-encoder initialization. |
| `ORT_DYLIB_PATH` | unset | Override ONNX Runtime shared library location. |
| `MEMD_CONSOLIDATOR` | `auto` | LLM backend for `memd consolidate`: `claude`, `codex`, `auto`, `mock`. |
| `MEMD_SHARED_TENANT` | unset | Destination tenant for `--promote-to-shared` consolidations. |

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
# Transport for the (deprecated) MCP server. Default: stdio.
transport = "stdio"

# Bind address for HTTP transport (if used).
bind = "127.0.0.1:8787"

# Endpoint path for streamable HTTP transport.
path = "/mcp"

[retrieval]
# Retrieval variant. Use "hybrid" for default; "hybrid-cross-encoder"
# enables the optional ONNX cross-encoder reranker (see Optional rerankers).
search_variant = "hybrid"
```

## Project alias compatibility

Cross-tenant project aliasing is **off by default**. Enable it only when
consolidating mis-routed history; every widened hit produces a warning log.
Same-tenant project scoping is the recommended default — keep separate trust
domains in separate data directories or under explicit tenant conventions.

## Optional reranker assets

The cross-encoder reranker and MemReranker-4B paths have their own cache and
runtime configuration. See [Optional rerankers](reranking.md).
