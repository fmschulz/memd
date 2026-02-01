# Quick Start Guide

Get memd running in **15 minutes** and start using intelligent memory for your AI agents.

## Table of Contents

1. [Installation](#installation)
2. [First Run](#first-run)
3. [MCP Integration](#mcp-integration)
4. [Basic Operations](#basic-operations)
5. [Advanced Features](#advanced-features)
6. [Performance Tuning](#performance-tuning)
7. [Troubleshooting](#troubleshooting)

## Installation

### Prerequisites Check

```bash
# Check Rust version (need 1.75+)
rustc --version

# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Build from Source

```bash
# Clone the repository
git clone https://github.com/fmschulz/memd.git
cd memd

# Build release binary (takes ~5-10 minutes first time)
cargo build --release

# Verify build
./target/release/memd --version
```

### Directory Setup

memd automatically creates directories on first run:

```bash
~/.config/memd/          # Configuration
~/.local/share/memd/     # Data storage
  └── tenants/
      └── <tenant-id>/
          ├── segments/    # Chunk storage
          ├── wal/         # Write-ahead log
          ├── indexes/     # HNSW, BM25, symbols
          └── cache/       # Hot tier cache
```

## First Run

### 1. Start the Server

```bash
# Run in MCP mode (default)
./target/release/memd

# Or run in CLI mode for testing
./target/release/memd --mode cli
```

The server will output:

```
[2026-01-31T12:00:00Z INFO memd] Starting memd server
[2026-01-31T12:00:00Z INFO memd] Mode: mcp
[2026-01-31T12:00:00Z INFO memd] Data directory: /home/user/.local/share/memd
[2026-01-31T12:00:00Z INFO memd] Ready to accept requests
```

### 2. Test with Manual MCP Request

In another terminal, send a test request:

```bash
cat << 'EOF' | ./target/release/memd
{
  "jsonrpc": "2.0",
  "method": "tools/list",
  "id": 1
}
EOF
```

You should see a JSON response listing all 13 available tools.

### 3. Add Your First Memory

```bash
cat << 'EOF' | ./target/release/memd
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "quickstart",
      "text": "The parseConfig function reads TOML configuration files and returns a Config struct. It validates all required fields and returns detailed error messages on parse failures.",
      "chunk_type": "code",
      "tags": ["rust", "config", "toml", "parsing"],
      "source": {
        "uri": "file:///src/config.rs",
        "repo": "memd",
        "commit": "abc123"
      }
    }
  },
  "id": 2
}
EOF
```

Expected response:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "chunk_id": "01932abc-def0-7123-4567-89abcdef0123",
    "status": "indexed"
  },
  "id": 2
}
```

### 4. Search for Memories

```bash
cat << 'EOF' | ./target/release/memd
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "memory.search",
    "arguments": {
      "tenant_id": "quickstart",
      "query": "how to parse configuration files",
      "k": 5
    }
  },
  "id": 3
}
EOF
```

Expected response:

```json
{
  "jsonrpc": "2.0",
  "result": [
    {
      "chunk_id": "01932abc-def0-7123-4567-89abcdef0123",
      "text": "The parseConfig function reads TOML...",
      "score": 0.89,
      "chunk_type": "code",
      "tags": ["rust", "config", "toml", "parsing"],
      "source": {
        "uri": "file:///src/config.rs",
        "repo": "memd",
        "commit": "abc123"
      }
    }
  ],
  "id": 3
}
```

## MCP Integration

### Claude Code Integration

Add to your Claude Code MCP configuration (`~/.config/claude/mcp.json`):

```json
{
  "mcpServers": {
    "memd": {
      "command": "/path/to/memd/target/release/memd",
      "args": [],
      "env": {}
    }
  }
}
```

Restart Claude Code, and you'll see memd tools available:

```
Available tools:
- memory.add
- memory.search
- memory.get
- memory.delete
- memory.stats
- code.find_definition
- code.find_references
- debug.find_tool_calls
... (13 tools total)
```

### Codex CLI Integration

Add to your Codex MCP configuration (`~/.codex/mcp-servers.json`):

```json
{
  "memd": {
    "command": "/path/to/memd/target/release/memd",
    "type": "stdio"
  }
}
```

Test the integration:

```bash
codex --mcp-server memd "Search my memories for authentication code"
```

## Basic Operations

### Adding Different Chunk Types

#### Code Snippet

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "my-project",
      "text": "async fn process_request(req: Request) -> Result<Response> { ... }",
      "chunk_type": "code",
      "tags": ["rust", "async", "http"],
      "source": {
        "uri": "file:///src/handlers.rs",
        "line_start": 45,
        "line_end": 67
      }
    }
  }
}
```

#### Documentation

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "my-project",
      "text": "API Endpoint: POST /api/users - Creates a new user account. Requires authentication token in Authorization header.",
      "chunk_type": "doc",
      "tags": ["api", "users", "authentication"]
    }
  }
}
```

#### Decision Record

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "my-project",
      "text": "Decision: Use SQLite for metadata storage instead of PostgreSQL. Rationale: Simplifies deployment, no external dependencies, sufficient performance for local daemon.",
      "chunk_type": "decision",
      "tags": ["architecture", "database", "sqlite"]
    }
  }
}
```

#### Trace / Debug Log

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "my-project",
      "text": "Error: Failed to parse config at line 45: Invalid TOML syntax\nStack trace:\n  at parseConfig (config.rs:45)\n  at main (main.rs:12)",
      "chunk_type": "trace",
      "tags": ["error", "config", "parsing"]
    }
  }
}
```

