# Workflow Summary
This document records the implementation and reproducibility details for `memd` as represented in the repository state at branch `feat/phase6-cutting-edge-retrieval`, commit `3b1fbabbff4e968ffc57992b127097e2d0dd0b2c`, version `0.1.0`. `memd` is a local Model Context Protocol (MCP) server for shared agent memory. The software combines append-only persistent storage, SQLite metadata, Candle-based dense embeddings, HNSW approximate nearest-neighbor search, Tantivy BM25 sparse retrieval, hybrid fusion and reranking, a task-oriented knowledge artifact layer, structural code indexing, and optional feature-gated ONNX cross-encoder reranking. This Methods record is grounded in checked-in code, configuration, tool definitions, and benchmark artifacts, and it avoids claims that are not supported by repository evidence.

## Software Artifact and Evidence Package
The software artifact audited for this Methods record is the Rust workspace declared in `Cargo.toml` and the `memd` crate declared in `crates/memd/Cargo.toml`. The repository remote recorded for the audited artifact is `https://github.com/fmschulz/memd.git`. The package version is `0.1.0`, the workspace edition is Rust 2021, and the primary implementation modules are exposed through `crates/memd/src/lib.rs`. The evidence package used to prepare this document consisted of the top-level user documentation in `README.md` and `QUICKSTART.md`, the default configuration in `configs/default.toml`, the MCP tool schema in `crates/memd/src/mcp/tools.rs`, the persistent storage and retrieval code in `crates/memd/src/store/*` and `crates/memd/src/index/*`, the task-memory schema in `crates/memd/src/task_memory/mod.rs` and `docs/scientific-task-memory/schema/README.md`, and the checked-in benchmark protocols and reports in `evals/BENCHMARK_PROTOCOL.md` and `docs/scientific-task-memory/benchmark-results/`.

The local toolchain captured during documentation consisted of `cargo 1.92.0`, `rustc 1.92.0`, and `Python 3.11.14`. No container digest, lockfile pin, or single workflow-engine provenance object was available for this documentation run. Consequently, this record describes a repository-grounded software documentation pass rather than a provenance-complete pipeline execution such as a Nextflow or Snakemake run.

## Build, Configuration, and Launch
The documented release build entrypoint is:

```bash
cargo build --release
```

The documented runtime entrypoint for persistent shared-session MCP service operation is:

```bash
./target/release/memd --mode mcp --transport http --http-bind 127.0.0.1:8787
```

The documented legacy subprocess entrypoint remains:

```bash
./target/release/memd --mode mcp
```

The documented disposable runtime entrypoint for local HTTP testing is:

```bash
./target/release/memd --mode mcp --transport http --http-bind 127.0.0.1:8787 --in-memory --data-dir /tmp/memd-demo
```

Configuration is loaded from an explicit `--config` path when provided, otherwise from `~/.config/memd/config.toml`, with fallback to compiled defaults. The default configuration declares `~/.memd/data` as the persistent data directory, JSON logging at level `info`, `stdio` as the default MCP transport, and `127.0.0.1:8787` plus `/mcp` as the default HTTP bind and path values. The checked-in configuration layer now validates both `stdio` and `http` transports, and path expansion for `~/` is handled inside the configuration loader.

## Software Architecture
`memd` exposes its functionality through an MCP server and a local CLI. The current MCP tool surface contains 30 tools distributed across generic memory operations, task-oriented artifact operations, context retrieval, structural code analysis, and debugging queries. The top-level architectural split is between raw searchable memory written with `memory.*` tools and structured task knowledge written with `task.*` tools. This distinction is fundamental to the software design and is not merely an interface convenience: `memory.*` stores flexible chunks, whereas `task.*` stores canonical lifecycle artifacts and then projects those artifacts into retrieval-optimized chunks.

The repository now supports two MCP transports. The original path is JSON-RPC over stdio, where the client launches `memd` as a subprocess. The new recommended path for shared local sessions is an HTTP MCP daemon bound to a single endpoint such as `http://127.0.0.1:8787/mcp`. The HTTP implementation currently supports request-response JSON-RPC over HTTP POST and intentionally returns `405 Method Not Allowed` on HTTP GET rather than exposing an SSE stream. When the `Origin` header is present, the server validates it against localhost-style origins, and the recommended bind address remains `127.0.0.1` rather than `0.0.0.0`.

