# memd - Intelligent Memory for AI Agents

> A local daemon that provides persistent, searchable memory for AI coding agents through hybrid retrieval with hot/warm/cold tiering.

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

- Rust 1.75+ ([install](https://rustup.rs/))
- Linux or macOS
- 4GB RAM minimum (8GB recommended)

### Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/memd.git
cd memd

# Build release binary
cargo build --release

# Run the server
./target/release/memd
```

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

Based on comprehensive benchmarks (Suites A-F):

### Retrieval Quality

- **Semantic Search**: Recall@10 > 0.7 (scifact dataset)
- **Keyword Search**: Recall@10 > 0.9 (exact match)
- **Hybrid Search**: Recall@10 > 0.75 (mixed queries)
- **Structural Queries**: 80% accuracy (definitions/imports)

### Latency

- **Hot Tier**: p50 < 10ms
- **Warm Tier (Hybrid)**: p50 < 100ms, p99 < 500ms
- **Cache Hit**: p50 < 5ms
- **Embedding**: ~10-50ms per query (CPU, model-dependent)

### Throughput

- **Ingestion**: ~1000 chunks/sec (batch insert)
- **Query**: 100-250 req/s (with hot tier)
- **Compaction**: Background with throttling (<10ms latency impact)

## Development

### Running Tests

```bash
# Unit tests
cargo test

# Integration tests
cargo test --test integration

# Run evaluation suite
cargo run --release --bin memd-evals -- --suite all
```

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

See [STATE.md](.planning/STATE.md) for 235+ detailed decisions logged during development.

## Implementation Status

**Milestone 1 Complete** - All 7 phases executed (45 plans, ~5 hours total):

- ✅ Phase 1: Skeleton + MCP Server
- ✅ Phase 2: Persistent Cold Store
- ✅ Phase 3: Dense Warm Index
- ✅ Phase 4: Sparse Lexical + Fusion
- ✅ Phase 4.1: Pooling Strategy Support
- ✅ Phase 5: Hot Tier + Cache
- ✅ Phase 6: Structural Indexes
- ✅ Phase 7: Compaction + Cleanup

See [ROADMAP.md](.planning/ROADMAP.md) for detailed phase breakdown and success criteria.

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

See [PROJECT.md](.planning/PROJECT.md) for out-of-scope features.

## Documentation

- **[QUICKSTART.md](QUICKSTART.md)** - Getting started guide with examples
- **[docs/implementation_v0.md](docs/implementation_v0.md)** - Original technical specification
- **[.planning/ROADMAP.md](.planning/ROADMAP.md)** - Development roadmap and phases
- **[.planning/STATE.md](.planning/STATE.md)** - Current status and decision log
- **[.planning/REQUIREMENTS.md](.planning/REQUIREMENTS.md)** - Requirements specification

## Contributing

This is currently a personal project under active development. Contributions welcome after v1.0 release.

### Development Workflow

1. Read [.planning/PROJECT.md](.planning/PROJECT.md) for context
2. Check [.planning/STATE.md](.planning/STATE.md) for current status
3. Follow existing code patterns (trait-based abstractions)
4. Add tests for new features
5. Run full evaluation suite before submitting

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

- **Issues**: File bugs and feature requests in GitHub Issues
- **Documentation**: See `docs/` directory for technical details
- **Planning Artifacts**: `.planning/` contains development history and decisions

---

**Status**: Milestone 1 Complete (v0.1.0) - Architecture A baseline with hybrid retrieval ready for production testing.