### Batch Insert (Efficient)

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.add_batch",
    "arguments": {
      "tenant_id": "my-project",
      "chunks": [
        {
          "text": "Function A does X",
          "chunk_type": "code",
          "tags": ["function-a"]
        },
        {
          "text": "Function B does Y",
          "chunk_type": "code",
          "tags": ["function-b"]
        }
      ]
    }
  }
}
```

### Search with Filters

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.search",
    "arguments": {
      "tenant_id": "my-project",
      "query": "authentication error handling",
      "k": 10,
      "filter": {
        "chunk_type": "code",
        "tags": ["authentication"],
        "project_id": "backend-api"
      }
    }
  }
}
```

### Get Specific Chunk

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.get",
    "arguments": {
      "tenant_id": "my-project",
      "chunk_id": "01932abc-def0-7123-4567-89abcdef0123"
    }
  }
}
```

### Delete Chunk

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.delete",
    "arguments": {
      "tenant_id": "my-project",
      "chunk_id": "01932abc-def0-7123-4567-89abcdef0123"
    }
  }
}
```

### Check Statistics

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.stats",
    "arguments": {
      "tenant_id": "my-project"
    }
  }
}
```

Response:

```json
{
  "total_chunks": 1543,
  "total_size_bytes": 4829384,
  "segments": 3,
  "index_stats": {
    "hnsw_size": 1543,
    "bm25_docs": 1543,
    "cache_hit_rate": 0.82
  }
}
```

## Advanced Features

### Structural Code Queries

#### Find Function Definition

```json
{
  "method": "tools/call",
  "params": {
    "name": "code.find_definition",
    "arguments": {
      "tenant_id": "my-project",
      "symbol_name": "parseConfig",
      "symbol_kind": "function"
    }
  }
}
```

#### Find Callers

```json
{
  "method": "tools/call",
  "params": {
    "name": "code.find_callers",
    "arguments": {
      "tenant_id": "my-project",
      "function_name": "parseConfig",
      "depth": 2
    }
  }
}
```

Response shows multi-hop call chain:

```json
{
  "callers": [
    {
      "name": "main",
      "file": "src/main.rs",
      "line": 12,
      "hops": 1
    },
    {
      "name": "init_app",
      "file": "src/app.rs",
      "line": 45,
      "hops": 1
    },
    {
      "name": "run_server",
      "file": "src/server.rs",
      "line": 23,
      "hops": 2
    }
  ]
}
```

#### Find Imports

```json
{
  "method": "tools/call",
  "params": {
    "name": "code.find_imports",
    "arguments": {
      "tenant_id": "my-project",
      "module_name": "config"
    }
  }
}
```

### Debug Trace Queries

#### Find Tool Calls

```json
{
  "method": "tools/call",
  "params": {
    "name": "debug.find_tool_calls",
    "arguments": {
      "tenant_id": "my-project",
      "tool_name": "memory.add",
      "time_range": {
        "start": "2026-01-31T00:00:00Z",
        "end": "2026-01-31T23:59:59Z"
      }
    }
  }
}
```

#### Find Errors

```json
{
  "method": "tools/call",
  "params": {
    "name": "debug.find_errors",
    "arguments": {
      "tenant_id": "my-project",
      "error_pattern": "parse.*config",
      "limit": 10
    }
  }
}
```

### Performance Metrics

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.metrics",
    "arguments": {
      "tenant_id": "my-project"
    }
  }
}
```

