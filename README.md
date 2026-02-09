# memd

Local MCP memory daemon for coding agents. `memd` stores and retrieves tenant-isolated memory chunks with hybrid retrieval and optional structural code/trace queries.

## What It Does

- Runs as an MCP server on stdio (`--mode mcp`, default)
- Supports persistent storage (WAL + segments + metadata) or in-memory mode (`--in-memory`)
- Exposes 15 MCP tools (memory, structural, debug, metrics, compaction, episode consolidation)
- Supports hybrid retrieval in persistent mode (dense + sparse + reranking)
- Applies tenant isolation on all read/write operations

## Architecture Rationale

`memd` keeps hot operations local and simple:

- Stdio MCP transport avoids network service complexity for agent integration
- Tenant-scoped storage paths and IDs provide hard partitioning
- Persistent mode combines append-friendly writes (WAL/segments) with query indexes
- In-memory mode enables fast test loops and protocol validation
- Hybrid retrieval blends lexical and semantic signals while keeping fallback behavior deterministic

## Build

```bash
cargo build --release
./target/release/memd --version
```

## Run Modes

```bash
# MCP server (persistent store)
./target/release/memd --mode mcp

# MCP server (in-memory store, useful for tests)
./target/release/memd --mode mcp --in-memory --data-dir /tmp/memd-dev

# CLI mode
./target/release/memd --mode cli --help
```

Data directory precedence:

1. `--data-dir`
2. `config.toml` `data_dir`
3. Default `~/.memd/data`

## MCP Protocol Shape

`memd` expects JSON-RPC 2.0. Tool calls use:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "demo_tenant",
      "text": "parseConfig reads TOML and validates required fields",
      "type": "code"
    }
  }
}
```

Tool results are MCP content blocks containing JSON text:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"chunk_id\":\"019c40c2-e632-7843-ad4b-545e63f66a47\"}"
      }
    ]
  }
}
```

## Tool Inventory (15)

Memory:

- `memory.search`
- `memory.add`
- `memory.add_batch`
- `memory.get`
- `memory.delete`
- `memory.stats`
- `memory.metrics`
- `memory.compact`
- `memory.consolidate_episode`

Structural:

- `code.find_definition`
- `code.find_references`
- `code.find_callers`
- `code.find_imports`

Debug:

- `debug.find_tool_calls`
- `debug.find_errors`

## Behavioral Details

### Input Validation

- `tenant_id`: required and validated (`[A-Za-z0-9_]+`)
- `memory.search.k`: must be `1..=100`
- `filters.time_range.from/to`: ISO 8601 parseable; if both set, `from <= to`
- `chunk_id`: UUID required for `memory.get` and `memory.delete`

### Chunk Type Handling

Canonical types:

- `code`, `doc`, `trace`, `decision`, `plan`, `research`, `message`, `summary`, `other`

Accepted aliases at handler level:

- `scientific -> doc`
- `general -> other`

### Add-Time Splitting

For long text (`> 1000` chars), `memory.add` and `memory.add_batch` use shared split logic across in-memory and persistent stores.

- Additional chunks are stored with tags `chunk_index:<n>` and `total_chunks:<m>`
- Split chunks also include `char_start:<n>` and `char_end:<n>` tags for citation span offsets
- Return value remains one `chunk_id` per input chunk (the first stored chunk ID)

### Search Filters

Current state:

- `k` and `time_range` are validated
- `debug_tiers` returns extra tier timing/source metadata
- `project_id` is enforced as a search result filter
- `filters.types` is enforced as a search result filter
- `filters.time_range` is enforced on `timestamp_created`
- `filters.episode_id` is enforced through episode tags (`episode:<id>`)
- Search responses include `citation` metadata (content hash, provenance, and chunk span offsets when available)
- Search responses include `episode_id` when present on the stored chunk
- Search uses adaptive candidate depth (`fetch_k`) for complex/filtered queries
- If initial retrieval returns no results, a deterministic repair pass normalizes query punctuation/spacing and retries
- `repair_info` in search responses reports whether a repair attempt was made and whether it recovered results

### Episodic Memory

- `memory.add` and `memory.add_batch` accept optional `episode_id`
- Episode IDs are stored as tags (`episode:<id>`) for cross-store compatibility
- `memory.consolidate_episode` creates a `summary` chunk from episode content
- `memory.consolidate_episode` can optionally remove source chunks (`retain_source_chunks=false`)

### Metrics and Compaction

- `memory.metrics` returns `timestamp`, `index`, `latency`, `recent_queries`, `tiered`
- Optional params:
- `tenant_id` filters index metrics
- `include_recent` defaults `true`
- `include_tiered` defaults `true`
- `memory.compact`:
- persistent store: runs thresholded compaction or forced compaction via trait-dispatched backend implementation
- in-memory store: returns `status: skipped`

## Quick Integration

### Claude Code MCP server entry

```json
{
  "mcpServers": {
    "memd": {
      "command": "/absolute/path/to/memd",
      "args": ["--mode", "mcp"]
    }
  }
}
```

### Codex MCP server entry

```json
{
  "memd": {
    "command": "/absolute/path/to/memd",
    "type": "stdio",
    "args": ["--mode", "mcp"]
  }
}
```

## Testing

Core checks:

```bash
cargo test -p memd
cargo test -p memd-evals
RUST_LOG=error cargo run -p memd-evals -- --suite mcp --skip-build
```

Deterministic baseline:

- `cargo test -p memd` is the required green gate for local/CI correctness.
- Network/model-download tests are explicitly ignored by default (Candle embedder tests).
- Run ignored tests only when network access and model downloads are expected:

```bash
cargo test -p memd -- --ignored
```

Additional eval suites:

```bash
RUST_LOG=error cargo run -p memd-evals -- --suite hybrid --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite retrieval --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite true-semantic --skip-build
```

Offline benchmark protocol (Phase 6):

```bash
./evals/scripts/run_offline_retrieval_benchmark.sh --model all-minilm --seed 42
```

Protocol details are documented in `evals/BENCHMARK_PROTOCOL.md`.

## Notes on Datasets

Large retrieval datasets under `evals/datasets/retrieval/` are intended for local benchmarking and are not required for normal operation.

## Related Docs

- `QUICKSTART.md` for end-to-end command examples
- `TESTING.md` for test matrix and release verification commands
- `docs/` for implementation notes and review artifacts
