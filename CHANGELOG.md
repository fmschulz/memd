# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.5.0] - 2026-04-18

Retrieval durability release. Two production-observed failure modes in the persistent store are
fixed, and a startup-time HNSW backfill closes the cold-start gap that left pre-crash data
semantically invisible. Every change was reviewed by Codex CLI; all flagged issues addressed
before cutting.

### Fixed

- **`get_chunk` no longer silently returns "not found" when the in-memory segment cache drifts.**
  Observed in production: after an 8-day daemon run, a freshly-added chunk had a valid metadata
  row (status=final, correct segment/ordinal) and its segment files existed on disk, yet
  `memory.get` returned `null` and semantic search returned no results. Root read: a missing
  entry in `tenant.segments` was being collapsed into `Ok(None)`. The read path now opens the
  segment on demand, emits a `warn!` for observability, repopulates the cache via
  `entry().or_insert()` (race-safe against concurrent rollover), and propagates a real
  `StorageError` on open failure instead of masking it as "chunk not found". The underlying
  registration race that produces the drift is not yet root-caused — the defensive fix is
  observable via the new log so the next recurrence is diagnosable.

### Added

- **HNSW startup backfill for cold tenants.** `DenseSearcher` persists HNSW state only on
  graceful shutdown (Drop / explicit shutdown); any non-graceful restart — crash, SIGKILL,
  systemd restart loop — left the in-memory dense index empty for every tenant on next boot
  while `load_segments()` still rehydrated segment readers. Reads worked but semantic search
  returned nothing for pre-crash data. New `PersistentStore::backfill_hnsw_for_cold_tenants`
  async method plus free-fn `run_hnsw_backfill` iterate each tenant's active metadata rows,
  filter via the new `DenseSearcher::contains_chunk` per-chunk membership test, and re-index
  the missing chunks in batches of 64 via `hybrid.index_batch` (or `dense.index_batch` when
  hybrid is off). New `PersistentStoreConfig::backfill_hnsw_on_startup` (default `true`, env
  override `MEMD_BACKFILL_HNSW_ON_STARTUP`) triggers a one-shot background task on the ambient
  Tokio runtime at `open()`. Non-blocking — the daemon starts serving immediately; semantic
  search on older chunks degrades until the task completes.
- **`DenseSearcher::contains_chunk`.** Per-chunk HNSW-mapping membership check. Codex-reviewed:
  count-based heuristics (`index_len >= active_count`) silently skip stale tenants because
  HNSW's `next_id` never decrements on delete, so a tenant with delete + re-add + crash can
  satisfy the count check while still missing live chunks.

### Testing

- Three new integration tests in `crates/memd/tests/bug_b_rollover_read.rs` (rollover/restart
  read round-trips) plus one white-box test `get_chunk_recovers_when_segments_cache_loses_entry`
  in the persistent-store tests module that deliberately removes a finalized reader from
  `tenant.segments` and asserts on-demand recovery plus cache repopulation.
- Four new backfill tests including
  `backfill_hnsw_detects_staleness_via_per_chunk_membership_not_counts`, which pins the
  delete-skew regression Codex flagged during review.
- Full suite: 623 lib + 30 integration tests pass.

### Review

- Codex CLI reviewed both fixes across multiple rounds: Phase 1 caught silent-`Ok(None)`,
  unsafe unconditional cache insert, and tests that did not exercise the new path. Phase 2
  caught the count-heuristic blind spot and the `LIMIT/OFFSET` race under concurrent writes
  (replaced with a single `metadata.list` snapshot). All flagged issues addressed before
  landing.

## [0.4.0] - 2026-04-16

This release is the product of a deep four-phase audit-and-remediation arc across safety, adoption,
concurrency, and durability. Every phase was independently reviewed by Codex CLI; all Codex-flagged
release-blocking issues were fixed before cutting.

### Security / correctness

- **Trust-boundary rewrite.** `derive_artifact_trust_tier` no longer honours agent-supplied
  `verification_status` / `approval_state` / `ArtifactKind::Verification` / non-empty `validation`
  as trust-promotion inputs. `PromotionState::Verified` — and thus `TrustTier::VerifiedRecord` — is
  reserved for artifacts explicitly promoted by `promote_if_countersigned`, which requires a
  distinct-writer countersignature (different `agent_id`) with `supports_claim = true`. The
  "trust-theater" channel where any single agent could self-label as verified is closed.