Response:

```json
{
  "query_stats": {
    "total_queries": 1247,
    "avg_latency_ms": 45.3,
    "p50_latency_ms": 32.1,
    "p90_latency_ms": 87.4,
    "p99_latency_ms": 234.5
  },
  "cache_stats": {
    "hit_rate": 0.82,
    "hot_tier_size": 842,
    "semantic_cache_entries": 156
  },
  "index_stats": {
    "hnsw_nodes": 1543,
    "bm25_docs": 1543,
    "memory_usage_mb": 124.5
  }
}
```

## Performance Tuning

### Configuration File

Create `~/.config/memd/config.toml`:

```toml
[storage]
data_dir = "~/.local/share/memd"

[embeddings]
model = "all-MiniLM-L6-v2"
dimension = 384
pooling_strategy = "mean"

[index]
# HNSW parameters
hnsw_m = 16                    # Higher = better recall, more memory
hnsw_ef_construction = 200     # Higher = better quality, slower build
hnsw_ef_search = 50            # Higher = better recall, slower search

[cache]
# Hot tier configuration
hot_tier_size = 1000           # Number of chunks in hot tier
semantic_cache_ttl = 2700      # 45 minutes

# Promotion thresholds
promotion_frequency = 5         # Access 5+ times
promotion_recency_weight = 0.3  # Recency vs frequency balance

[compaction]
# Auto-compaction triggers
tombstone_threshold = 0.20      # Compact at 20% deleted
segment_threshold = 10          # Merge when 10+ segments
hnsw_staleness_threshold = 0.15 # Rebuild at 15% stale

# Throttling
batch_delay_ms = 10
batch_size = 100
```

### Memory Usage Optimization

For **low memory** environments (< 4GB):

```toml
[index]
hnsw_m = 8                     # Reduce from 16
hnsw_ef_construction = 100     # Reduce from 200

[cache]
hot_tier_size = 500            # Reduce from 1000
```

For **high performance** (8GB+):

```toml
[index]
hnsw_m = 32                    # Increase from 16
hnsw_ef_construction = 400     # Increase from 200
hnsw_ef_search = 100           # Increase from 50

[cache]
hot_tier_size = 5000           # Increase from 1000
```

### Manual Compaction

Trigger compaction manually:

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.compact",
    "arguments": {
      "tenant_id": "my-project",
      "force": true
    }
  }
}
```

## Troubleshooting

### Server Won't Start

**Check logs:**

```bash
RUST_LOG=debug ./target/release/memd 2> memd.log
cat memd.log
```

**Common issues:**

1. **Port already in use**: Check if another instance is running
   ```bash
   ps aux | grep memd
   kill <pid>
   ```

2. **Permission denied on data directory**:
   ```bash
   chmod 700 ~/.local/share/memd
   ```

3. **Configuration error**:
   ```bash
   # Validate config
   toml-check ~/.config/memd/config.toml
   ```

### Search Returns No Results

**Debug checklist:**

1. **Verify chunks exist:**
   ```json
   {"method": "tools/call", "params": {"name": "memory.stats", "arguments": {"tenant_id": "my-project"}}}
   ```

2. **Check tenant_id matches:**
   - Adding with `tenant_id: "project-a"`
   - Searching with `tenant_id: "project-a"` (must match exactly)

3. **Increase k parameter:**
   ```json
   {"arguments": {"k": 100}}
   ```

4. **Test with exact text match:**
   Search for exact phrase from a known chunk

### Slow Search Performance

**Diagnostics:**

```json
{
  "method": "tools/call",
  "params": {
    "name": "memory.metrics",
    "arguments": {"tenant_id": "my-project"}
  }
}
```

**Check:**

1. **Cache hit rate < 50%**: Increase `hot_tier_size`
2. **p99 latency > 500ms**: Run compaction or increase `hnsw_ef_search`
3. **High memory usage**: Reduce `hnsw_m` or `hot_tier_size`

**Optimization:**

```bash
# Force compaction
# (sends JSON request for memory.compact)

