# memd - Intelligent Memory for AI Agents

> A local daemon that provides persistent, searchable memory for AI coding agents through hybrid retrieval with hot/warm/cold tiering.

[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)](https://github.com/fmschulz/memd/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust Version](https://img.shields.io/badge/rust-1.75%2B-blue.svg)](https://www.rust-lang.org)

## Overview

**memd** (Memory Daemon) enables AI coding agents like Claude Code and Codex CLI to find and use relevant past context—across sessions, projects, and time—without hitting context window limits or losing continuity.

### Key Features

- **Hybrid Retrieval**: Dense vector (HNSW) + sparse lexical (BM25) + structural code indexes
- **Three-Tier Architecture**: Hot cache → Warm index → Cold storage with automatic promotion/demotion
- **Structural Code Awareness**: AST parsing, symbol tables, call graphs, and trace indexing for 6+ languages
- **MCP Integration**: Standard Model Context Protocol for seamless agent integration
- **Multi-Tenant Isolation**: Strict partitioning with no cross-tenant data leakage
- **CPU-Only**: Pure Rust implementation with no GPU dependency
- **Offline-First**: Runs entirely locally with no external service dependencies

## Quick Start

### Prerequisites

- Linux (x64)
- 4GB RAM minimum (8GB recommended)
- curl (for installation)

### Installation

**Option 1: Automated Install Script (Recommended)**

```bash
curl -sSL https://raw.githubusercontent.com/fmschulz/memd/main/install.sh | bash
```

This will:
- Download the latest binary to `~/.local/bin/memd`
- Create default configuration at `~/.config/memd/config.toml`
- Optionally configure MCP for Claude Code and/or Codex CLI
- Verify the installation

**Option 2: Manual Installation**

Download the binary from [releases](https://github.com/fmschulz/memd/releases):

```bash
# Download latest release
curl -sSL https://github.com/fmschulz/memd/releases/latest/download/memd-linux-x64 -o ~/.local/bin/memd
chmod +x ~/.local/bin/memd

# Create config directory
mkdir -p ~/.config/memd

# Run memd
memd
```

**Option 3: Build from Source**

```bash
# Prerequisites: Rust 1.75+ (https://rustup.rs/)
git clone https://github.com/fmschulz/memd.git
cd memd
```

**Ubuntu/Debian:**

```bash
# Install dependencies
sudo apt update
sudo apt install build-essential pkg-config libssl-dev

# Build
cargo build --release
cp target/release/memd ~/.local/bin/
```

**Arch Linux:**

```bash
# Install dependencies
sudo pacman -S base-devel openssl lld

# Build (Arch Rust uses lld linker - requires explicit gcc)
CC=/usr/bin/gcc cargo build --release
cp target/release/memd ~/.local/bin/
```

**Build Troubleshooting:**

- Linker errors with `-m64` or `-fuse-ld=`: Use `CC=/usr/bin/gcc cargo build --release`
- Clear sccache if experiencing stale builds: `sccache --stop-server && cargo clean`

The server will start and listen for MCP requests on stdin/stdout.

### Basic Usage

Connect your AI agent via MCP and start using memory tools:

```json
# Add a memory chunk
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

# Search for relevant memories
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

See [QUICKSTART.md](QUICKSTART.md) for detailed setup instructions and examples.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                    AI Agent                         │
│            (Claude Code / Codex CLI)                │
└──────────────────────┬──────────────────────────────┘
                       │ MCP Protocol (stdio)
┌──────────────────────▼──────────────────────────────┐
│                  memd Server                        │
├─────────────────────────────────────────────────────┤
│  Query Router (Intent Classification)              │
│    ↓                    ↓                    ↓      │
│  Semantic Cache    Hot Tier (LRU)    Structural    │
│    ↓                    ↓                    ↓      │
│  Warm Index         Dense HNSW      Symbol Table   │
│  (Hybrid Search)    + BM25 Sparse   Call Graph     │
│    ↓                    ↓            Trace Index    │
│  Cold Storage (Append-only Segments + WAL)         │
└─────────────────────────────────────────────────────┘
         │                  │                  │
         ▼                  ▼                  ▼
   Persistent         Persistent         Persistent
   Segments           Indexes            Metadata
   (mmap)            (Tantivy)          (SQLite)
```

### Data Flow

1. **Ingestion**: Add → Segment Writer → WAL → Metadata Store → Index (Dense + Sparse + Structural)
2. **Retrieval**: Query → Router → Cache/Hot/Warm → RRF Fusion → Reranker → Context Packer
3. **Tiering**: Access Tracker → Promotion Logic → Hot Tier ↔ Warm Tier

## MCP Tools

memd provides 13 MCP tools for memory operations:

### Core Memory Tools

- `memory.add` - Add a single memory chunk
- `memory.add_batch` - Add multiple chunks efficiently
- `memory.search` - Hybrid semantic + lexical search
- `memory.get` - Retrieve chunk by ID
- `memory.delete` - Soft delete chunks
- `memory.stats` - Get storage and index statistics

### Structural Code Tools

- `code.find_definition` - Find symbol definitions by name
- `code.find_references` - Find all references to a symbol
- `code.find_callers` - Find callers of a function (multi-hop)
- `code.find_imports` - Find import/dependency relationships

### Debug & Trace Tools

- `debug.find_tool_calls` - Search past tool invocations
- `debug.find_errors` - Find stack traces and error patterns

### System Tools

- `memory.metrics` - Get performance metrics
- `memory.compact` - Trigger manual compaction

## Project Structure

```
memd/
├── crates/
│   └── memd/              # Main crate
│       ├── src/
│       │   ├── chunking/      # Text chunking utilities
│       │   ├── compaction/    # Tombstone audit, HNSW rebuild
│       │   ├── embeddings/    # Embedder trait + implementations
│       │   ├── index/         # HNSW, BM25, embedding cache
│       │   ├── mcp/           # MCP server, handlers, tools
│       │   ├── metrics/       # Query latency, index stats
│       │   ├── retrieval/     # Fusion, reranker, packer
│       │   ├── store/         # Storage layer abstractions
│       │   ├── structural/    # AST parsing, symbols, traces
│       │   ├── text/          # Text processing
│       │   ├── tiered/        # Hot tier, semantic cache
│       │   └── types/         # Core types
│       └── tests/         # Integration tests
├── evals/
│   ├── harness/           # Evaluation framework
│   ├── datasets/          # Test datasets
│   └── results/           # Benchmark results
├── docs/                  # Technical documentation
└── .planning/             # Development planning docs
```

## Performance Characteristics

Based on comprehensive benchmarks (January 31, 2026):

### Retrieval Quality (Recall@10)

| Metric | Target | Actual | Status |
|--------|--------|--------|--------|
| Code Retrieval | > 0.80 | 1.000 | Perfect |
| Semantic Search | > 0.70 | 0.867 | Excellent |
| Hybrid Search | > 0.75 | 0.833 | Exceeds target |
| Keyword Queries | > 0.85 | 0.875 | Good |
| Sanity Check | 1.000 | 1.000 | Perfect |

**By Query Type (Hybrid Suite):**

| Type | Recall@10 | MRR | Queries |
|------|-----------|-----|---------|
| Keyword | 0.875 | 1.000 | 4 |
| Semantic | 0.875 | 0.625 | 4 |
| Mixed | 0.750 | 0.750 | 4 |

### Latency Performance

| Metric | Target | Actual | Status |
|--------|--------|--------|--------|
| Warm Tier p50 | < 100ms | 99.5ms | Met |
| Warm Tier p90 | < 500ms | 113.3ms | Exceeded |
| Warm Tier p99 | < 500ms | 130.1ms | Exceeded |

**Embedding Latency (all-MiniLM-L6-v2):**
- True Semantic: 10ms p50, 15ms p99
- CodeSearchNet: 23ms p50, 38ms p99

### Test Coverage

- Unit Tests: 425/435 passing (97.7%)
- HNSW Persistence: 6/6 passing (100%)
- Evaluation Suites: 6/6 passing (100%)

## Development

### Running Tests

```bash
# Unit tests (on Arch Linux, prefix with CC=/usr/bin/gcc)
cargo test

# Integration tests
cargo test --test integration

# Run evaluation suite
cargo run --release --bin memd-evals -- --suite all
```

**Note for Arch Linux users:** Prefix commands with `CC=/usr/bin/gcc` if you encounter linker errors.

### Evaluation Suites

memd includes 6 comprehensive test suites:

- **Suite A**: MCP conformance (protocol compliance)
- **Suite B**: Retrieval quality (semantic, lexical, code search)
- **Suite C**: Hybrid retrieval (fusion, reranking)
- **Suite D**: Tiered search (cache, hot tier promotion)
- **Suite E**: Structural queries (symbols, callers, traces)
- **Suite F**: Compaction (tombstone cleanup, HNSW rebuild)

### Code Quality

```bash
# Format code
cargo fmt

# Run linter
cargo clippy -- -D warnings

# Check types
cargo check
```

## Configuration

memd uses TOML configuration at `~/.config/memd/config.toml`:

```toml
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
```

## Deployment

### System Requirements

- **Memory**: 4GB minimum (scales with index size)
- **Storage**: ~500MB per 100K chunks (depends on chunk size)
- **CPU**: Any modern CPU (no GPU required)

### Production Deployment

1. Build release binary: `cargo build --release`
2. Copy binary to target system
3. Configure via `~/.config/memd/config.toml`
4. Run as daemon or systemd service

### Docker Support

See [DOCKER.md](DOCKER.md) for containerized deployment (if needed for system compatibility).

## Technical Decisions

Key architectural choices:

| Decision | Rationale |
|----------|-----------|
| **Rust** | Memory control, mmap, concurrency, single-binary packaging |
| **MCP Protocol** | Standard agent integration (Claude, Codex ecosystem) |
| **CPU-Only** | No GPU dependency, works offline, universal deployment |
| **HNSW + BM25** | Best-of-both-worlds: semantic + exact keyword matching |
| **Three-Tier** | Hot cache for latency, warm for quality, cold for capacity |
| **Structural Indexes** | Code-aware queries outperform pure embedding search |

All architectural decisions are documented in code comments and design docs.

## Implementation Status

**v0.1.0 - Milestone 1 Complete**

Core features implemented:
- MCP Server with 13 tools
- Persistent storage with crash recovery
- Hybrid retrieval (dense HNSW + sparse BM25)
- Three-tier caching (hot/warm/cold)
- Structural code indexes (6 languages)
- Compaction and cleanup
- Comprehensive evaluation suite (6 test suites)

## Known Limitations

### Current Implementation

- **HNSW Persistence**: Rebuilds on restart (warm startup ~2-5s for 10K chunks)
- **Embedding Backend**: Candle migration in progress (pure Rust, no C++ dependencies)
- **Language Support**: Structural parsing limited to 6 languages (Rust, Python, TypeScript, JavaScript, Go, C/C++)

### Future Enhancements (v2+)

- **Architecture B**: Graph memory module with semantic neighbor edges
- **Learned Router**: Replace regex heuristics with tiny model
- **Cold-Tier Dense**: Binary coarse index + int8 rescoring
- **Additional Languages**: Java, C#, Ruby, etc.

## Documentation

- **[QUICKSTART.md](QUICKSTART.md)** - Getting started guide with examples
- **[docs/implementation_v0.md](docs/implementation_v0.md)** - Technical specification
- **[docs/QWEN3_UPGRADE.md](docs/QWEN3_UPGRADE.md)** - Embedding model upgrade guide

## Contributing

This is currently a personal project under active development. Contributions welcome after v1.0 release.

### Development Workflow

1. Follow existing code patterns (trait-based abstractions)
2. Add tests for new features
3. Run evaluation suite: `cargo test && cargo run --bin memd-evals -- --suite all`
4. Format code: `cargo fmt`
5. Check lints: `cargo clippy -- -D warnings`

## License

MIT License - see [LICENSE](LICENSE) for details

## Acknowledgments

Built with:
- [hnsw_rs](https://github.com/jean-pierreBoth/hnswlib-rs) - HNSW implementation
- [Tantivy](https://github.com/quickwit-oss/tantivy) - Full-text search
- [Candle](https://github.com/huggingface/candle) - ML framework in Rust
- [tree-sitter](https://tree-sitter.github.io/) - Parser framework

Inspired by memory systems in LangChain, LlamaIndex, and agent architectures from OpenAI Codex, Anthropic Claude, and Replit Ghostwriter.

## Support

- **Issues**: File bugs and feature requests at [GitHub Issues](https://github.com/fmschulz/memd/issues)
- **Contact**: fmschulz@gmail.com
- **Documentation**: See `docs/` directory for technical details

---

**Status**: Milestone 1 Complete (v0.1.0) - Architecture A baseline with hybrid retrieval ready for production testing.