Client integration reflects this transport split. The checked-in install materials now target `~/.codex/config.toml` for Codex CLI and `~/.claude.json` for Claude Code, and the repository includes helper scripts for registering the shared daemon with both clients and for verifying cross-session interoperability between Codex and Claude.

Persistent storage is implemented as a composite system rather than a monolithic database. Chunk payloads are stored in append-only segment files under `tenants/<tenant_id>/segments/`, durability is protected with a write-ahead log, and lightweight metadata is normalized into SQLite. The metadata layer stores only chunk descriptors and indexing state, not payload bodies. This design separates crash recovery and queryable metadata from payload storage, while preserving a straightforward directory layout in the user’s data directory. The repository documentation and storage modules further show that persistent mode writes `metadata.db`, tenant WAL files, segment directories, `warm_index/` for persisted dense retrieval state, and `sparse_index/` for lexical retrieval state.

Dense retrieval uses a Candle-backed embedder and an HNSW warm-tier index. The dense search coordinator constructs a `CandleEmbedder`, derives the embedding dimensionality from the loaded model, embeds chunk text during indexing, and embeds the query at search time before searching the HNSW structure. Sparse retrieval is implemented with Tantivy BM25 and a code-aware tokenizer. The sparse index stores tenant identifiers, chunk identifiers, sentence indices, and indexed text, and it is committed and reloaded to support search visibility. Hybrid retrieval then fuses dense and sparse candidates using reciprocal-rank fusion and passes the fused candidates into a reranking stage.

Reranking is intentionally split from dense retrieval. The default mode is a feature-based reranker that combines fused rank score with recency, project affinity, and chunk-type preferences. An optional feature-gated ONNX cross-encoder reranker can instead score query-document pairs with an ONNX model. The repository evidence shows that ONNX is not the default embedding path. The default embedding path remains Candle; ONNX is only used for the optional cross-encoder reranking stage. If the cross-encoder feature is not compiled in, or if ONNX runtime/model initialization fails, the code falls back to feature reranking rather than aborting the service.

The task-oriented knowledge layer keeps a canonical task artifact envelope separate from retrieval projections. The canonical envelope captures fields such as task goal, motivation, hypothesis, scientific question, tool choice, parameters, inputs, outputs, evidence, validation, uncertainty, and follow-up actions. These canonical artifacts are projected into ordinary retrieval chunks of kinds such as `task_goal`, `task_summary`, `run`, `evidence`, `worked`, `failed`, and `validation`. Exact task-aware filters are applied through normalized SQLite side tables before the candidate set is reranked for `task.search`. This design is explicitly documented in the checked-in task schema README and benchmarked in the task-memory evaluation artifacts.

Beyond raw memory and task artifacts, `memd` includes structural indexing and debugging subsystems. The structural subsystem uses tree-sitter parsers and associated query services to extract symbols, call edges, imports, traces, and stack frames. Query routing can separate structural intents from ordinary semantic retrieval and fall back to semantic search when needed. The compaction subsystem monitors tombstone ratios, segment count, and HNSW staleness, and includes dedicated modules for HNSW rebuild, segment merging, throttling, tombstone auditing, and a compaction runner. The checked-in code comments state that parts of the compaction manager remain skeletal, so compaction should be described as implemented infrastructure with some lifecycle pieces still evolving.

## Write and Retrieval Semantics
When new memory or task projection chunks are added in persistent mode, the payloads are written to the WAL and active segment, metadata rows are inserted into SQLite, and indexing is then performed synchronously or through the optional asynchronous indexing worker. The indexing path uses the hybrid indexer when hybrid retrieval is enabled and otherwise uses the dense searcher directly. In both cases the embedding stage is Candle-backed. Long documents are split at add time into sub-chunks with chunk-position metadata, preserving a consistent write-time behavior across backends.

At query time, `memory.search` and the broader retrieval path first obtain candidate results through hybrid dense+sparse search when the persistent hybrid searcher is available. Dense retrieval embeds the query with Candle, sparse retrieval performs BM25 search over the lexical index, and the resulting candidate set is fused. Full reranking occurs after candidate chunks are fetched and annotated with metadata and text content. If the service was started with the default `hybrid-feature` variant, the feature reranker is used. If the service was started with `hybrid-cross-encoder` and the feature flag plus ONNX runtime are available, the ONNX cross-encoder is used in the reranking step. In-memory mode bypasses these persistent retrieval backends and uses a simple in-memory search path, which is suitable for smoke testing but not for evaluating the full retrieval architecture.