# Restart server to rebuild HNSW
kill <pid>
./target/release/memd
```

### MCP Integration Not Working

**Claude Code:**

1. Check MCP config syntax:
   ```bash
   jq . ~/.config/claude/mcp.json
   ```

2. Verify binary path:
   ```bash
   ls -l /path/to/memd/target/release/memd
   ```

3. Test binary directly:
   ```bash
   echo '{"jsonrpc":"2.0","method":"tools/list","id":1}' | ./target/release/memd
   ```

**Codex CLI:**

1. List MCP servers:
   ```bash
   codex --list-mcp-servers
   ```

2. Test specific server:
   ```bash
   codex --mcp-server memd --test
   ```

### Embedding Errors

**Model download failed:**

```bash
# Check internet connection
ping huggingface.co

# Verify model cache
ls ~/.cache/huggingface/hub/

# Clear cache and retry
rm -rf ~/.cache/huggingface/
```

**Out of memory:**

```toml
# Use smaller model
[embeddings]
model = "all-MiniLM-L6-v2"  # 384 dim instead of 1024
```

## Next Steps

### Run Evaluations

Test memd with benchmark datasets:

```bash
# Quick sanity check (should get 100% recall)
cargo run --release --bin memd-evals -- --suite sanity

# Scientific papers dataset
cargo run --release --bin memd-evals -- --suite scifact

# Code search dataset
cargo run --release --bin memd-evals -- --suite codesearchnet

# Full evaluation suite (takes ~30 min)
cargo run --release --bin memd-evals -- --suite all
```

### Integrate with Your Agent Workflow

See [examples/](examples/) for:
- Claude Code integration patterns
- Codex CLI workflows
- Batch ingestion scripts
- Custom MCP clients

### Production Deployment

1. **Build optimized binary:**
   ```bash
   cargo build --release --target x86_64-unknown-linux-musl
   ```

2. **Create systemd service:**
   ```ini
   [Unit]
   Description=memd - Agent Memory Service
   After=network.target

   [Service]
   Type=simple
   User=youruser
   ExecStart=/usr/local/bin/memd
   Restart=on-failure

   [Install]
   WantedBy=multi-user.target
   ```

3. **Configure monitoring:**
   - Query `memory.metrics` periodically
   - Alert on p99 latency > 1000ms
   - Monitor disk usage in data_dir

## Support

- **Documentation**: See [README.md](README.md) for architecture overview
- **Issues**: File bugs at GitHub Issues
- **Planning Docs**: `.planning/` contains detailed development history

## Appendix: Complete Tool Reference

### memory.add

**Required:**
- `tenant_id` (string)
- `text` (string)

**Optional:**
- `chunk_type` (code|doc|trace|decision|plan)
- `tags` (string[])
- `project_id` (string)
- `source` (object with uri, repo, commit, etc.)

### memory.search

**Required:**
- `tenant_id` (string)
- `query` (string)

**Optional:**
- `k` (number, default: 10)
- `filter` (object with chunk_type, tags, project_id)

### memory.get

**Required:**
- `tenant_id` (string)
- `chunk_id` (string)

### memory.delete

**Required:**
- `tenant_id` (string)
- `chunk_id` (string)

### memory.stats

**Required:**
- `tenant_id` (string)

### code.find_definition

**Required:**
- `tenant_id` (string)
- `symbol_name` (string)

**Optional:**
- `symbol_kind` (function|class|interface|type|enum|variable)

### code.find_callers

**Required:**
- `tenant_id` (string)
- `function_name` (string)

**Optional:**
- `depth` (number, 1-3, default: 1)

---

**Ready to build intelligent agent memory!** 🚀