- **`artifact.verify` renamed to `artifact.find_related`.** The tool is a retrieval helper, not a
  trust primitive; the docs and name now reflect that. The legacy `artifact.verify` alias continues
  to work with a deprecation warning.
- **Digest forgery mitigation.** `artifact.create` rejects `artifact_kind = "digest"` — digests are
  server-generated and have deterministic IDs; accepting agent-authored digests let callers
  overwrite canonical `project_brief` / `failure_library` / etc. artifacts.
- **Explicit writer identity.** Removed the forgeable process-global `SESSION_DEFAULT_AGENT_ID`.
  Artifact writes must carry an explicit `agent_id` to participate in countersignature promotion;
  anonymous writes stay anonymous and cannot produce false `VerifiedRecord` trust.
- **Tenant isolation hardened.** The legacy cross-tenant `project_id` fallback is now opt-in via
  `server.allow_cross_tenant_project_fallback` (default `false`). `TenantId::validate` runs at
  every storage-path boundary. `README.md` was tightened to state explicitly that `tenant_id` is
  NOT an authentication boundary.
- **Persistence durability.** `SegmentWriter::flush_payload` (flush + `sync_data`) runs before
  every SQLite metadata commit. `next_segment_id` now scans all `seg_*` directories on disk so
  unfinalized orphans cannot be silently overwritten. `recover_from_wal` finalizes the active
  segment before WAL truncation so a second crash after recovery cannot strand metadata.
- **HNSW compaction is real.** `DenseSearcher::rebuild_hnsw_for_tenant` actually swaps the rebuilt
  Hnsw graph into the live `RwLock<Hnsw<...>>` and marks the excluded internal-ids invalid in the
  embedding cache, so save/load after compaction no longer resurrect removed points.
- **BM25 reopens correctly.** `Bm25Index::with_path` uses `Index::open_or_create` instead of
  `Index::create_in_dir`. Hybrid search no longer silently disables itself on every daemon restart.
- **MCP protocol compliance.** Stdio notifications no longer receive responses (JSON-RPC §4.1);
  HTTP notifications that error return 202 Accepted with an empty body; `ping` returns an empty
  object `{}` with the request id echoed. Dropped the unimplemented `text/event-stream`
  advertisement from the startup banner.
- **Structural code-nav.** `code.find_callers` resolves the caller via `caller_symbol_id` (was
  looking up the wrong field and returning garbage names for multi-hop). `link_callees` is
  implemented end-to-end with an ambiguity guard; error-recovery tree-sitter spans no longer
  pollute the symbol index.
- **Chunking.** Merge-too-small-final-chunk path uses `max(old_end, new_end)` and filters overlap
  sentences, so character offsets stay consistent and no text is duplicated.
- **Cross-encoder scoring.** Logits are interpreted by shape: 1-dim regression → sigmoid, 2-dim
  `[not_relevant, relevant]` → softmax (`[1]`), other shapes warn and fall back. Previously a 2-class
  export silently inverted ranking.

### Adoption

- **Default `tenant_id` resolution.** `tenant_id` is now optional on every tool. The resolver
  falls through explicit param → `$MEMD_DEFAULT_TENANT` → `~/.memd/default_tenant` file → literal
  `"default"`. Agents that don't know their tenant still land writes in a stable local tenant.
- **Shrunk required fields on `task.*`.** `task.start` requires only `{goal}`; `task.finish` only
  `{task_id}`; analogous shrinking for `task.progress`, `task.run_start`, `task.run_finish`,
  `task.add_evidence`. Previously-required fields like `motivation`, `hypothesis`,
  `scientific_question`, `confidence`, `supports_claim` became optional with sensible defaults.
- **Split `artifact.create` into focused tools.** Four new tools with tight 3-8 field schemas:
  `artifact.review`, `artifact.revision`, `artifact.decision`, `artifact.verification`. Each
  injects the appropriate `artifact_kind` server-side and rejects a conflicting override.
  `artifact.create` stays registered with a deprecation warning.
- **Search consolidated.** `memory.search` with `mode` is the primary retrieval surface; the SKILL
  guide leads with it. `context.search_context_documents` now logs a deprecation warning on use.
  `task.search` / `artifact.search` remain first-class through v0.5 because their output shapes
  differ meaningfully.
- **Cut write amplification.** New `build_task_projections_minimal` emits 1 canonical + 1 summary
  projection. `task.progress` and `task.add_evidence` use the minimal path by default. `task.start`
  / `task.finish` / `task.run_*` keep the full fanout because their kind-specific projections
  carry tool/command content that filter paths rely on.

