# Todo

## 2026-05-09 Track B Visibility Filter + Benchmarks

### Plan

- [x] Verify current worktree state against pickup handoff and memd records.
- [x] Implement remaining Track B visibility filtering for `memory.search` and context search paths.
- [x] Extend compaction HNSW rebuild exclusions to lifecycle-hidden chunks.
- [x] Run focused tests and benchmark gates.
- [x] Record results and follow-ups in memd.

### Progress

- Worktree is clean on `feature/nanomem-inspired` at `ac5b79b`.
- The handoff's plan file is not present in this checkout, so implementation follows the handoff, memd records, and current code.
- Current code has Track A lifecycle overlay and `memory.get` visibility, but `memory.search` and context handlers still need B1 filtering.
- Implemented `memory.search` include flags, lifecycle-aware result filtering, and over-fetch/refill after hidden rows.
- Applied the same default visibility filter and over-fetch behavior to context search paths.
- Compaction HNSW rebuild exclusions now union deleted chunk IDs with lifecycle-hidden chunk IDs.

### Review / Results

- `cargo test -p memd --test lifecycle_overlay lifecycle_hidden`: 2 passed.
- `cargo test -p memd --test lifecycle_overlay`: 15 passed.
- `cargo test -p memd compaction::runner::tests::hnsw_excluded_chunk_ids_include_lifecycle_hidden_rows`: 1 passed.
- `cargo test -p memd mcp::tools::tests`: 25 passed; pre-existing unrelated `unused_mut` warnings remain.
- Retrieval smoke benchmark:
  `cargo run -p memd-evals -- --suite benchmark --dataset-path evals/bench/datasets/retrieval/code_pairs.json --embedding-model all-minilm --system-variant hybrid-feature --bootstrap-iterations 1000 --seed 42 --threshold-recall 0.8 --threshold-mrr 0.6 --report-json /tmp/memd-track-b-code_pairs.json`
  passed 4/4 with Recall@10 1.000, MRR 1.000, Precision@10 0.200.
- Compaction suite:
  `cargo run -p memd-evals -- --suite compaction --skip-build --memd-path target/debug/memd --embedding-model all-minilm`
  passed 6/6; F5 latency p50 79ms, p99 84ms under the 500ms threshold.
- `git diff --check`: passed.
- Touched Rust files pass `rustfmt --edition 2021 --check`.
- `cargo fmt -p memd --check` still reports pre-existing unrelated formatting drift in `crates/memd/src/mcp/server.rs`, `crates/memd/src/store/metadata/sqlite.rs`, and `crates/memd/src/types.rs`; no broad formatter pass was applied.

### 2026-05-10 Verification Addendum

- Re-read the Track B diff after pickup; no additional code patch was needed.
- Re-ran `cargo test -p memd --test lifecycle_overlay lifecycle_hidden`: 2 passed.
- Re-ran `cargo test -p memd --test lifecycle_overlay`: 15 passed.
- Re-ran `cargo test -p memd compaction::runner::tests::hnsw_excluded_chunk_ids_include_lifecycle_hidden_rows`: 1 passed; Cargo still reports unrelated pre-existing `unused_mut` warnings.
- Re-ran `cargo test -p memd mcp::tools::tests`: 25 passed; same unrelated warning baseline.
- Re-ran `git diff --check`: passed.
- Re-ran touched-file `rustfmt --edition 2021 --check`: passed.
- Re-ran `cargo test -p memd --tests`: 648 passed, 4 ignored in lib tests; integration targets passed (`hnsw_persistence` 6, `http_integration` 7, `lifecycle_overlay` 15, `test_chunking_integration` 1, `trace_tools` 13).

## 2026-05-10 Track C Temporal Fields + Lifecycle Maintenance

### Plan

- [x] Add tests for temporal add, explicit expiry updates, expiry sweep, history promotion, and compact maintenance.
- [x] Implement temporal fields on `memory.add` with lifecycle overlay writes.
- [x] Add `memory.set_expiry` for setting/clearing `expires_at_ms` and `review_after_ms`.
- [x] Add persistent-store helpers for expiry sweep and stale lifecycle-hidden history promotion.
- [x] Wire lifecycle maintenance into `memory.compact`.
- [x] Run focused tests, broad tests, whitespace checks, and record results in memd.

### Progress

