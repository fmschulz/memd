# Quick Start

This guide gets `memd` running, validates MCP behavior, and connects it to Claude Code/Codex.

## 1. Build

```bash
cargo build --release
./target/release/memd --version
```

## 2. Start memd

```bash
# Persistent mode (default data dir: ~/.memd/data)
./target/release/memd --mode mcp

# Or isolated in-memory mode for local testing
./target/release/memd --mode mcp --in-memory --data-dir /tmp/memd-quickstart
```

## 3. Send MCP requests manually

In another terminal, pipe JSON-RPC lines into `memd`:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"quickstart","version":"0.1.0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory.add","arguments":{"tenant_id":"quickstart_tenant","text":"parseConfig reads TOML and validates required fields","type":"code","tags":["rust","config"]}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory.search","arguments":{"tenant_id":"quickstart_tenant","query":"parseConfig","k":5}}}' \
  | ./target/release/memd --mode mcp --in-memory --data-dir /tmp/memd-quickstart
```

You will see JSON-RPC responses. Tool payloads come inside:

- `result.content[0].type == "text"`
- `result.content[0].text == "<JSON string payload>"`

## 4. Core memory tool examples

### Add

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "text": "Config parser now rejects missing api_key",
      "type": "decision",
      "project_id": "backend",
      "source": {
        "path": "src/config.rs",
        "repo": "memd"
      },
      "tags": ["config", "validation"]
    }
  }
}
```

### Search

```json
{
  "jsonrpc": "2.0",
  "id": 11,
  "method": "tools/call",
  "params": {
    "name": "memory.search",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "query": "config parser",
      "k": 10,
      "debug_tiers": true,
      "filters": {
        "time_range": {
          "from": "2026-01-01T00:00:00Z",
          "to": "2026-01-31T23:59:59Z"
        }
      }
    }
  }
}
```

Validation rules:

- `k` must be between `1` and `100`
- `filters.time_range.from/to` must be ISO 8601
- if both are set, `from <= to`

Current behavior notes:

- `filters.types`, `project_id`, and `filters.time_range` are enforced as search result filters
- `filters.episode_id` is enforced via episode tags (`episode:<id>`)
- each search result includes a `citation` object (content hash + provenance + split offsets when chunked)
- each search result may include `episode_id` when present
- search applies adaptive retrieval depth for complex queries and filters
- when initial retrieval is empty, query normalization retry may run; response includes `repair_info`

### Get / Delete / Stats

```json
{
  "jsonrpc": "2.0",
  "id": 12,
  "method": "tools/call",
  "params": {
    "name": "memory.get",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "chunk_id": "<uuid>"
    }
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 13,
  "method": "tools/call",
  "params": {
    "name": "memory.delete",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "chunk_id": "<uuid>"
    }
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 14,
  "method": "tools/call",
  "params": {
    "name": "memory.stats",
    "arguments": {
      "tenant_id": "quickstart_tenant"
    }
  }
}
```

## 5. Metrics and compaction

### Metrics

```json
{
  "jsonrpc": "2.0",
  "id": 20,
  "method": "tools/call",
  "params": {
    "name": "memory.metrics",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "include_recent": false,
      "include_tiered": true
    }
  }
}
```

### Compaction

```json
{
  "jsonrpc": "2.0",
  "id": 21,
  "method": "tools/call",
  "params": {
    "name": "memory.compact",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "force": false
    }
  }
}
```

Compaction behavior:

- persistent store: runs if thresholds exceeded, or immediately when `force=true`
- in-memory store: returns `status: "skipped"`

### Consolidate Episode

```json
{
  "jsonrpc": "2.0",
  "id": 22,
  "method": "tools/call",
  "params": {
    "name": "memory.consolidate_episode",
    "arguments": {
      "tenant_id": "quickstart_tenant",
      "episode_id": "session-2026-02-09",
      "max_chunks": 40,
      "retain_source_chunks": true
    }
  }
}
```

## 6. Chunk types

Canonical values:

- `code`
- `doc`
- `trace`
- `decision`
- `plan`
- `research`
- `message`
- `summary`
- `other`

Accepted aliases:

- `scientific` maps to `doc`
- `general` maps to `other`

## 7. Long-document behavior

When `text` length is greater than `1000` characters, `memory.add`/`memory.add_batch` split input into multiple chunks.

Stored chunks get tags:

- `chunk_index:<n>`
- `total_chunks:<m>`
- `char_start:<n>`
- `char_end:<n>`

API return semantics:

- one returned `chunk_id` per input chunk (first stored chunk ID)

## 8. Connect to Claude Code

Add an MCP server entry (path may vary by installation):

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

## 9. Connect to Codex

```json
{
  "memd": {
    "command": "/absolute/path/to/memd",
    "type": "stdio",
    "args": ["--mode", "mcp"]
  }
}
```

## 10. Initialize guardrails (recommended)

```bash
./target/release/memd --mode cli init --tenant-id quickstart_tenant
```

This creates repository guardrail files and MCP snippets, and by default updates:

- `~/.codex/mcp_config.json`
- `~/.config/claude/mcp_settings.json`

Scope options:

```bash
# Default local scope (read/write current tenant only)
./target/release/memd init --tenant-id quickstart_tenant --scope local

# Global read scope (read all discovered tenants, write current tenant only)
./target/release/memd init --tenant-id quickstart_tenant --scope global

# Explicit allowlist read scope
./target/release/memd init --tenant-id quickstart_tenant --scope allowlist --allow-tenants shared_arch,shared_debug
```

## 11. Validate locally

```bash
cargo test -p memd
cargo test -p memd-evals
RUST_LOG=error cargo run -p memd-evals -- --suite mcp --skip-build
```

## 12. Export memory for human review

Use the CLI export command to inspect what `memd` has stored for a tenant:

```bash
./target/release/memd --mode cli export \
  --tenant-id quickstart_tenant \
  --format markdown \
  --output memd-export.md
```

Other formats:

- `--format json`
- `--format jsonl`

Optional quality/eval suites:

```bash
RUST_LOG=error cargo run -p memd-evals -- --suite hybrid --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite retrieval --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite true-semantic --skip-build
```

Offline benchmark protocol:

```bash
./evals/scripts/run_offline_retrieval_benchmark.sh --model all-minilm --seed 42
```

## 13. Troubleshooting

- `invalid 'k': must be between 1 and 100`: adjust `k`
- `invalid filters.time_range.*`: use ISO 8601 timestamps
- `tenant_id ... invalid characters`: use only letters, digits, underscores
- `memory.compact` returns skipped: expected in in-memory mode, or no thresholds exceeded in persistent mode