### Concurrency / performance

- **HTTP request concurrency unlocked.** Dropped the outer `Arc<AsyncMutex<McpServer<S>>>`. The
  server is now shared directly as `Arc<McpServer<S>>` and every handler takes `&self`. Concurrent
  MCP requests (reads, writes, and mixes) no longer serialize on a single mutex.
- **Custom SQLite connection pool.** Replaced the single `Mutex<Connection>` with a bounded pool
  (`SqliteConnectionPool`, default 16, `$MEMD_SQLITE_POOL_MAX` to override). Concurrent readers
  parallelise naturally; writers serialise at SQLite's own WAL-mode locking. Sidesteps the
  `r2d2_sqlite` / `libsqlite3-sys` version conflict with `rusqlite 0.38`.
- **Unified per-tenant `memory_version`.** The warm-tier version counter is now actually bumped on
  every add/delete via `HybridSearcher::bump_tenant_memory_version`. The semantic cache invalidates
  correctly on writes for the first time.
- **Tombstone visibility.** `SegmentReader.tombstones` is now `Arc<RwLock<TombstoneSet>>`;
  `mark_deleted` takes `&self`. Delete callers dropped from `segments.write()` to `segments.read()`
  so concurrent readers on other segments (and even on the same segment's payload) don't block.
- **Background digest sweeper.** Phase 3.4's writer-side dirty tracker is now drained on a timer
  (`$MEMD_DIGEST_SWEEP_INTERVAL_SEC`, default 10s). `memory.compact` remains a manual hook.
  Sweeper re-marks keys on regeneration failure so transient errors retry.
- **Writer-side digest invalidation.** `task.add_evidence`, `task.finish`, `artifact.create` (by
  kind, plus the validation-bearing cross-family case) mark the affected (tenant, project, role)
  digest scopes dirty via a new `task_memory::digest_dirty` tracker. Reader-path regen still works
  as a defensive fallback.

### Observability

- **Rejection metrics.** `MetricsCollector::record_rejection(tool, reason)` surfaces per-tool /
  per-reason rejection counts via `memory.metrics`. Every failing tool call is now visible in
  aggregate rather than lost to the individual error response.

### Tests

- 618 library tests, 7 HTTP integration tests, 624 with the `cross-encoder-reranker` feature (5
  ignored, requiring the real ONNX runtime). Phase 4 added a black-box HTTP integration harness
  that spawns real daemons against tempdirs and exercises the full lifecycle (initialize → task.* →
  artifact.* focused tools → memory.search → task.resume → concurrent read/write).

### Deferred to future releases

- Real session identity (cryptographic / OAuth writer attribution). Today's agent_id is explicit
  but unauthenticated.
- Full streamable-HTTP / SSE for long-running tools. `memory.compact` + `artifact.find_highlights`
  are the natural candidates.
- `hnsw_rs` → `usearch` migration. The current `ArcSwap`-style swap handles v0.4 needs; a
  native-delete index could simplify the pipeline further.

## [0.3.0] - 2026-04-10

### Added
- Explicit trust-boundary metadata on search and digest-style MCP responses.
- `artifact.verify` for grounding claims against canonical artifacts.
- Structural runtime wiring so `code.find_*` tools are initialized in normal server startup.
- A checked-in structural benchmark fixture for the eval harness.

### Changed
- The compiled wiki prototype now renders trust tiers and grounding links when current memd metadata is available.
- The packaged memd skill binary has been rebuilt from the current release tree.

### Fixed
- Structural benchmark runs no longer depend on a missing fixture or an uninitialized structural index.
- The local shared HTTP daemon can now be restarted from the current installed binary to expose the latest shipped behavior.

## [0.2.0] - 2026-04-01

### Added
- Shared local HTTP daemon support for multi-session MCP access.
- Structured task and artifact workflows for progress tracking, evidence capture, review, and thread-level collaboration.
- Summary-first project briefs, task resumes, and failure, decision, evidence, and highlight digests.
- Additional MCP tools for context retrieval, structural code queries, and debug inspection.

### Changed
- Retrieval can widen by `project_id` across older local tenant histories when needed.
- The release surface and bundled skill assets are aligned with the current `main` branch.

### Fixed
- Search-style retrieval now skips unreadable finalized chunks instead of aborting on CRC-related storage errors.
- The packaged Linux skill binary has been refreshed to the current release build.