- Track B committed and pushed as `6f6bb1a feat(search): hide lifecycle-retired chunks`.
- Track C added temporal lifecycle writes to `memory.add`, `memory.set_expiry`, expiry sweep/history promotion helpers, and `memory.compact` lifecycle maintenance reporting.

### Review / Results

- `cargo test -p memd --test lifecycle_overlay`: 20 passed.
- `cargo test -p memd mcp::tools::tests`: 25 passed; only pre-existing unused-mut warnings.
- `cargo test -p memd --tests`: 648 passed, 4 ignored in lib tests; integration targets passed (`hnsw_persistence` 6, `http_integration` 7, `lifecycle_overlay` 20, `test_chunking_integration` 1, `trace_tools` 13).
- `git diff --check`: passed.

## 2026-05-10 Track D Conflict-Aware Ingestion

### Plan

- [x] Add tests for exact/fuzzy near-duplicate preview and supersede-on-add.
- [x] Extend supersession canonicalization with fuzzy trigram similarity helpers.
- [x] Persist canonical text from persistent `memory.add` writes.
- [x] Add `memory.find_near_duplicates` preview tool.
- [x] Add `supersede_near_duplicates` exact and opt-in fuzzy behavior to `memory.add`.
- [x] Run focused tests, tool schema tests, broad verification, and record results in memd.

### Progress

- Track C committed and pushed as `bfee846 feat(lifecycle): add temporal maintenance`.
- Track D added exact canonical duplicate lookup, opt-in fuzzy trigram matching, `memory.find_near_duplicates`, and `memory.add` supersede-on-add for active duplicate candidates.

### Review / Results

- Red test pass before implementation: `cargo test -p memd --test lifecycle_overlay` failed the four new Track D cases as expected.
- `cargo test -p memd --test lifecycle_overlay`: 24 passed.
- `cargo test -p memd mcp::tools::tests`: 25 passed; only pre-existing unused-mut warnings.
- `cargo test -p memd store::supersession::tests`: 6 passed; same warning baseline.
- `cargo test -p memd --tests`: 651 passed, 4 ignored in lib tests; integration targets passed (`hnsw_persistence` 6, `http_integration` 7, `lifecycle_overlay` 24, `test_chunking_integration` 1, `trace_tools` 13).
- `git diff --check`: passed.

## 2026-05-10 Tracks E-F-G Completion

### Plan

- [x] Track E: add `mode` to `memory.add` / `memory.add_batch`, store ingestion-mode labels, and default conversation review windows.
- [x] Run focused Track E tests, Claude Code CLI review, then commit and push Track E.
- [ ] Track F: implement OMF export/import with lifecycle and ingestion-mode round trips plus semantic merge behavior.
- [ ] Run focused Track F tests, Claude Code CLI review, then commit and push Track F.
- [ ] Track G: implement markdown projection MCP/CLI support with guarded output paths.
- [ ] Run focused Track G tests, Claude Code CLI review, then commit and push Track G.
- [ ] Run the benchmark suite again and check behavior.
- [ ] Align README/quickstart/docs, then update manuscript.

### Progress

- Resource check: 64 logical CPU cores, 251.52 GB RAM, 367.77 GB available disk, 3 CUDA GPUs. Benchmarks can run locally with high parallelism available.
- The original 2026-04-18 detailed plan file is absent in this checkout; implementation follows the active handoffs and current Track A-D code.
- Track E red tests were added first. `cargo test -p memd --test lifecycle_overlay ingestion_mode` failed as expected because current handlers ignore `mode`.
- Track E implementation adds authoritative `ingestion_mode:<mode>` tags, defaults missing mode to `document`, and sets conversation `review_after_ms` to now + 14 days unless explicitly supplied.
- Claude Code CLI review flagged semantic contracts to lock down: explicit review timestamp precedence, invalid mode errors, ingestion-mode tag normalization, supersede-on-add with conversation mode, non-persistent batch failure, and sequential persistent writes for conversation batches. Tests/comments were added for those.

### Track E Review / Results

- `cargo test -p memd --test lifecycle_overlay`: 31 passed.
- `cargo test -p memd --lib`: 652 passed, 4 ignored; only pre-existing unused-mut warnings remain.
- `git diff --check`: passed.
- Claude Code CLI Track E review completed; follow-up findings were addressed before commit.
