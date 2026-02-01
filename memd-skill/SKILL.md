# memd - Intelligent Memory for AI Agents

**Persistent, searchable memory daemon for AI coding agents using hybrid retrieval with hot/warm/cold tiering.**

## When to Use

Use this skill when you need to:
- Add persistent memory to AI agents across sessions
- Search past conversations, code patterns, or decisions
- Retrieve relevant context without hitting token limits
- Index and search code repositories semantically
- Track tool calls, errors, and debugging sessions
- Find symbol definitions, call graphs, and imports

## What memd Provides

**13 MCP Tools:**
- `memory.add` / `memory.add_batch` - Store information
- `memory.search` - Hybrid semantic + keyword search
- `memory.get` / `memory.delete` - CRUD operations
- `code.find_definition` / `code.find_references` - Structural queries
- `code.find_callers` / `code.find_imports` - Code navigation
- `debug.find_tool_calls` / `debug.find_errors` - Debug traces
- `memory.stats` / `memory.metrics` / `memory.compact` - System management

**Architecture:**
- Hybrid retrieval: Dense HNSW + Sparse BM25 + Structural indexes
- Three-tier caching: Hot (LRU) → Warm (HNSW+BM25) → Cold (segments)
- CPU-only, offline-first, no GPU required
- Pure Rust implementation with MCP protocol integration

## Prerequisites

- Linux (x64)
- 4GB RAM minimum (8GB recommended)
- AI agent with MCP support (Claude Code, Codex CLI)
- curl (for automated installation)

## Setup Instructions

### 1. Install memd

**Automated Installation (Recommended):**

```bash
curl -sSL https://raw.githubusercontent.com/fmschulz/memd/main/install.sh | bash
```

The install script will:
- Download and install the memd binary to `~/.local/bin/memd`
- Create default configuration at `~/.config/memd/config.toml`
- Prompt to configure MCP for Claude Code and/or Codex CLI
- Verify the installation

**Manual Installation:**

```bash
# Download binary from releases
curl -sSL https://github.com/fmschulz/memd/releases/latest/download/memd-linux-x64 -o ~/.local/bin/memd
chmod +x ~/.local/bin/memd

# Ensure ~/.local/bin is in PATH
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc

# Verify installation
memd --version
```

### 2. Create Configuration

memd uses TOML configuration at `~/.config/memd/config.toml`:

```bash
mkdir -p ~/.config/memd

cat > ~/.config/memd/config.toml <<'EOF'
[server]
mode = "mcp"  # or "cli"

[storage]
data_dir = "~/.local/share/memd"

[embeddings]
model = "all-MiniLM-L6-v2"
dimension = 384
pooling_strategy = "mean"

[index]
hnsw_m = 16
hnsw_ef_construction = 200
hnsw_ef_search = 50

[cache]
hot_tier_size = 1000
semantic_cache_ttl = 2700  # 45 minutes

[compaction]
tombstone_threshold = 0.20
segment_threshold = 10
hnsw_staleness_threshold = 0.15
EOF
```

### 3. Configure AI Agent (Claude Code)

Add memd to your MCP configuration:

```bash
# Edit Claude Code MCP config
cat >> ~/.config/claude/mcp_settings.json <<'EOF'
{
  "mcpServers": {
    "memd": {
      "command": "memd",
      "args": [],
      "env": {},
      "disabled": false
    }
  }
}
EOF
```

**Important:** Restart Claude Code after adding MCP server configuration.

### 4. Configure AI Agent (Codex CLI)

For Codex CLI, add to MCP configuration:

```bash
# Edit Codex MCP config
cat >> ~/.codex/mcp_config.json <<'EOF'
{
  "servers": {
    "memd": {
      "command": "memd",
      "args": [],
      "env": {}
    }
  }
}
EOF
```

### 5. Verify Installation

Test memd is accessible:

```bash
# Run memd directly (should start MCP server on stdio)
memd

# In another terminal, check data directory was created
ls -la ~/.local/share/memd
```

You should see memd waiting for MCP input on stdin. Press Ctrl+C to exit.

## Usage Patterns

### Basic Memory Operations

**Add a memory:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "my-project",
      "text": "Function parseConfig reads TOML files and returns Config struct",
      "chunk_type": "code",
      "tags": ["rust", "config", "parsing"]
    }
  },
  "id": 1
}
```

**Search for memories:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "memory.search",
    "arguments": {
      "tenant_id": "my-project",
      "query": "how to parse configuration",
      "k": 10
    }
  },
  "id": 2
}
```

### Tenant Isolation

Use `tenant_id` to isolate memories by project:

```bash
# Project A memories
tenant_id: "project-alpha"

# Project B memories
tenant_id: "project-beta"

# Personal notes
tenant_id: "personal"
```

**Critical:** Tenants are strictly isolated - no cross-tenant data leakage.

### Batch Insert

For bulk indexing (repositories, documentation):

