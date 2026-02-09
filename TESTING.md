# Testing memd

This document reflects the current verification workflow for code, MCP conformance, and retrieval evaluation.

## Scope

Use this matrix to validate:

- core crate behavior (`memd`)
- eval harness behavior (`memd-evals`)
- MCP end-to-end behavior for add/search/metrics/compact
- retrieval quality suites

## Prerequisites

```bash
cargo --version
rustc --version
```

Optional for isolated runtime checks:

```bash
mkdir -p /tmp/memd-test
```

## 1. Core test suites

```bash
cargo test -p memd
cargo test -p memd-evals
```

Expected:

- `memd`: unit + integration tests pass
- `memd-evals`: harness tests pass

## 2. MCP conformance suite

```bash
RUST_LOG=error cargo run -p memd-evals -- --suite mcp --skip-build
```

This suite validates MCP protocol/tool behavior including:

- initialize and tools/list
- memory add/search/get/delete/stats/add_batch
- metrics/compact dispatch and payload shape
- invalid JSON / unknown method / invalid params
- end-to-end add/search/metrics/compact in:
- in-memory mode
- persistent mode

## 3. Retrieval and hybrid suites

```bash
RUST_LOG=error cargo run -p memd-evals -- --suite retrieval --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite hybrid --skip-build
RUST_LOG=error cargo run -p memd-evals -- --suite true-semantic --skip-build
```

Optional dataset override (single suite runs):

```bash
cargo run -p memd-evals -- --suite retrieval --dataset-path evals/datasets/retrieval/code_pairs.json --skip-build
```

## 4. Full eval run

```bash
cargo run -p memd-evals -- --suite all
```

Notes:

- `all` runs sanity first and halts on sanity failure
- use `--include-compaction true` if you want compaction suite included in the full run

## 5. Property/fuzz-style tests currently in-tree

`memd` includes `proptest` coverage for key invariants:

- `validate_search_k` bounds and rejection cases
- `time_range` ordering/ISO validation paths
- add-time split invariants (`split_for_add`)

Run via:

```bash
cargo test -p memd
```

## 6. Manual MCP smoke test

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1.0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory.add","arguments":{"tenant_id":"smoke_tenant","text":"hello world","type":"doc"}}}' \
  | ./target/release/memd --mode mcp --in-memory --data-dir /tmp/memd-smoke
```

## 7. Common failures

- `invalid 'k': must be between 1 and 100`
- `invalid filters.time_range.from` or `.to`
- `tenant_id` validation failures (must be alnum/underscore)
- filesystem permission failures in persistent mode if `--data-dir` is unwritable

## 8. Release gate

Minimum gate before tagging/push:

```bash
cargo test -p memd
cargo test -p memd-evals
RUST_LOG=error cargo run -p memd-evals -- --suite mcp --skip-build
```