`task.search` follows a distinct query plan. It first resolves exact candidate chunk identifiers from normalized task tables using the requested filters, then reranks that candidate set for the query text. The checked-in benchmark results are consistent with this design: the structured task-memory path incurs more write-time expansion but yields substantially better artifact recovery than flattened chunk search on the hardened Phase 5 corpus.

## Reproducible Usage
The most direct reproducible software commands documented in the repository are shown below.

Build and launch persistent shared-session MCP service:

```bash
cargo build --release
./target/release/memd --mode mcp --transport http --http-bind 127.0.0.1:8787
```

Smoke-check HTTP MCP startup:

```bash
curl -sS -X POST http://127.0.0.1:8787/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"quickstart","version":"0.1.0"}}}'
```

Register current client CLIs against the shared daemon:

```bash
codex mcp add memd --url http://127.0.0.1:8787/mcp
claude mcp add --transport http --scope user memd http://127.0.0.1:8787/mcp
```

Run the checked-in cross-client verification script:

```bash
./scripts/verify_shared_http_clients.sh
```

Build and run the optional ONNX cross-encoder path:

```bash
cargo build --release --features cross-encoder-reranker
./target/release/memd --mode mcp --search-variant hybrid-cross-encoder
```

Run the real ONNX smoke test:

```bash
cargo test -p memd --features cross-encoder-reranker smoke_real_onnx_scores_relevant_pair_higher -- --ignored --nocapture
```

Run the kept offline retrieval benchmark flow:

```bash
./evals/bench/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

Run the task-memory benchmark flow:

```bash
./evals/bench/scripts/run_task_memory_benchmark.sh
```

## Evaluation Artifacts and Quality Control
The repository contains two benchmark families that are relevant to software characterization. The first is an offline retrieval benchmark protocol over BEIR-style datasets and a small smoke dataset. The documented datasets are `beir_fiqa.json`, `beir_scidocs.json`, `beir_trec-covid.json`, and `code_pairs.json`, and the documented aggregate metrics are `Recall@10`, `MRR`, `Precision@10`, and latency, with bootstrap confidence intervals controlled by `--seed` and `--bootstrap-iterations`.

The second is the checked-in Phase 5 task-memory benchmark. The checked-in report states that the hardened `2026-03-21.v2` corpus contains 8 task cases, 23 labeled queries, and 4 shared-project sibling groups. On that artifact, the generic chunk baseline achieved `hit@3 = 0.00`, `MRR = 0.00`, and average search latency `145.4 ms`, whereas the structured task-memory path achieved `hit@3 = 0.96`, `MRR = 0.82`, and average search latency `2.9 ms`. Because these values are drawn directly from the checked-in report, they are suitable for documentation of repository state but should not be generalized beyond that corpus without rerunning the benchmark.

Quality control for this Methods document consisted of cross-checking all architectural claims against code or checked-in documentation, limiting numeric performance claims to checked-in benchmark reports, and validating the optional ONNX cross-encoder path with a real ignored smoke test rather than relying only on unit tests that use fallback lexical scoring.

## Outputs and Limitations
The outputs of this documentation run are `METHODS.md`, `MANUSCRIPT.md`, and `run_manifest.yaml`. The main limitation is that this record documents a software repository rather than a single provenance-rich workflow run. Some operational aspects, such as a full release build log or a fresh end-to-end rerun of every benchmark entrypoint, were not captured as one canonical artifact bundle in this documentation pass. In addition, the checked-in task schema documentation explicitly notes that a formal external schema specification and migration history are not yet provided. The compaction manager also includes documented scaffolding for later expansion.

The transport layer also has current, explicit boundaries. While `memd` now supports an HTTP daemon that can be reached by multiple local sessions and, if the operator chooses, by other machines over a private network or tunnel, the repository does not yet implement a hosted multi-user control plane with built-in authentication or server-enforced per-account isolation. `tenant_id` remains a caller-supplied logical partitioning key rather than a security boundary. These limitations are recorded here to keep the documentation faithful to the current state of the repository rather than an aspirational future state.
