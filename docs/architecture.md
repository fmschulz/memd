# Architecture

`memd` is a single Rust binary that owns local storage, hybrid retrieval, and a
shared operation surface used by the direct CLI commands, `memd call`, and the
warm/batch execution modes.

![memd architecture — clients, CLI surface, hybrid retrieval, persistent store, on-disk layout](figures/architecture.svg)

The figure shows five layers: clients (same machine), the CLI surface plus the
typed operation surface, hybrid retrieval, the persistent store, and the
on-disk layout. The same layers are sketched below as Mermaid for environments
without SVG.

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

## Layers

### Clients

One trusted machine; multiple agent processes. Coding agents (Claude Code,
Codex, others) and AI-scientist workflows speak to `memd` through ordinary
shell commands. Humans and scripts use the same CLI. No special transport is
required: the binary is the API.

### CLI surface and operations

Two surfaces, one shared dispatcher.

- **Entry commands.** `memd agent-context` builds a bounded pre-work context
  file with a JSON audit log; `memd search` and `memd add` are the workhorse
  retrieve/write pair; `memd warm start|status|stop` manages the worker that
  keeps the store and indexes hot across calls; `memd batch --jsonl` streams
  structured operations through a single loaded process. `memd doctor`
  diagnoses host wiring (binary/worker version skew, the resolved data dir,
  agent-rule and hook installation).
- **Retrieval observability.** `memd search` and `memd agent-context` attach a
  `scope_status` block to every result payload: the effective tenant/project,
  the `retrieval_mode` (`hybrid` vs `text_fallback` when the embedder is
  unavailable), a warning when the requested tenant has no stored memory, and
  a `wider_scope_hits` hint when a project-scoped query comes up short but the
  tenant holds matching memory one scope wider. This keeps "no memory exists"
  distinguishable from "scoped past it" and surfaces degraded retrieval
  in-band instead of only in logs.
- **Operation surface.** `memd call <op> --json '{...}'` and the typed CLI
  subcommands dispatch to the same handlers: `memory`, `task`, `artifact`,
  `context`, `code`, and `debug`. `memory.*` is intentionally flexible
  (chunks, code, docs, traces); `task.*` and `artifact.*` are stricter and
  preserve the recoverable structure of work.

### Hybrid retrieval

| Lane | Role | Notes |
| --- | --- | --- |
| Dense (HNSW) | semantic recall | `mapping.bin` (bincode-packed), graph dump optional |
| Sparse (BM25) | lexical recall | tantivy index, `open_or_create` |
| Hot tier | recency boost | bounded LRU on top of segment store |
| Semantic cache | repeat-query short-circuit | TTL-bounded, query-hash keyed |
| Optional reranker | precision lift | feature-based by default; ONNX cross-encoder and MemReranker-4B opt-in |

Dense and sparse run in parallel; the hybrid searcher fuses their results.
Hot tier and semantic cache short-circuit recency-biased and repeat-shape
queries. Rerankers are off the quickstart path — the built-in feature
reranker handles ordinary agent traffic. See
[Optional rerankers](reranking.md) for the cross-encoder and MemReranker-4B
paths.

### Persistent store

- **SQLite metadata** under WAL-mode locking with a bounded connection pool
  (`MEMD_SQLITE_POOL_MAX`). Pooled WAL gives concurrent readers without
  starving writers.
- **Immutable segments** per tenant. Append-only payload files; deletion is
  logical (tombstones) until maintenance compacts.
- **Per-tenant WAL** (`wal.log`) fsynced before commit. Crash recovery
  replays WAL into segments.
- **Structural code index** sits next to the metadata DB and serves
  `code.*` definitions, references, callers, and imports without going
  through the hybrid retriever.

### Concurrency and the warm worker

One trusted machine runs **one writer, many readers**. The warm worker is the
normal writer: it holds an exclusive `flock` on `<data_dir>/.writer.lock` and
records its pid and `memd` version there. Reads open the store read-only, take
no lock, and never mutate indexes. Direct writes (`--warm off`, or the cold
fallback when no worker is reachable) take the same lock with a bounded retry
budget, so two processes never corrupt the store.

The worker's control socket lives at a stable path derived only from the data
directory, so a freshly upgraded binary can reach, ping, and replace a worker
left running by the previous version instead of stranding it. An idle worker
exits on its own after `MEMD_WARM_IDLE_TIMEOUT_SECS` (default 30 minutes),
releasing the lock — an orphaned daemon cannot block writes indefinitely. See
[Shared topology](shared-topology.md) for the single-writer contract in full.

### On-disk layout

See [Data layout](data-layout.md) for the full tree. The short version:

```
~/.memd/data/
├── metadata.db
├── sparse_index/
└── tenants/<tenant_id>/
    ├── wal.log
    ├── segments/
    └── warm_index/
```

## Trust boundary

Search and digest helpers return **candidates**. Canonical non-digest task
and artifact records are the trust anchor. Persisted digests (project briefs,
failure/decision/evidence libraries) are compiled hints, not authority.
Promotion to `verified_record` requires an independent reviewer with a
distinct `agent_id` submitting a verification — single-writer self-promotion
is rejected. See [Trust boundary](trust-boundary.md) for the rules and
[Comparison with other memory tools](comparison.md) for how this contrasts
with implicit-trust designs.

## Why this shape

Three design choices fall out of the layers above.

1. **No LLM in the write path.** `memory.add` stores chunks and structured
   records directly; `task.*` and `artifact.*` capture the lifecycle without
   running an extractor. The LLM, if any, is in the agent loop, not in the
   store. This keeps seed cost low and decouples write quality from
   extractor-model quality.
2. **Hybrid retrieval, not vector-only.** Dense alone misses identifiers,
   error strings, paths, and ticket IDs. Sparse alone misses paraphrase.
   Both lanes run; results are fused.
3. **Trust is a first-class object.** Most memory systems return what they
   retrieve and trust the caller to decide. `memd` separates retrieval
   candidates from canonical artifacts, marks digests as compiled hints,
   and requires distinct-writer verification before anything is labelled
   verified. The compiled wiki and digest libraries are useful precisely
   because they cannot become ungrounded authority.
