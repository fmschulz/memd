# Architecture

`memd` is a single Rust binary that owns local storage, hybrid retrieval, and a
shared operation surface used by direct CLI commands, `memd call`, and the
warm/batch execution modes.

```mermaid
flowchart TB
  subgraph clients["Clients"]
    direction LR
    ca["Coding agent"]
    sci["AI scientist"]
    human["Human or controller"]
  end

  subgraph cli_flow["Skill and CLI workflow"]
    direction LR
    agent_context["memd agent-context"]
    search_add["memd search / memd add"]
  end

  subgraph core["memd core"]
    direction TB
    cli_call["memd call"]
    handlers["Memory, task, artifact, code, context, and debug operations"]
    metrics["Metrics and cache statistics"]
  end

  subgraph retrieval["Hybrid retrieval"]
    direction LR
    hybrid["Hybrid searcher"]
    dense["Dense HNSW search"]
    sparse["BM25 sparse search"]
    tiered["Hot tier and semantic cache"]
    rerank["Optional rerankers"]
  end

  subgraph storage["Persistent store"]
    direction LR
    sqlite["SQLite metadata"]
    segments["Segment files"]
    wal["Write-ahead log"]
    structural["Structural code index"]
  end

  subgraph disk["On-disk layout"]
    direction LR
    db[("metadata.db")]
    segment_files[("tenant segments")]
    wal_file[("tenant WAL")]
    sparse_index[("sparse index")]
    warm_index[("warm index")]
  end

  ca --> agent_context
  sci --> agent_context
  human --> search_add
  human --> cli_call
  agent_context --> handlers
  search_add --> handlers
  cli_call --> handlers
  cli_call --> metrics
  handlers --> hybrid
  handlers --> sqlite
  handlers --> structural
  hybrid --> dense
  hybrid --> sparse
  hybrid --> tiered
  hybrid --> rerank
  dense --> warm_index
  sparse --> sparse_index
  sqlite --> db
  handlers --> segments
  segments --> segment_files
  segments --> wal
  wal --> wal_file
  structural --> db
```

The runtime stack is designed so direct CLI commands and local operation calls
share the same storage, retrieval, and artifact machinery. SQLite is accessed
through a bounded connection pool under WAL-mode locking, and HNSW rebuilds
swap atomically without blocking readers.

## Hybrid retrieval lanes

| Lane | Role | Notes |
| --- | --- | --- |
| Dense (HNSW) | semantic recall | `mapping.bin` (bincode-packed), graph dump optional |
| Sparse (BM25) | lexical recall | tantivy index, `open_or_create` |
| Hot tier | recency boost | bounded LRU on top of segment store |
| Semantic cache | repeat-query short-circuit | TTL-bounded, query-hash keyed |
| Optional reranker | precision lift | feature-based by default; ONNX cross-encoder and MemReranker-4B opt-in |

See [Optional rerankers](reranking.md) for the cross-encoder and MemReranker-4B
paths, [Data layout](data-layout.md) for on-disk structure, and
[Trust boundary](trust-boundary.md) for what each surface's results commit to.