```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "memory.add_batch",
    "arguments": {
      "tenant_id": "my-project",
      "chunks": [
        {
          "text": "Database schema uses PostgreSQL with JSONB columns",
          "chunk_type": "doc",
          "tags": ["database", "postgres"]
        },
        {
          "text": "API endpoints follow REST conventions with /api/v1 prefix",
          "chunk_type": "code",
          "tags": ["api", "rest"]
        }
      ]
    }
  },
  "id": 3
}
```

### Code Navigation

**Find function definition:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "code.find_definition",
    "arguments": {
      "tenant_id": "rust-project",
      "symbol_name": "HnswIndex"
    }
  },
  "id": 4
}
```

**Find all callers of a function:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "code.find_callers",
    "arguments": {
      "tenant_id": "rust-project",
      "function_name": "insert_batch",
      "max_depth": 3
    }
  },
  "id": 5
}
```

### Debug Traces

**Search past tool invocations:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "debug.find_tool_calls",
    "arguments": {
      "tenant_id": "debug-session",
      "tool_pattern": "git.*",
      "limit": 20
    }
  },
  "id": 6
}
```

**Find error patterns:**
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "debug.find_errors",
    "arguments": {
      "tenant_id": "debug-session",
      "error_pattern": "NullPointerException",
      "k": 10
    }
  },
  "id": 7
}
```

## Agent Integration Examples

### Claude Code Pattern

```markdown
User: "Remember that we decided to use PostgreSQL with JSONB for the user profiles table"

Claude: I'll store this decision in memory.

<uses memory.add tool>
{
  "tenant_id": "ecommerce-app",
  "text": "Architecture decision: Use PostgreSQL with JSONB columns for user profiles table to support flexible schema evolution",
  "chunk_type": "decision",
  "tags": ["architecture", "database", "postgres", "user-profiles"]
}

User: "How did we decide to store user profiles?"

Claude: Let me search memory...

<uses memory.search tool>
{
  "tenant_id": "ecommerce-app",
  "query": "user profiles storage decision",
  "k": 5
}

Result: "Architecture decision: Use PostgreSQL with JSONB columns for user profiles table..."
```

### Codex CLI Pattern

```bash
# Index a codebase
codex -p "Index this repository into memory as 'my-rust-app'"

# Search for patterns
codex -p "Find all async functions in memory for 'my-rust-app'"

# Code navigation
codex -p "Show me the call graph for the parse_config function"
```

## Performance Characteristics

Based on comprehensive benchmarks (January 31, 2026):

**Retrieval Quality (Recall@10):**
- Code Retrieval: 1.000 (Perfect)
- Semantic Search: 0.867 (Excellent)
- Hybrid Search: 0.833 (Exceeds target)
- Keyword Queries: 0.875 (Good)

**Latency:**
- Warm Tier p50: 99.5ms
- Warm Tier p90: 113.3ms
- Warm Tier p99: 130.1ms
- Embedding p50: 10-23ms (CPU-only, all-MiniLM-L6-v2)

**Capacity:**
- ~16MB per 10K chunks (384-dim embeddings)
- ~500MB per 100K chunks
- Reload time: ~2-5s for 10K chunks (HNSW rebuild)

## Common Workflows

### 1. Session Context Tracking

```bash
# At session start
memory.add: "Starting work on user authentication feature"

# During work
memory.add: "Implemented JWT token validation in auth.rs:245"
memory.add: "Bug fix: handle expired tokens with 401 response"

# At session end
memory.add: "Completed auth feature, all tests passing, ready for review"

# Later session
memory.search: "authentication implementation status"
```

### 2. Codebase Indexing

```bash
# Batch index all Rust files
find . -name "*.rs" -exec memd-index {} \;

# Search code semantically
memory.search: "functions that handle database connections"

# Navigate code structure
code.find_definition: "DatabasePool"
code.find_callers: "connect_to_db"
```

### 3. Decision Tracking

```bash
# Record architectural decisions
memory.add:
  text: "ADR-001: Use Redis for session storage instead of database to reduce latency"
  tags: ["architecture", "redis", "sessions", "adr"]

# Search decisions later
memory.search: "why did we choose redis"
```

### 4. Debug Session Persistence

```bash
# Record debugging context
debug.find_errors: "database connection timeout"
memory.add: "Bug: Connection pool exhaustion after 1000 requests - fixed by increasing pool size to 50"

# Next debugging session
memory.search: "connection pool issues"
debug.find_tool_calls: "database.*"
```

## Troubleshooting

### memd Not Starting

**Symptom:** `memd: command not found`

**Solution:**
```bash
# Check if binary exists
ls -la ~/.local/bin/memd

# Check PATH
echo $PATH | grep "$HOME/.local/bin"

# Add to PATH if missing
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### MCP Connection Failed

**Symptom:** Agent can't connect to memd

**Solution:**
1. Verify memd runs standalone: `memd` (should wait for input)
2. Check MCP config syntax: `cat ~/.config/claude/mcp_settings.json`
3. Restart agent after config changes
4. Check logs: `~/.local/share/memd/memd.log`

### Low Search Quality

**Symptom:** Search results not relevant

**Solution:**
1. Use more specific queries (not single words)
2. Add tags to chunks for better filtering
3. Use hybrid search (default) for best results
4. Check tenant_id isolation (searching correct tenant)
5. Verify embeddings are working: `memory.stats` should show indexed chunks

### Slow Queries

**Symptom:** Search takes >500ms

**Solution:**
1. Check index size: `memory.stats` (large indexes slower)
2. Reduce `k` parameter (fewer results faster)
3. Tune HNSW ef_search in config (lower=faster, higher=better quality)
4. Use hot tier for frequent queries (automatic promotion)
5. Run compaction: `memory.compact` (clean up tombstones)

### Out of Memory

**Symptom:** memd crashes or system slows down

**Solution:**
1. Check index size: `memory.stats`
2. Reduce `hot_tier_size` in config
3. Increase system RAM or reduce chunk count
4. Use compaction to reclaim space: `memory.compact`

## Configuration Tuning

### For Code Search (Precision)

```toml
[index]
hnsw_m = 32                    # More connections
hnsw_ef_construction = 400     # Better graph quality
hnsw_ef_search = 100           # Higher search quality

[embeddings]
model = "all-MiniLM-L6-v2"     # Good for code
```

### For Speed (Latency)

```toml
[index]
hnsw_m = 8                     # Fewer connections
hnsw_ef_construction = 100     # Faster indexing
hnsw_ef_search = 20            # Faster search

[cache]
hot_tier_size = 5000           # Larger hot tier
```

### For Memory Efficiency

```toml
[cache]
hot_tier_size = 100            # Small hot tier
semantic_cache_ttl = 900       # Shorter TTL (15 min)

[compaction]
tombstone_threshold = 0.10     # Compact more aggressively
```

## Advanced Usage

### Multi-Tenant Setup

Isolate memories by context:

```bash
# Work projects
tenant_id: "acme-corp-backend"
tenant_id: "acme-corp-frontend"

# Personal projects
tenant_id: "personal-blog"
tenant_id: "learning-rust"

# Experimentation
tenant_id: "sandbox"
```

### Tag Strategies

Effective tagging improves retrieval:

```bash
# By language
tags: ["rust", "python", "typescript"]

# By domain
tags: ["auth", "database", "api"]

# By type
tags: ["bug-fix", "feature", "refactor"]

# By status
tags: ["completed", "in-progress", "blocked"]

# Combined
tags: ["rust", "database", "bug-fix", "completed"]
```

### Batch Indexing Scripts

Index entire repositories:

```bash
#!/bin/bash
# index-repo.sh

TENANT_ID="my-project"

# Find all source files
find . -type f \( -name "*.rs" -o -name "*.py" -o -name "*.ts" \) | while read file; do
  # Extract file content
  content=$(cat "$file")

  # Add to memd via MCP (pseudo-code)
  memd-cli add --tenant "$TENANT_ID" \
    --text "$content" \
    --chunk-type "code" \
    --tags "$(basename $file | cut -d. -f2)"
done
```

## Best Practices

1. **Consistent Tenant IDs** - Use project names or UUIDs, not random strings
2. **Descriptive Tags** - 3-5 tags per chunk, include language + domain + type
3. **Chunk Granularity** - Functions/classes work well, avoid very large chunks
4. **Regular Compaction** - Run `memory.compact` weekly for large indexes
5. **Monitor Stats** - Check `memory.stats` periodically to track growth
6. **Semantic Queries** - Write queries as questions, not keywords ("how to handle errors" > "error handling")

## Integration with Skills

### With /commit

```bash
# Before committing, search for similar past work
memory.search: "how did we handle database migrations"

# After commit, record decision
memory.add: "Implemented database migration using Alembic auto-generation"
```

### With /plan

```bash
# Search for architectural patterns
memory.search: "api error handling patterns we use"

# Store planning decisions
memory.add: "Plan: Implement rate limiting using Redis with 100 req/min per user"
```

### With /codex-review

```bash
# Search for past code review findings
memory.search: "common security issues in auth code"

# Store review outcomes
memory.add: "Code review: Fixed SQL injection in user query endpoint"
```

## Reference

- **Documentation**: See README.md and QUICKSTART.md in repository
- **Repository**: https://github.com/fmschulz/memd
- **Issues**: https://github.com/fmschulz/memd/issues
- **Contact**: fmschulz@gmail.com

## Version

**Current Release**: v0.1.0 (January 31, 2026)

**Status**: Milestone 1 Complete - Production-ready hybrid retrieval system

---

**Quick Start Summary:**
1. Install: `cargo build --release && cp target/release/memd ~/.local/bin/`
2. Configure: Create `~/.config/memd/config.toml`
3. Add to agent: Update `~/.config/claude/mcp_settings.json`
4. Restart agent
5. Use `memory.add` and `memory.search` tools
