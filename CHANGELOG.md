# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Fixed

- `memory.md` now deduplicates project and machine-wide candidates as one
  bounded union, including exact chunk IDs, consolidation lineage, and topic
  matches. Active-project results are assigned to the project section instead
  of appearing twice or leaking into the machine-wide section.
- Startup answerability no longer treats a missing or unreadable
  `tasks/todo.md` as proof that no tasks are open. The generated project state
  records the task source as missing, parsed-empty, parsed-open, or failed.

- CLI, structured-operation, batch, OMF-import, supersession, episode, and
  consolidation writes now share one preparation service for tag
  normalization, admission, retention, priority, and initial trust. Batch and
  single writes no longer diverge on inferred priority or retention defaults.
- Consolidation now admits chunks retrieved after the last-run watermark,
  even when those chunks were created earlier. Every candidate is checked
  against its current lifecycle and project scope before use.
- Tenant-wide consolidation no longer hides project-scoped source memories.
  It records `derives_from:<csv>` lineage and leaves those sources active;
  project-scoped consolidation retains its existing `supersedes:<csv>` and
  soft-tombstone behavior.
- WAL replay preserves newer SQLite lifecycle metadata when repairing an
  existing payload location, so restart cannot revert `Final`, `Superseded`,
  `Expired`, or `Error` rows to the status stored in their original Add record.

### Added

- Search and agent context now return privacy-safe retrieval episode IDs.
  `memd outcome` and `memory.record_outcome` accept explicit used/harmful
  attribution and verifier evidence. `outcome-v1` computes bounded,
  project-scoped, time-decayed shadow ranks while production serving remains
  unchanged.
- `memd eval-outcome-ranking` writes JSON and Markdown reports comparing
  served and source-deduplicated shadow top-k lists against explicit relevant
  and harmful chunk judgments.

- Consolidation runs use durable run, entry, and normalized lineage tables.
  Outputs are written as hidden `Candidate` chunks. The default
  `memd consolidate` stops after validation; `memd consolidate-review --list`
  discovers staged runs, and `--accept` records durable promotion intent
  before atomically promoting candidates with their same-project sources.
  `memd consolidate --promote` explicitly requests that path in one command.
  Exact source-set retries reuse the existing run.
- LLM consolidation output must include a concrete agent action, exact source
  evidence, and bounded confidence. The journal records the consolidator
  command, model, and version and references a permission-restricted,
  size-capped raw-response audit artifact with integrity hashes.
- Session start performs bounded consolidation recovery before refreshing
  agent context. Recovery isolates per-run errors, protects fresh in-flight
  work with a 30-second grace period, promotes only runs with recorded intent,
  and refreshes promoted dense-index rows. Background consolidation stages
  proposals for later review instead of changing source visibility.

### Changed

- Upgrading an existing store is conservative. Previously validated runs gain
  `promotion_requested = 0` and appear in the review list, but cannot be
  promoted because they lack the new audit artifact; an acceptance attempt
  fails closed and rejects their candidates. Older planned or
  candidate-written runs without that artifact are rejected during recovery.
  Sources remain active in both cases. `--legacy-immediate` preserves the old
  one-command behavior for one release and prints a deprecation warning.

## [1.4.0] - 2026-07-11

### Added

- **bge-base-en-v1.5 embedder** (`--embedding-model bge-base`): a 768-d Candle
  BERT retriever (CLS pooling, query-only instruction prefix) selectable
  alongside the default `all-minilm`. Assets are fetched on first use from a
  pinned Hugging Face revision and verified by sha256, matching the existing
  MiniLM download contract.
- **Model-conditional default search variant**: when `bge-base` is selected and
  `--search-variant` is not given, the retrieval default becomes `dense-only`
  (hybrid fusion off). `all-minilm` and other models keep the `hybrid-feature`
  default, and an explicit `--search-variant` always takes precedence.

### Changed

- **Warm workers report their embedding model and search variant in the ping
  identity.** A resident worker serving a different model/variant than the
  client requests is now shut down and respawned (the same path as a
  version/protocol skew), instead of silently answering with the wrong
  embedder.
- **Switching `--embedding-model` on an existing store no longer wipes the
  dense index.** A persisted index built at a different vector dimension
  (e.g. bge-base 768-d vs all-minilm 384-d) is incompatible with the active
  model; memd now fails with a clear error naming both dimensions instead of
  deleting the embedding cache and leaving an empty index. Set
  `MEMD_BACKFILL_HNSW_ON_STARTUP=1` to intentionally discard the dense index
  and re-embed from segments under the new model. Same-dimension cache
  corruption still deletes and rebuilds as before.
- `CandleEmbedder::with_config` applies the model's required pooling strategy
  (mean for MiniLM, CLS for bge-base), overriding any pooling carried on the
  passed `EmbeddingConfig` — pooling is a property of the model's training
  recipe, not a caller choice.

## [1.3.1] - 2026-07-06

Fixes a 1.3.0 regression in warm-worker write acknowledgements.

### Fixed

- **Write acks imply searchability again**: 1.3.0 defaulted the warm worker
  to async indexing, which acknowledged `memory.add` / `add_batch` before
  the chunks were indexed — a bulk load followed by immediate searches read
  a partially built index (BEIR fiqa+scidocs paired nDCG@10 0.42 → 0.26;
  caught by the retrieval gate on the release push). Adds now hold their
  acknowledgement until the background index job completes. The await
  yields, so the worker's event loop, ping, and concurrent commands stay
  live while the indexer works — 1.3.0's availability properties are
  preserved. The search-side read-your-writes wait this replaces is
  removed.
- **Cold-path searches wait again**: the bounded search-lock / busy-reply
  behavior is now scoped to warm-worker processes. Single-shot CLI searches
  racing an in-process repair wait for the lock, as before 1.3.0, instead
  of failing with `memd:dense-index-busy`.

With correct acknowledgements, the 1.3.0 retrieval improvements measure
above the pre-1.3.0 baseline on the document-retrieval gate as well:
fiqa nDCG@10 0.497 → 0.709 (Recall@10 0.882 → 0.941), scidocs nDCG@10
0.373 → 0.453.

## [1.3.0] - 2026-07-06

Retrieval recall, bi-temporal memory, and warm-worker availability.

### Added

- **Event time on writes**: `memory.add` / `memory.add_batch` accept an
  optional `event_time_ms` — when the underlying event occurred, as distinct
  from ingestion time — persisted to the chunk's bi-temporal
  `timestamp_observed`. `memory.search` gains an opt-in `render_event_time`
  that prefixes each result's text with its observed date (`[YYYY-MM-DD]`)
  at recall, so answer models see when events happened without dates
  polluting the indexed text.
- **Source-aware result dedup**: `memory.search --dedupe-by-source` collapses
  ranked results sharing a `source.uri` to the best-ranked one before the
  final top-k trim (default off; for one-document-per-add workloads).
- **Per-request timing in batch**: `memd batch` response rows report
  `elapsed_ms` per request, so latency percentiles don't depend on buffered
  stdout timing.
- **Repo-local LoCoMo benchmark harness** (`benchmarks/locomo/`):
  fetch-on-run dataset, hermetic per-run stores, retrieval and QA
  answer-usefulness evals, date-at-render and external-contexts QA modes,
  and documented same-harness reference comparisons.

### Fixed

- **Warm-worker availability**: a dense-index write hold (repair pass or
  bulk insert) could park the worker's single event-loop task and freeze
  every client — ping included — until the 30s client timeout. Searches now
  bound their index lock waits and fail fast with a busy error carried on
  the warm wire (`busy` reply flag, wire-compatible both directions);
  auto-mode reads fall back to the cold path immediately; the worker
  defaults to async indexing (explicit `MEMD_ASYNC_INDEXING` wins) so adds
  acknowledge after WAL + metadata and the background indexer absorbs the
  lock wait; searches wait a bounded window for a just-acked add's index
  job to land, preserving read-your-writes.
- **Crash consistency**: sparse-index self-heal on open (a tenant with
  active rows but an empty sparse index is re-indexed through the hybrid
  path instead of silently degrading to dense-only), segment finalize
  syncs payload before `meta` (no torn payloads in loadable segments), and
  WAL checkpoints finalize the active segment first (recovery no longer
  discards committed rows).
- **Deterministic ranking**: every ranking sort carries a fixed tie-break,
  so equal-scored results no longer reorder run-to-run.

### Performance

- **BM25 per-chunk collapse pre-RRF**: BM25 indexes each sentence of a
  chunk as its own document, so a multi-sentence chunk could occupy several
  sparse candidate slots and be double-counted by the RRF accumulator.
  Sparse search now over-fetches and collapses per-sentence hits to
  distinct chunks at their best rank. Measured on the MemoryData LoCoMo
  eval (Qwen3-8B, n=600, paired against a same-session control):
  recall@1 +10.4, recall@5 +3.2, recall@10 +1.5 — all 95%-CI significant —
  at flat latency.

## [1.2.1] - 2026-06-30

Retrieval robustness and performance improvements.

### Fixed

- **Lexical query parsing**: BM25 search falls back to an alphanumeric-only query
  when both strict and lenient Tantivy parsing fail (e.g. queries with brackets
  or other special characters) instead of erroring — fewer failed lexical
  searches and better recall on messy queries.
- **Batch-add deduplication**: `memory.add_batch` now deduplicates exact-content
  chunks both against the store and within the same batch before a single bulk
  insert, so batch writes no longer create duplicate chunks.

### Changed

- **Skip the dense path when disabled**: hybrid search skips dense indexing and
  dense search entirely when `dense_k == 0`, avoiding wasted embedding/search
  work in sparse-only configurations.

## [1.2.0] - 2026-06-28

Turns `memory.md` into a deterministic agent startup briefing and adds a
release gate for startup-memory usefulness.

### Added

- **Latest Project State**: `memd memory-md` now starts with the resolved
  tenant/project scope, project directory, generation time, git branch/dirty
  summary, latest task or handoff signal, source-backed next actions, and
  project-scoped memory readability warnings.
- **Agent-usefulness gate**: `memd eval-memory-md --agent-usefulness` checks
  startup output for current project state, git state in git repositories, open
  task next actions, sourced next actions, suppressed fragments, suppressed
  boilerplate, and bounded machine-wide facts.
- **Gold-file evaluation**: `memd eval-memory-md --gold-file <path>` runs the
  agent-usefulness gate across local multi-project fixtures.
- **Explain-output project state**: `memory-md --explain-output` now includes
  structured project state, startup-quality flags, topic keys, and
  agent-usefulness metrics for debugging noisy startup context.

### Changed

- **Startup fact libraries**: the rendered sections are now `Project Fact
  Library` and `Machine-Wide Fact Library`. Machine-wide facts default to two
  items, while project facts are deduplicated by topic and filtered for
  generated wrappers, continuation fragments, generic boilerplate actions, and
  unrelated machine-wide records.
- **Session-start defaults**: `memd session-start` uses the same capped
  machine-wide fact default as `memory-md`.
- **Memory health scan**: project-scoped readable/unreadable counts are bounded
  for session-start use and report a partial-scan warning instead of doing
  unbounded payload reads.

## [1.1.1] - 2026-06-17

Bounds warm-worker memory so a misconfigured idle timeout can no longer
accumulate resident workers until the host runs out of memory.

### Fixed

- **Warm-worker accumulation / OOM**: the warm worker keeps the embedding model
  (~400 MB on CPU) resident, and the idle timeout was the *only* reaper for
  idle/orphaned workers. With `MEMD_WARM_IDLE_TIMEOUT_SECS=0` every per-data-dir
  worker became immortal, so many ephemeral data dirs (e.g. per-test / per-run
  `.memd` directories) could pile up hundreds of workers and exhaust host RAM.
  Three independent guards now prevent this and make `idle=0` safe again:
  - a resident-worker **cap** (`MEMD_WARM_MAX_WORKERS`, default 16, cannot be
    disabled): `warm start` refuses to spawn beyond the cap and the client falls
    back to the cold path (or errors under `--warm required`);
  - **idle-independent orphan eviction**: a worker whose published pid file no
    longer names it has been replaced (bind+rename race) and exits within ~60 s,
    even when the idle reaper is disabled;
  - an **ephemeral-data-dir guard**: auto-warm is skipped for `pytest-of-` data
    dirs (the accumulation vector), overridable with `MEMD_WARM_ALLOW_EPHEMERAL=1`.

### Added

- **`MEMD_WARM_MAX_WORKERS`**: hard ceiling on concurrent warm workers (default
  16); `0` or an invalid value falls back to the default — the cap cannot be
  disabled.
- **`MEMD_WARM_ALLOW_EPHEMERAL`**: opt ephemeral (`pytest-of-`) data dirs back
  into auto-warm.

## [1.1.0] - 2026-06-15

Makes the warm worker's index repair non-blocking, cuts HNSW cold-start cost,
and adds embedding-device pinning and a prebuilt-first installer.

### Added

- **`MEMD_EMBED_DEVICE`**: pin the embedding device (`cpu`, `cuda`, `cuda:N`)
  instead of always taking `cuda:0`, so memd can stay off a contended GPU on
  shared machines.
- **Prebuilt-first `make install`**: installs the released binary when it runs
  on the host and compiles from source only as a fallback, so a first install
  no longer requires a Rust toolchain.
- **`warm status` repair signal**: the `ryw_probe` payload now reports
  `repair_in_progress`, so an in-flight background index repair is observable.

### Changed

- **Non-blocking external-mutation repair**: when the warm worker detects an
  external `metadata.db` mutation it now schedules a single-flight background
  HNSW repair and serves the request after a short bounded wait, instead of
  running the full backfill synchronously in the request path. Previously a
  detected mutation could block a warm request for minutes and trip the client
  timeout (notably on shared/NFS data directories). The worker still indexes
  its own warm-routed writes synchronously, so same-worker read-your-writes is
  unchanged; dense/hybrid coverage of externally-added chunks catches up once
  the background repair lands (SQLite and sparse reads are immediate).
- **Honest warm client-timeout message**: the timeout no longer asserts a
  successful cold-path fallback that may not have happened; it points at the
  worker log and `memd warm status`.

### Fixed

- **`add_batch` usage events**: batched adds now record per-chunk usage events
  (mirroring single `add`), so the usefulness report and the central per-chunk
  hit log no longer undercount batched writes.

### Performance

- **Cheap HNSW cold-start**: the cold-start backfill reuses persisted
  embeddings (`embeddings.bin`) and re-embeds only the genuine delta via a
  cache-aware membership check, instead of re-embedding every chunk from text.
  A clean restart on a 140-chunk tenant drops from ~50 s to no re-embeds.

## [1.0.0] - 2026-06-11

First stable release. Hardens retrieval recall, input validation, and the
write-admission gate (external code review), and refreshes the LoCoMo benchmark
numbers against the current code.

### Fixed

- **Retrieval recall**: hybrid retrieval no longer truncates the candidate pool
  before reranking, so deeper matches are no longer silently dropped.
- **Cross-agent prompt injection**: `ProjectId::validate` rejects `..` and
  markdown/path-control bytes at input boundaries while still allowing repo
  basenames (`-` and `.`), so a project id can no longer inject instructions
  into cross-project memory.
- **Write-admission gate**: non-finite or garbage `priority:` values (`inf`,
  `nan`, `garbage`) can no longer short-circuit the low-signal gate to durable,
  and oversize text is rejected before the marker scan.

### Changed

- Re-measured LoCoMo on the current code: memd MRR@10 0.412, Hit@10 0.613, mean
  search 23.2 ms (previously 0.420 / 0.621 / 26.7 ms). memd still leads both
  baselines and wins all four categories; docs and figures updated to match.

## [0.62.0] - 2026-06-11

### Added

- In-band `scope_status` block in every `memd search` / `memd agent-context`
  payload (and a terse markdown footer): reports the effective tenant/project,
  `retrieval_mode` (`hybrid` vs `text_fallback` when semantic search is
  degraded to substring matching), a warning when the requested tenant has no
  stored memory on this machine, and — when a project-scoped search returns
  fewer than `k` results — `wider_scope_hits` with the exact widening command,
  so "no memory exists" and "memory exists one flag away" are distinguishable.
  The `wider_scope_hits` probe keys on the pre-budget retrieval count, so
  token-budget trimming (the default `agent-context` path) never emits a
  false widen hint or an avoidable extra tenant-wide scan.
- `memd add` payloads report `created_tenant: true` when a write creates a
  brand-new tenant, so a typo'd `--tenant-id` no longer silently forks a
  fresh silo.
- Warm worker idle timeout (`MEMD_WARM_IDLE_TIMEOUT_SECS`, default 1800,
  `0` disables): an idle worker exits and releases the data-dir writer flock
  instead of holding it forever.
- `.writer.lock` holder line now records the holder's memd version next to
  its pid.

### Changed

- The warm socket path is now a pure function of the data dir — version,
  wire protocol, embedding model, and search variant are no longer hashed in
  — so after a binary upgrade the new CLI can ping, stop, and auto-replace a
  worker left behind by the old binary instead of bricking machine-wide
  writes. `warm stop`/`warm status` also sweep legacy version-hashed socket
  dirs (`legacy_stopped`/`legacy_workers` payload keys).
- `memd doctor` now diagnoses the resolved global `--data-dir` (was
  hardcoded `~/.memd/data`), pings the warm worker and flags worker-vs-CLI
  version skew, and reports the PATH binary's actual `--version` output
  (was: the current process's compile-time version), flagging PATH skew.
- `memd consolidate` with an explicit `--tenant-id` no longer silently
  inherits the cwd scope file's `project_id` (mirrors `memd search`
  semantics): tenant-wide consolidation now actually consolidates
  tenant-wide. Tenant-wide runs warn in the summary JSON that lessons
  written without a `project_id` only surface via tenant-wide search and
  memory-md machine-wide takeaways.
- `memory-md --global-limit` now defaults to 5 (was 0) and `memd
  session-start` includes machine-wide takeaways in the auto-refreshed
  `memory.md`, so cross-project lessons reach new projects by default.
- The priority-8+ write gate admits-and-downgrades instead of rejecting:
  high-priority writes without a concrete `Agent action:` sentence are
  stored at priority 7 with an in-band `admission_warning` naming the verb
  allowlist, which gained common imperatives (`set`, `configure`, `pin`,
  `export`, `point`, `enable`, `disable`, `update`, `keep`). The gate and
  the memory-md renderer now share one allowlist.
- `memory.md`'s Known Failures section now requires a real failure signal
  (`kind:failure` tag, `root cause`, `failed because`, `failure:`,
  `blocker`): success traces mentioning "0 failures" or "resolved after"
  classify as Validated Fixes instead of being filed as failures with
  fabricated avoid-this guidance, and arrival via a `*_failures` retrieval
  query alone no longer counts as failure evidence.
- The agent contract (skill, installer snippet, generated guardrails) adds:
  if project-scoped retrieval returns nothing, rerun with `--tenant-id` only
  (no `--project-id`) before concluding no memory exists.

### Removed

- The structurally unreachable cross-project sharing tiers:
  `consolidate --promote-to-shared` and the `MEMD_SHARED_TENANT` shared-tenant
  plumbing (the multi-project trigger predicate could never fire — the
  consolidation region is pre-filtered to one project), the
  `memory-md --cross-tenant` section (readable only from the unpopulatable
  shared tenant), `memd init --scope/--allow-tenants` and the
  `read_tenants`/scope-mode fields in `tenant_scope.json`/
  `project_scope.json` (written but consumed by zero retrieval code; old
  scope files with the fields still parse), the dead
  `HybridSearcher::search_with_routing` surface, and the never-spawned
  digest sweeper (`spawn_digest_sweeper`). The unused
  `MEMD_DIGEST_SWEEP_INTERVAL_SEC` env var is gone with it.

## [0.61.0] - 2026-05-28

### Added

- Cross-platform prebuilt release binaries via cargo-dist: macOS (arm64,
  x86_64) and Linux (x86_64, aarch64) built as **static musl**, published as
  GitHub Release assets with a one-line `curl … | sh` installer
  (`memd-installer.sh`). Static musl eliminates `GLIBC_… not found` errors on
  older and HPC Linux hosts.

### Changed

- Releases are now built and published by cargo-dist
  (`.github/workflows/release.yml`, tag-triggered): it builds the four
  platform archives and creates the GitHub Release with notes from this
  changelog. `auto-release.yml` now only pushes the `vX.Y.Z` tag — via a
  `RELEASE_PAT` secret so the cargo-dist build is triggered — and no longer
  creates the Release itself.
- The skill installer (`install_memd_enforcement.sh --install-binary`) now
  downloads the latest release binary instead of copying an in-repo blob.
  README, INSTALL, SKILL, and the docs site document the one-line installer,
  with `cargo binstall memd` (best-effort) and `cargo install memd` (from
  source) as alternatives.

### Removed

- Removed the ~30 MB Linux binary committed at `memd-skill/bin/linux-x64/memd`
  and the `release-skill-binary.yml` workflow; cargo-dist release assets
  replace both.

## [0.60.0] - 2026-05-28

### Added

- Added write-admission guardrails for public memory writes so obvious
  low-signal progress chatter and generated digest wrappers do not become
  durable memory by default, while explicit priority / importance tags and
  concrete evidence, decisions, commands, paths, fixes, and validation results
  remain durable.
- Added `memd eval-retrieval` and `memd eval-write-quality` gates for checking
  retrieval quality, known-useful recall, admission behavior, dedupe behavior,
  storage growth, and generated-digest suppression with machine-readable
  reports.
- Added `memd audit`, `memd cleanup-plan`, `memd purge`, and
  `memd purge-archive` operations for measuring memory bloat, producing
  approval-first cleanup plans, archiving purge candidates, and verifying purge
  archives.
- Added operational documentation for useful memory writes, cleanup safety,
  archive-first purge workflows, and memd-vs-alternative positioning.

### Changed

- `memd memory-md`, `memd search`, and session-start context now bias more
  strongly toward durable, project-scoped, non-generated lessons and suppress
  low-value generated digest noise unless explicitly requested.
- Cleanup plans now include estimated purge batches plus exact destructive and
  archive-verification command previews so large stores can be cleaned in
  reviewable chunks.
- `memd purge --apply` now verifies the written archive before mutating the
  store and reports the verification result in the completed purge response.

### Verified

- Re-ran the release BEIR retrieval benchmark gate with the CI-pinned fiqa +
  scidocs parameters. Candidate normalized `nDCG@10` was 0.571, and the paired
  regression gate passed against `evals/bench/baselines/beir_v1.json`
  (`candidate_mean=0.536`, `baseline_mean=0.417`, 47 paired queries).

## [0.51.0] - 2026-05-23

### Added

- Added `memd doctor`, a filesystem-only diagnostic command that reports
  binary discovery, data directory presence, global agent rule wiring for
  Claude / Codex / Cursor, the Claude Code `SessionStart` hook, and the
  current project's `.memd` scope. It supports human-readable Markdown and
  machine-readable JSON / JSONL output and does not require opening the store.
- `memd session-start` now auto-creates a minimal
  `.memd/project_scope.json` when a repo has no scope yet, using
  `$MEMD_DEFAULT_TENANT`, then `$USER`, then `"default"` for `tenant_id` and
  the sanitized repo basename for `project_id`. Set `MEMD_AUTO_SCOPE=0` or
  add `.memd-skip` in the repo root to opt out.
- The skill installer now writes a Cursor user rule at
  `~/.cursor/rules/memd.mdc` and wires the Claude Code `SessionStart` hook in
  `~/.claude/settings.json`.

### Changed

- Updated README, MkDocs pages, skill docs, and verification scripts to cover
  auto-scope, Cursor enforcement, `memd doctor`, and the expanded installer
  behavior.

## [0.50.0] - 2026-05-21

This release eliminates the long-running warm_index orphan-snapshot leak
and ships disk-hygiene tooling that lets operators reclaim space without
manual file-juggling. Triage measured one production tenant's warm_index
at ~249 GB while the underlying chunk data was a few hundred MB — a
single hnsw_rs reload-then-save behavior was the culprit. The fix is
narrow, foundational, and TDD-driven, with a Codex code review checkpoint
on every phase.

### Fixed

- HNSW warm index no longer leaks orphan `graph-NNNN.hnsw.*` snapshots
  on every save. Root cause: hnsw_rs 0.3.3's `HnswIo::load_hnsw`
  unconditionally sets `datamap_opt = true` on the returned `Hnsw`,
  forcing `file_dump` into the unique-basename fallback. The loader
  only ever reads the canonical basename, so every reload-then-save
  cycle stranded one fresh snapshot pair on disk. Existing orphans
  are swept on the next load and reclaimed at scale via `memd
  maintenance`. Triage on one long-lived install observed a single
  tenant's warm_index at ~249 GB (with ~1.5k stale snapshots) before
  this fix; per-install footprint will vary with reload-then-save
  frequency.
- `HnswIndex::load` now wraps `hnsw_rs::load_hnsw` in `catch_unwind`.
  A crash that landed mid-`file_dump` leaves both canonical files
  present but truncated; hnsw_rs panics (not Err) when reading those.
  The daemon now degrades to rebuild-from-embedding-cache instead of
  unwinding.

### Added

- `HnswConfig.persist_graph_dump` (default `true`): set to `false` to
  skip the HNSW graph dump entirely and rely on rebuilding from
  `embeddings.bin` on load. Halves warm_index disk footprint at the
  cost of one rebuild pass per startup. Old configs lacking the field
  deserialize to `true` for back-compat.
- `PersistentStoreConfig.min_finalize_chunks` (default `256`): active
  segments below this threshold are not sealed on graceful shutdown
  or Drop. The chunks remain durable via WAL replay on the next
  startup. The gate disables itself automatically when
  `wal_checkpoint_interval > 0` so checkpointing-enabled deployments
  do not lose data. KNOWN LIMITATION: today the WAL recovery rewrite
  path still creates a fresh segment per startup; the visible
  cross-run segment-count reduction needs a follow-up segment-reuse
  patch (incremental `payload.idx` persistence + open-for-append).
  This release lands the configurable surface so the reuse work has a
  stable entry point.
- `memd maintenance` CLI subcommand. Flags:
  - `--data-dir <p>` (inherits the top-level `--data-dir` if omitted)
  - `--tenant-id <t>` restricts the sweep to one tenant
  - `--dry-run` reports without modifying disk
  - `--aggressive` runs the full pass (orphan sweep today; segment
    compaction and mapping repack hooks reserved for follow-up).
  Output is greppable key:value (`removed_orphan_snapshots: N` for
  real runs, `would_remove_orphan_snapshots: N` for dry runs, plus
  `orphan_snapshots_failed: M` for real runs that hit unlink errors).

### Changed

- `IndexMapping` now persists as `mapping.bin` (bincode, `config::standard()`)
  instead of `mapping.json`. Bincode is typically several times more
  compact than the equivalent JSON for this struct shape; cold-load
  parsing is correspondingly faster. Legacy `mapping.json` is read
  once on upgrade and the next save rewrites as `mapping.bin` and
  removes the JSON file. Crash between rename and remove leaves a
  loadable state (parent-dir sync runs between the two operations).

## [0.40.0] - 2026-05-21

This release introduces the **memory self-improvement loop** — a
continuously-updated, LLM-curated working set of takeaways that survives
across sessions. Four cooperating mechanisms (heuristic priority at write
time, LLM consolidation, retrieval-success counterfactual scoring,
opt-in cross-tenant transfer) replace the previous purely extractive
`memory.md` rendering. The reranker also gains a lightweight query-text
lexical bonus that materially improves retrieval quality without
requiring a cross-encoder.

### Added

- Added `memd memory-md`, a session-start CLI command that refreshes a
  project-root `memory.md` with up to 10 project takeaways and up to 10
  machine-wide takeaways ranked by explicit `priority:N` / `importance:N`
  tags, memory type, `kind:*` tags, recurrence, multi-query matches, and
  search score.
- **Reranker: query-text lexical bonus.** `FeatureReranker` now blends a
  query/document keyword + bigram + phrase + numeric overlap score
  (weight 0.12 by default) into the final ranking, so a relevant chunk
  whose RRF score is dragged down by long-tail noise still surfaces. The
  bonus is bounded [0.0, 1.0] and uses ASCII-only tokenisation so it
  cannot mis-score on Unicode text. Configurable via
  `RerankerConfig::query_text_weight`.
- **Phase 1 — Heuristic priority foundation.** New `auto_priority`
  module stamps a `priority:N` tag (3..=7) at write time based on
  `ChunkType`, `kind:*` tags, and validation/finish text signals;
  explicit user tags always win. The cap is set below the
  preserve-on-suppression threshold so heuristic stamps can never
  masquerade as deliberate operator judgement. `memory.md` now adds
  (a) a +15 priority bonus for `task:role:highlight_library`,
  `task:role:project_brief`, and `kind:consolidated` chunks; (b) a
  4th query targeting digests; (c) a post-merge filter that drops raw
  `task:kind:task_finish` takeaways whose `task:id` is covered by a
  system-generated digest (verified via `task:status:generated`),
  keyed by `(project_id, task_id)` so cross-project finishes are
  preserved, and skipping any chunk with explicit `priority>=8`.
- **Phase 2 — LLM-backed consolidation.** New `consolidate` module
  with a pluggable `Consolidator` trait and adapters for
  `claude -p --model claude-haiku-4-5-20251001 --output-format json`
  and `codex exec --model codex-5.3-spark --json`. Selected at runtime
  via `MEMD_CONSOLIDATOR=claude|codex|auto|mock`. New `memd
  consolidate` subcommand builds a working region from chunks
  written/retrieved since the last run, calls the LLM to rewrite them
  into deduplicated `kind:consolidated` lessons with
  `supersedes:<csv>` provenance and `consolidator:<name>` attribution,
  inherits the dominant `ctx:*` tags, and soft-tombstones every
  source via `ChunkStatus::Superseded` (never deletes). Chunk text is
  serialised as JSON inside the prompt so untrusted content cannot
  forge framing. Subprocess execution timeouts the whole
  spawn+write+wait sequence and reaps zombies on expiry. New `memd
  session-start` subcommand refreshes `memory.md` synchronously then
  spawns a detached `memd consolidate --background` when ≥ 10 dirty
  chunks have accumulated (preflighting that a consolidator backend
  exists). `memory.md` and `memd search` now hide
  `kind:superseded`-tagged chunks; `memd search --include-superseded`
  exposes them for provenance lookups. The bundled skill installer
  wires the Claude Code SessionStart hook into
  `~/.claude/settings.json` idempotently; a Codex example template
  ships at `memd-skill/examples/codex_session_start_hook.json`.
- **Phase 3 — Retrieval-success signal.** New `hit_stats` module
  appends one JSONL record per returned chunk to
  `.memd/data/hit_counts.jsonl` (one `write_all` per line, lines
  ≥4 KiB dropped to preserve the Linux atomic-append boundary), with
  a 1 h TTL aggregate cached to `.memd/data/hit_counts.summary.json`.
  `priority_score` consumes those stats: +0.8 per recent selection
  (capped at +8), −2 for chunks with zero hits older than 30 days.
  New `memd eval-counterfactual` subcommand replays a JSONL
  benchmark file (default
  `evals/bench/queries/counterfactual_queries.jsonl`, 20 starter
  queries included) and writes a Markdown report under
  `evals/bench/reports/counterfactual_<unix>.md` with overlap@k loss
  and mean rank shift between full retrieval and a
  `kind:consolidated`-filtered baseline derived from the same
  ranking pass. Internal probes call a `cli_search_payload_silent`
  variant so they do not pollute the retrieval-success signal.
- **Phase 4 — Cross-tenant transfer.** `memd memory-md
  --cross-tenant` (opt-in) renders a `## Cross-Tenant Takeaways`
  section sourced from `kind:consolidated, priority>=8` chunks
  across every other tenant under the store data root, deduped by a
  normalised first-100-char key. `memd consolidate
  --promote-to-shared` (opt-in) copies any consolidated lesson whose
  supersedes set spans ≥ 2 distinct named projects to the
  `MEMD_SHARED_TENANT` (default `shared`) with
  `kind:cross_tenant_promoted, source_tenant:<orig>,
  source_chunk:<id>, source_projects:<csv>` provenance. Promotion is
  idempotent — every promoted chunk carries a deterministic
  `provenance:<source_tenant>:<sha8>` tag derived from the source
  consolidated chunk id and the sorted supersedes list; duplicates
  are detected and re-used.

### Changed

- Generated project guardrails and bundled skill docs now require agents to
  refresh and read `memory.md` at session start before task-specific
  `agent-context` retrieval, and recommend `priority:N` tags for durable
  lessons.

## [0.30.0] - 2026-05-12

### Changed

- Split the main CLI implementation into focused modules for arguments,
  search/context payloads, rendering, batch JSONL, warm-worker control, call
  parsing, path handling, and the operation bridge while preserving the
  existing public `memd::cli::*` compatibility paths.
- Replaced wildcard MCP handler exports/imports with explicit public exports
  so future operation-layer moves are reviewable and do not silently shrink
  the Rust API surface.
- Moved the shared operation handler implementation out of
  `mcp/handlers.rs` into the protocol-neutral `ops` module, leaving the
  historical `memd::mcp::handlers::*` path as a compatibility re-export.
- Migrated the eval harness away from the removed `memd --mode mcp` process
  startup path. The harness now has a current CLI contract suite, a `CliClient`
  for `memd call`, and a temporary MCP-shaped compatibility wrapper for older
  behavior suites.
- Recorded the retired protocol-only MCP conformance suite in
  `evals/bench/BENCHMARK_INVENTORY.md`.
- Refreshed repo-wide rustfmt output in a dedicated style commit so future
  formatter checks are meaningful.

### Fixed

- Eval-harness help and package metadata now describe the CLI-first executable
  instead of MCP conformance.
- Warm-worker sockets now fall back to a short temp-directory path when the
  configured data directory would exceed Unix socket path length limits.
- Release metadata and `memd-wiki` lockstep package versions now align with the
  `0.30.0` binary release.

## [0.13.0] - 2026-05-10

### Added

- CLI-first agent workflow: `memd agent-context`, expanded `memd search`,
  `memd add`, and `memd call <operation> --json ...` now cover the ordinary
  agent workflow without external client-tool registration.
- Private warm-worker commands (`memd warm start/status/stop`) and
  `--warm auto|off|required` for `search`, `agent-context`, and `call`, giving
  repeated local retrieval a low-latency path while keeping the public
  interface as shell commands.
- `memd batch --jsonl` for benchmark and script workloads that need many
  structured operations in one loaded process.
- Optional MemReranker-4B post-retrieval reranking for `memd search` through
  explicit `--reranker auto|memreranker-4b` flags. The default search path
  stays slim and does not load Python, PyTorch, Hugging Face models, or GPU
  runtimes.
- Bright-Pro adapter code for scoped static-retrieval checks against the
  Bright-Pro framework, including `memd`, SuperLocalMemory, and optional
  MemReranker comparison lanes.

### Changed

- Root documentation, quickstart, skill docs, installer scripts, and verifier
  scripts now document skill + CLI as the main workflow.
- The compiled JSON-RPC/HTTP transport code was removed from the Rust crate;
  reusable operation handlers remain available through `memd call` and
  `memd batch`.
- The bundled `memd` skill binary was refreshed to the current release build.
- README benchmarking now highlights the recommended warm-worker and batch
  execution modes. Startup-overhead diagnostics remain in raw benchmark
  artifacts for reproducibility, but are not presented as the recommended agent
  workflow.
- Task-memory benchmark scripts now run against the release binary by default
  and can report `cli_cold`, `cli_warm`, and `cli_batch` lanes.
- `memd-wiki` compiler code now handles path-shaped project identifiers with a
  safe page stem, prunes missing referenced tasks instead of failing a build,
  and backfills digest-library grounding refs from concrete result rows when
  available.
- `memd-wiki` now reads project state through the local `memd` executable
  instead of a network endpoint.

### Fixed

- README Mermaid diagrams were simplified so GitHub renders them reliably.
- CLI workflow docs no longer point to stale client-registration files or
  wrapper guard paths that are not part of the shipped workflow.
- Release-facing ignore rules now keep local caches, draft paper files, raw
  benchmark payloads, and local agent configuration out of normal commits.
- `memory.health` now computes duplicate aggregate counts and ratios over the
  full requested tenant/project scope even when `include_examples=true`.
  `duplicate_limit` limits only the returned example groups, so
  `memory.dream` reports no longer understate duplicate pressure.
- `context.find_relevant_context(include_hot=true)` now bounds the legacy
  hot-context pre-scan before falling through to normal retrieval, avoiding
  long lookups on large or duplicate-heavy tenants.
- Retrieval/list scans now skip unreadable stale segment rows with a warning
  instead of aborting the whole scan; strict `memory.get` behavior is
  unchanged.

## [0.12.0] - 2026-04-21

### Added

- **BEIR retrieval-gate infrastructure.** The `memd-evals` harness now
  computes BEIR-standard nDCG@{1, 5, 10, 100}, supports graded qrels,
  reads `CrossCorpusReport` / `BenchmarkReport` interchangeably in the
  regression gate, accepts a TOML dataset manifest, and ships with a
  committed baseline + GitHub Actions workflow so accidental retrieval
  regressions fail PR checks instead of silently landing.
  - **P1 nDCG math** (`59df881`). New
    `calculate_ndcg(retrieved, grades, k)` in
    `evals/harness/src/suites/benchmark_protocol/math.rs` using the
    standard `2^rel - 1` gain with `log2(rank + 1)` discount.
    Ten unit tests against hand-computed textbook vectors with 1e-12
    tolerance: binary / graded perfect-ranking = 1.0, reverse-ranking
    against the [3,2,1] ideal = 0.6806060567602009, retrieved outside
    qrels = 0.0, empty grades = 0.0, k=0 = 0.0, iDCG caps at known
    relevant docs (1 retrieved / 2 relevant → 0.6131…), cutoff
    excludes positions beyond k, mixed ranking with a zero slot =
    0.9594535145926796, k > retrieved.len() with complete qrels = 1.0.
  - **P2 graded qrels + per-query nDCG in reports** (`a062d8f`).
    Additive `queries[].relevance_grades: HashMap<String, u8>` on
    dataset JSON. `build_query_grades()` synthesizes grade=1 for
    every entry in `relevant[]`, and explicit grades override that
    (including grade=0 to demote a binary-relevant entry).
    `QueryMetrics` / `BenchmarkSummary` gain
    `ndcg_at_k: BTreeMap<usize, f64>` fields — BTreeMap for
    byte-stable JSON, `#[serde(default)]` so pre-0.12 baselines
    read clean. The benchmark runner asks the retriever for top-100
    (the max cutoff in `NDCG_K_VALUES = &[1, 5, 10, 100]`);
    recall/MRR/P@10 are explicitly sliced to top-10 so old baselines
    remain numerically comparable. Stdout summary lines gain an
    `nDCG@10 {x.xxx | n/a}` suffix.
  - **P3 CrossCorpusReport-aware regression gate + `--metric`**
    (`410d7f4`, folded as `117d117`).
    `load_report_either` tries `CrossCorpusReport` first and falls
    back to `BenchmarkReport`. Cross-corpus alignment pairs queries
    per-dataset (match by `dataset_path`, then by `query_id` within
    that dataset) so `q1` collisions across datasets can't produce
    false positives in the paired test. New `--metric` flag (default
    `ndcg_at_10`; accepts `all`, `recall_at_10`, `mrr`,
    `precision_at_10`, or `ndcg_at_<k>` for any positive k). Metrics
    absent from both sides skip with an actionable "regenerate
    baseline" reason instead of failing on missing data.
    `DatasetBenchmarkResult` gains `#[serde(default)] query_metrics`
    so cross-corpus reports carry the per-query detail the paired
    gate needs.
  - **P4 `--dataset-manifest` + trec-covid soft fetch + README
    reconcile** (`303c834`). New `evals/bench/beir_manifest.toml`
    lists fiqa / scidocs / trec-covid with path + name +
    qrels_format hint + approx_bytes + license. Relative paths
    resolve against the manifest's own directory. `memd-evals`
    gains a `--dataset-manifest` flag that concatenates manifest
    entries after explicit `--dataset-path` flags. The offline
    dataset fetcher gains `try_fetch_one` for `beir_trec-covid.json`
    — warns on 404 instead of failing the whole script (the base
    commit mirror doesn't carry trec-covid today; manual drop-in is
    still supported). `evals/bench/datasets/retrieval/README.md`
    reconciled: new tier table names all five BEIR files the
    harness ever touches, including the two manual-only entries
    (`beir_scifact_fixed.json`, `beir_nfcorpus.json`) that
    `scifact.rs` and `nfcorpus.rs` silently expect.
  - **P5 committed baseline + regeneration ritual** (`1fc3a92`).
    `evals/bench/baselines/beir_v1.json` — a `CrossCorpusReport`
    generated at this commit against fiqa (17 queries after
    relevance filtering) + scidocs (30 queries) with
    `--embedding-model all-minilm --system-variant hybrid-feature
    --max-queries 30 --max-documents 500 --seed 42
    --bootstrap-iterations 1000`. Headline: normalized nDCG@10 =
    0.4347 (fiqa 0.4969, scidocs 0.3725); normalized nDCG@100 =
    0.5221. `evals/BENCHMARK_PROTOCOL.md` gains the full nDCG
    section, a pinned-parameter table that lockstep-mirrors the CI
    env vars, and a two-PR regeneration ritual (PR 1 lands the
    substantive change; the gate fails; PR 2 lands the refreshed
    baseline with a written justification).
  - **P6 CI workflow** (`87a37be`, folded as `c1ac5b8` after Codex
    review). `.github/workflows/retrieval-gate.yml` runs on
    pull_request + push-to-main against `crates/` / `evals/` /
    `Cargo.{toml,lock}`. Single job: cache cargo + BEIR datasets +
    embedding model, build release memd + memd-evals, run
    `--suite benchmark` with the pinned-parameter set,
    run `--suite benchmark-regression --metric ndcg_at_10` against
    the committed baseline, upload candidate + regression reports
    as an always-retain artifact. Concurrency group cancels
    superseded runs for the same ref. `timeout-minutes: 60`
    accommodates the cold-cache path. Least-privilege
    `permissions: { contents: read, actions: read }`. All pinned
    parameters live as top-level env vars
    (`BEIR_EMBEDDING_MODEL`, `BEIR_SYSTEM_VARIANT`,
    `BEIR_MAX_QUERIES`, `BEIR_MAX_DOCUMENTS`, `BEIR_SEED`,
    `BEIR_BOOTSTRAP_ITERATIONS`, `BEIR_METRIC`,
    `BEIR_SIGNIFICANCE_ALPHA`, `BEIR_MIN_EFFECT_SIZE`) so a bump
    is a single edit.

### Changed

- `memd-evals`: retrieval depth for the benchmark protocol is now 100
  (max of `NDCG_K_VALUES`), up from 10. Historical recall/MRR/P@10
  are explicitly sliced to the top-10 window inside `evaluate_queries`
  so pre-0.12 baselines remain numerically comparable.

### Deferred to v0.12.x+

- Heavy-BEIR lane (`nq`, `hotpotqa`, `arguana`, `webis-touche2020`)
  gated on a `--include-heavy` branch in the fetcher with a
  `--accept-licenses` gate.
- Commit-SHA pinning for the GitHub Actions in
  `.github/workflows/retrieval-gate.yml` (a repo-wide supply-chain
  audit pass, not a retrieval-gate concern specifically).
- `beir_trec-covid.json` mirror hosting (deferred until a future
  commit bump that includes it in the pinned base-URL commit).

## [0.11.0] - 2026-04-21

### Added

- **memd-wiki serve v3.0: read-only HTTP runtime over the compiled
  tree.** The four-phase sub-plan deferred from v0.10.0 shipped as
  four discrete commits (`fdb67fc`, `6711f87`, `4ec36d7`, `8087f3a`)
  with a mid-P1 security fold (`ffc825e`). `memd-wiki serve` binds
  localhost by default and exposes the compiler's full page set plus
  the human-owned `notes/` lane at stable artifact-id URLs. Zero new
  third-party dependencies — the server is stdlib
  `http.server.ThreadingHTTPServer` + a hand-rolled Markdown → HTML
  renderer.
  - **P0 `memd-wiki serve` skeleton** (`fdb67fc`). Registered the
    `serve` subparser next to `build` / `lint` / `migrate`, plus a
    pure `resolve_route(outdir, url_path)` + `make_handler(outdir)`
    factory. Signal-safe shutdown on SIGINT + SIGTERM.
  - **P1 hand-rolled Markdown → HTML renderer** (`6711f87`, folded
    in `ffc825e`). `tools/wiki/compiled_wiki/html_render.py` covers
    the exact dialect the deterministic compiler emits (ATX
    headings, `-` lists with 2-space continuation lines, inline
    code, link, italic/bold, YAML frontmatter) plus the minimal
    superset LLM-authored concept bodies need (fenced code, hr).
    Pure string-in / string-out so golden-byte tests don't bind a
    socket. Every link `href` runs through a URL-scheme allowlist
    — `http`, `https`, `mailto`, `ftp`, `ftps`, and scheme-less
    relative URLs are permitted; `javascript:` / `data:` / etc.
    render as HTML-escaped literal markdown so the filtered URL
    stays visible instead of silently dropped. The scheme guard
    runs AFTER the link rewriter so a malicious rewriter cannot
    smuggle an unsafe URL through. The served document is wrapped
    in a minimal self-contained `<html>` shell with an inline
    `<style>` block (zero external asset requests).
  - **P2 expanded route table + containment guard** (`4ec36d7`).
    Formal routes for `/`, `/log`, `/manifest.json` (raw
    `application/json`), `/concepts/<id>/`, `/entities/<id>/`,
    `/tasks/<id>/`, `/projects/<id>/`, `/libraries/<name>/`, and
    `/notes/a/b/c/`. Layered defenses before hitting disk: per-
    segment char allowlist `[A-Za-z0-9._-]+`, top-level prefix
    whitelist, reuse of
    `containment.reject_if_any_symlink_inside_outdir` (fail-closed
    parity with the Rust export-markdown CLI), and `Path.is_file()`
    — all rejections funnel to a 404 `text/plain` so the server
    does not leak why a request was refused.
  - **P3 per-page link rewriter** (`8087f3a`). Compiler-emitted
    relative `.md` links (`projects/memd.md`,
    `../tasks/019dadab.md`, `../libraries/failures.md`) are
    resolved against the current page's outdir-relative path and
    rewritten to the P2 route shape. Query + fragment suffixes are
    preserved. Paths that normalize to an outdir escape
    (`../outside.md` from `index.md`) pass through unchanged so
    the browser 404s cleanly. Round-trip integration test crawls
    BFS from `/`, scrapes every `<a href>` via
    `html.parser.HTMLParser`, and asserts zero dead internal links
    on a seeded linked tree.

### Changed

- Both `Cargo.toml` and `tools/wiki/pyproject.toml` move to 0.11.0
  in lockstep. The MCP tool contract and manifest schema are
  unchanged — a v0.11.0 `memd-wiki` works against a v0.11.0 memd
  server identically to how v0.10.0 against v0.10.0 worked, plus
  the new `serve` subcommand.

### Notes

- Test counts: Rust suite unchanged at 910 / 4 ignored; Python
  `tools/wiki` grew from 174 to 249 (+75 = 23 html_render unit +
  9 URL-scheme / edge-case folds + 23 P2 route-resolver + 15 P3
  rewriter + 5 integration/round-trip) with zero regressions.
- v3.1+ deferrals documented in `tools/wiki/README.md`: slugs +
  `concept_slugs` manifest field, `--build`/`--open`/`--quiet`
  flags, watch + live rebuild, in-memory search.

## [0.10.0] - 2026-04-20

### Added

- **memd-wiki v2: LLM-authored concept / entity pages (plan
  `docs/plans/active/2026-04-20-memd-wiki-v2-llm-authored-pages.md`).**
  Adds a first-class lane for LLM-authored knowledge pages alongside
  the deterministic compiler-owned surface from v1. Six phases
  shipped as discrete commits, each with its own test gate; the
  rollout is opt-in (default `concept_pages = []`) so a fresh
  install runs identically to v0.9.0.
  - **`ArtifactKind::WikiPage` + `TaskArtifact::content`** Phase 0:
    new variant on `ArtifactKind` with `as_str = "wiki_page"`,
    `FromStr` mapping (error-message updated to enumerate the new
    kind), and a nullable `content: Option<String>` field on
    `TaskArtifact` carrying the markdown body. The trust model is
    explicit: a fresh `WikiPage` sits at `TrustTier::CanonicalRecord`
    and stays there forever — the existing
    `promote_if_countersigned` path early-returns on non-Review/
    Revision/Verification/Decision kinds, so the WikiPage's own
    `promotion_state` never reaches `Verified`. Verification of the
    page's claims is signaled by *children*: distinct-writer
    `Verification` artifacts whose `reply_to_artifact_id` targets
    the page promote themselves (via the existing path) and the
    renderer surfaces them as a `Verified by:` footer. MCP-boundary
    validator rejects non-empty `content` on every other kind.
  - **`artifact.create` accepts `wiki_page`** Phase 1: enriched
    boundary validator requires non-empty `related_artifact_ids`
    (grounding refs), `summary ≤ 500 bytes`, `artifact_role ∈
    {"concept", "entity"}`, and `content ≤ 256KB`. Constants live
    on `pub(crate)` so follow-on work references the canonical
    limits.
  - **Python compiler reads `wiki_page` artifacts** Phase 2:
    `tools/wiki/compiled_wiki/compiler.py` now calls `artifact.search`
    with `artifact_kind=wiki_page` filter, resolves each grounding
    ref via `artifact.get`, fetches Verification children via a
    second `artifact.search` call (filter on `reply_to_artifact_id`
    + `promotion_state=verified`), and renders one markdown page
    per WikiPage under `concepts/<artifact_id>.md` (role=concept) or
    `entities/<artifact_id>.md` (role=entity). Stable sort key
    `(artifact_role, entity_name or summary[:50], artifact_id)`
    keeps output byte-identical under arbitrary backend
    permutation. The render emits YAML frontmatter, a summary
    heading + metadata bullets, the raw markdown body from
    `content`, a `## Grounded By` footer with backlinks, and (when
    Verification children exist) a `## Verified By` footer. v1
    surfaces (index, log, projects, tasks, libraries) are
    unchanged.
  - **Manifest schema v2** Phase 2: bumps
    `MANIFEST_SCHEMA_VERSION` from 1 to 2, adds
    `llm_authored_prefixes = ["concepts/", "entities/"]`,
    `human_owned_prefixes = ["notes/"]` (declared for v3 but the
    compiler never writes there), and a `concept_pages` list with
    one entry per WikiPage (artifact_id, path, trust_tier, role,
    grounding_refs, source_updated_at_ms). Empty-by-default so a
    fresh install with no WikiPage artifacts produces a clean v2
    manifest.
  - **Four new lint checks** Phase 3, scoped to
    `manifest.concept_pages`:
    - `concept-missing-grounding` (ERROR, paranoid): WikiPage with
      empty `grounding_refs` (validator already rejects this; the
      check defends against rows from a prior-version server).
    - `concept-stale` (WARN, oracle-gated): page snapshot lags the
      newest grounded artifact by more than `concept_staleness_ms`
      (default 30 days). Skipped silently when no oracle is
      provided.
    - `concept-contradicts-canonical` (ERROR, syntactic scaffold):
      page cites ONLY `task_finish` artifacts with
      `status=rejected`; v3 layers an LLM-backed semantic diff
      onto the same hook.
    - `concept-trust-tier-ungrounded` (ERROR): page self-labels
      `verified: true` (frontmatter or body) without a matching
      `Verified by:` footer line. Closes the trust-laundering
      vector codex caught during plan review.
  - **Manifest forward-compat + `memd-wiki migrate`** Phase 4: new
    `WikiManifestTooNewError` raised by `check_manifest_version`
    when `manifest.schema_version > MAX_KNOWN_MANIFEST_SCHEMA_VERSION`.
    `lint_output_dir` re-raises it so a future-manifest reader gets
    a clear "upgrade memd-wiki" diagnostic instead of a silent
    partial lint. New `memd-wiki migrate` subcommand upgrades v1
    manifests to v2 in place (preserves existing fields, adds
    empty new lanes via `setdefault`); `--dry-run` prints the
    upgraded manifest without writing.
  - **MCP contract pin** Phase 2: `mcp_contract.py` adds
    `artifact.search` (with filter args) and `artifact.get` to
    `REQUIRED_MCP_TOOLS` so a memd-side rename or schema change
    breaks the contract test rather than the wiki silently.
- **Advisory single-writer lockfile for HF model downloads (Candle
  follow-up from v0.8.0 handoff).** `download_file` in
  `crates/memd/src/embeddings/download.rs` now acquires a sibling
  `<target>.lock` via atomic `create_new` (O_EXCL on Unix, CREATE_NEW
  on Windows) before streaming. Late-arriving processes see the
  contended lock, poll (50ms → 2s backoff, 15-minute bound) for the
  target file to publish, then return `Ok` without re-downloading.
  Stale locks older than 60 minutes (generous for a ~614MB Qwen3
  download on a 2 Mbps link) are reclaimed. Correctness is still
  anchored by the existing `hard_link`-based first-writer-wins
  publish — the lock is purely a bandwidth optimization that degrades
  to the pre-existing race-safe behavior on lock-owner crash,
  permission errors, or wait-timeout. Adds 10 new unit tests
  (`test_advisory_lock_*`, `test_wait_*`,
  `test_download_file_waiter_reuses_publication`) covering lock
  acquisition, contention, stale reclaim, TOCTOU re-check on
  lock-disappearance, and an end-to-end two-caller cooperation path
  against a single-connection HTTP server.

### Changed

- **MCP `artifact_kind` enum extended.** The `artifact.create`,
  `task.search`, and `artifact.search` JSON schemas now accept
  `"wiki_page"` in addition to the v1 set. JSON consumers with
  closed-enum validators must update.

### Migration

- Existing v0.9.0 wikis: run `memd-wiki migrate --output-dir
  <wiki_dir>` once to upgrade the manifest from `schema_version=1`
  to `schema_version=2`. The next `memd-wiki build` will overwrite
  the manifest with the canonical v2 shape regardless, so the
  explicit migrate is only needed if you want to read the manifest
  with v0.10.0 lint before recompiling.
- Existing v0.9.0 servers: refresh
  `memd-skill/bin/linux-x64/memd` and `~/.local/bin/memd` from the
  v0.10.0 release build, then restart the daemon. The MCP wire
  format is additive; pre-v0.10.0 clients keep working but cannot
  author `wiki_page` artifacts.

## [0.9.0] - 2026-04-20

### Added

- **memd-wiki: first-class sibling surface (Item 7 / plan
  `docs/plans/active/2026-04-20-item7-compiled-wiki-promotion.md`).**
  Relocated `prototypes/compiled_wiki/` to `tools/wiki/` as a tracked
  Python package with its own `pyproject.toml` (`requires-python>=3.11`,
  `console_scripts memd-wiki = compiled_wiki.cli:main`), version-
  aligned with the `memd` workspace. Ships:
  - **Server-version compat gate** via parsed MAJOR.MINOR
    (`compiled_wiki.compat`), stderr WARN on patch skew, hard fail
    `ServerIncompatibleError` on MAJOR.MINOR mismatch. Vendored
    stdlib semver parser (`_semver.py`).
  - **Config loader** (`compiled_wiki.config_loader`): nearest-
    ancestor `.memd/config.json` with `wiki` subsection (`outdir`,
    `max_tasks`, `library_k`, `memd_url`); CLI > config > built-in
    defaults precedence; typed `ConfigLoadError` with file path on
    malformed input; stops at the first scope-file hit even if
    incomplete.
  - **Containment guard** (`compiled_wiki.containment`), a verbatim
    port of `memd export-markdown`'s three refusal rules: reject
    `outdir` inside `$HOME/.memd/data`, reject `outdir` inside the
    scope-resolved `data_dir` from the nearest-ancestor
    `.memd/tenant_scope.json`, and reject any pre-existing symlink
    component below `outdir`. Uses `os.lstat` + `stat.S_ISLNK` so
    `ELOOP` / `ENOTDIR` / permission errors surface as refusals.
  - **Deterministic rebuild contract.** Second run on unchanged
    memd state yields `written=0` and byte-identical `manifest.json`.
    Stable secondary sorts (`task_id`, `artifact_id`) on tasks /
    thread artifacts / library results / log entries so the
    invariant holds under arbitrary backend reordering of a
    logically identical payload.
  - **Manifest v1**: `manifest.json` now embeds `schema_version: 1`
    and `compiler_owned_prefixes`, scaffolding the v2 LLM-authored /
    human-edited ownership split without changing the format.
  - **Force-emit referenced task pages.** Libraries and the project
    page can link to any task returned by a digest query; the
    compiler now emits `tasks/<id>.md` for every referenced task
    (primary window union with `grounding_refs` + library
    `results.task_id`) so internal links never dangle.
  - **Lint subcommand** (`memd-wiki lint`): 5 health checks with
    exit codes `0` / `1` / `2` — library-missing-grounding
    (ERROR), dead-backlink (ERROR, scoped to compiler-owned
    prefixes), trust-tier-ungrounded (WARN), manifest-drift (ERROR,
    force-emit task pages accepted), manifest-missing /
    -invalid (ERROR).
  - **MCP tool contract pin** (`compiled_wiki.mcp_contract`): the
    7 tools memd-wiki depends on (`context.brief_project`,
    `task.resume`, `artifact.list_thread`, and the 4
    `artifact.find_*` surfaces) are pinned as typed expectations.
    Offline test asserts the contract matches compiler.py call
    sites; live integration test (skippable when no daemon reachable)
    asserts the running memd honors it.
  - Full operator README at `tools/wiki/README.md`.

### Fixed

- **Candle embedder `RelativeUrlWithoutBase` boot failure.** Replaced
  `hf-hub` 0.3.2 in `CandleEmbedder::with_config` with a direct `ureq`
  download helper (`embeddings::download::get_candle_bert_paths`). The
  old path passed huggingface.co's relative 307 Location headers verbatim
  to `ureq::get`, which failed URL parse with `Bad URL: failed to parse
  URL: RelativeUrlWithoutBase`. Plain `ureq::get` follows the relative
  redirect correctly against the request base. Fixes the pre-existing
  env-block that forced the v0.8.0 Item 1 retrieval gate to close by
  constructive argument instead of Recall@10 / MRR / Precision@10
  metrics.

### Changed

- **Dropped `hf-hub` workspace dependency.** Only consumer was the
  Candle embedder; replaced with the existing `download.rs` ureq
  plumbing.
- **Candle model cache moved to `~/.cache/memd/models/`.** Previously
  the Candle embedder used `hf-hub`'s cache at `~/.cache/huggingface/hub`
  (or `$HF_HOME/hub`). A host upgrading from v0.8.0 with a warm hf-hub
  cache but no network access will re-download `config.json`,
  `tokenizer.json`, and `model.safetensors` (~91MB) on first use. Once
  downloaded, the memd cache is stable across versions.
- **`download_file` writes atomically with first-writer-wins publish.**
  Downloads now stream into `<target>.partial.<pid>.<thread>.<counter>`,
  call `sync_all()`, then publish via `hard_link` — which fails
  atomically with `AlreadyExists` when a racing caller has already
  published the same target. The loser cleans up its tmp and keeps the
  winner's bytes; `rename` is deliberately avoided because it would
  silently clobber a concurrent publisher on Unix. An interrupted
  download, a crash mid-stream, or two racing processes/threads can
  now never leave a half-written `config.json` / safetensors blob at
  the canonical cache path where `verify_file_size` would either wedge
  boot or (worse) let a truncated model through. Applies to both the
  new Candle path and the existing ONNX `get_model_path_for` /
  `get_tokenizer_path_for` callers (Codex LOW round-1 + MEDIUM round-2).

## [0.8.0] - 2026-04-20

Post-nanomem cleanup release. Closes the remaining items from the
nanomem-inspired follow-up handoff (Items 2, 3, 4, 5, 6, and the
Item 2 PRAGMA-detection NIT), plus a verification-gate write-up for
F-track. Item 2 is a one-way schema migration; downgrade is not
supported.

### Changed

- **Schema migration (one-way): chunks UNIQUE is now tenant-scoped** (Item 2). The legacy `UNIQUE(segment_id, ordinal)` was a pre-existing bug: `PersistentStore::next_segment_id()` allocates segment IDs per-tenant, so tenant_a's (segment=1, ordinal=0) and tenant_b's first write would collide on the global UNIQUE and `INSERT OR REPLACE` would silently overwrite the first-written row's metadata. The constraint is now `UNIQUE(tenant_id, segment_id, ordinal)`. Legacy databases are migrated automatically on open via a rebuild-in-place in `migrate_chunks_unique_to_tenant_scoped`. `segment_id` is no longer globally meaningful; `MetadataStore::get_by_segment` takes a `tenant_id` parameter accordingly, and `idx_chunks_segment` is now `(tenant_id, segment_id)`.
- **PostWriteEvent moved to its own module** (Item 6). Relocated from `mcp::handlers` to `mcp::post_write_hooks`, with `PostWriteEvent::from_imported_chunk` adapter. Public path `memd::mcp::PostWriteEvent` preserved; `memd::mcp::handlers::PostWriteEvent` retained via `pub use` re-export.
- **chunks UNIQUE migration detection uses `PRAGMA index_list` / `pragma_index_info`** (Item 2 NIT). `migrate_chunks_unique_to_tenant_scoped` now inspects the concrete UNIQUE index columns SQLite reports instead of grepping the `CREATE TABLE` text. Tolerates DDL variations (`UNIQUE (segment_id, ordinal)` with a space, `CONSTRAINT name UNIQUE(...)`, casing) that the substring match would have missed — a legitimate memd legacy DB written by a non-default client would otherwise skip the rebuild. Filter is `origin='u'` only, so a foreign chunks schema with a manual `CREATE UNIQUE INDEX` (origin='c') is left alone (Codex NIT round-1 MEDIUM).
- **OMF supersession round-trip** (Item 5). `import_omf` now preserves the supersession graph across memd↔memd round-trips. Previously the importer dropped `extensions.memd.lifecycle.supersedes` / `superseded_by` because source-side chunk IDs had no meaning in the destination. The importer now runs two passes: pass 1 writes each chunk (assigning a fresh dest-side id) and records a `source_chunk_id → target_chunk_id` map; pass 2 replays each `supersedes` edge through `MetadataStore::atomic_supersede` using the translated dest-side IDs. Edges whose other side is not present in the document (partial export) are silently dropped, never translated to dangling IDs. Edge replay is gated by the F3 trust gate — untrusted sources (`source.app != 'memd'` or version mismatch) cannot fabricate supersession state. A pre-flight invariant check rejects malformed trusted documents (forked supersession graph, duplicate source chunk_ids, non-UUID `chunk_id` / `supersedes` / `superseded_by`) before any chunk is written, so a broken input can't half-apply. `preview_omf_import` runs the same pre-flight so it shares fail-closed behavior with real import.

### Added

- **`memd export-markdown` auto-discovers `--data-dir` from `.memd/tenant_scope.json`** (Item 4). When invoked from inside an initialized project, picks up the daemon's data dir from the nearest-ancestor scope config written by `memd init` (without forcing `--data-dir` on every call). Discovery *augments* the `$HOME/.memd/data` fallback rather than replacing it, so the containment guard still refuses an outdir inside the home default even if discovery finds a different path. Also persists `data_dir` in `tenant_scope.json` for every scope mode (not just `global`), and absolutizes `--memd-data-dir` at init time.
- **G3 symlink-escape guard on `memd export-markdown` writes** (Item 3). `reject_if_any_symlink_inside_outdir` refuses before any filesystem write if any existing component inside outdir is a symlink — closing a pre-existing-symlink-plant escape where an attacker could redirect the write to a path of their choosing. Outdir itself can be a symlink (users may legitimately point at a symlinked exports dir); only components BELOW outdir are refused.

### Migration notes

- **Downgrade is one-way.** A pre-Item-2 binary opening a post-migration database would see the new `UNIQUE(tenant_id, segment_id, ordinal)` constraint but its code paths still treat `segment_id` as globally unique — no silent corruption is expected, but any compaction / audit tooling from the older binary could misread tenant-scoped segments as cross-tenant rows. Roll forward, not back.

## [0.7.0] - 2026-04-18

Track C release. Lands the temporal-fields + sweep + expiry-control surface on top of
Track A (lifecycle overlay) and Track B (visibility filter + HNSW exclusion) from 0.6.0.
Every task (C1–C6) went through Codex CLI review; all HIGH findings on races,
boundary semantics, and TOCTOU were addressed before sign-off.

### Added

- **`memory.add(_batch)` accepts `expires_at_ms`, `review_after_ms`, and `mode`** (C1).
  Temporal overlay fields are persisted via `PersistentStore::add_chunk_with_lifecycle`
  in the same logical op as the chunk write, so retrieval visibility (C2) and the sweeps
  (C3/C4) see them on the next read. `mode` is accepted now for Track E forward-compat.
- **`memory.set_expiry` MCP tool** (C6). Atomic out-of-band update of a chunk's
  `expires_at_ms` / `review_after_ms`. JSON triple-state: field absent → leave, `null` →
  clear, integer → set. Refuses deleted / unknown / cross-tenant chunks via a guarded
  SQL `UPDATE` with rowcount check, so `{"updated": true}` is a load-bearing claim.
- **`ExpirySweep`** (C3) — compaction-level sweep that promotes rows past their
  `expires_at_ms` to `status=Expired`. Uses a race-safe `mark_expired_if_final` that
  re-checks both the status and retention predicate at UPDATE time.
- **`HistoryPromotion`** (C4) — compaction-level job that demotes long-stale
  Superseded/Expired rows to `MemoryTier::History` based on the overlay-idle clock
  (`lifecycle_updated_at_ms`, NOT `timestamp_created`). Uses a guarded
  `promote_to_history_if_stale` with three predicates re-checked at UPDATE time.
- **Wired into `CompactionRunner::run_compaction`** (C5). Both sweeps run before the
  excluded-ID gather so their transitions flow into Track B2's HNSW-rebuild exclusion.
  `CompactionResult` gains `expired_count` and `promoted_count`. `CompactionConfig`
  gains `expiry_sweep_enabled`, `history_promotion_enabled`, and `history_promotion_age_ms`
  (default 90 days). The runner owns a single centralised `tenant_memory_version` bump
  per cycle, narrowly scoped to C3/C4 overlay transitions.

### Changed

- **Lazy retrieval hiding matches the sweep boundary.** `list_expired_before` now uses
  `expires_at_ms <= now` (was `<`), mirroring `VisibilityPolicy::is_visible_at`.

### Notes

- `VisibilityPolicy::is_visible_at` was already in place in 0.6.0 and handles Track C2's
  lazy retrieval hiding. 0.7.0 adds end-to-end tests for the MCP path now that C1 writes
  the overlay field.
- `memory.set_expiry` guard chain: preflight-via-atomic-UPDATE with
  `AND status != 'deleted'` and tenant filter, so TOCTOU windows are closed.

### Test coverage

21 new tests in `crates/memd/tests/expiry_and_history.rs` plus two lib tests in
`crates/memd/src/compaction/` and one in `crates/memd/src/store/persistent.rs`. Race
guard paths are exercised directly (not just through the pre-filter) for both
`mark_expired_if_final` and `promote_to_history_if_stale`.

## [0.6.0] - 2026-04-18

Lifecycle release. Landed Track A of the nanomem-inspired features plan — atomic chunk
supersession, lifecycle overlay on `memory.get`, and the SQLite overlay columns that
underpin the remaining tracks (B–G). Addresses all five findings from Codex CLI's round-1
review of the merge (2 HIGH + 2 MEDIUM + 1 LOW).

### Breaking changes

- **`memory.get` response shape changed.** Pre-0.6 returned a bare `MemoryChunk | null`.
  0.6 returns a discriminated envelope so callers can distinguish "not found" from
  "hidden by visibility policy" and retrieve lifecycle metadata without a second round
  trip:
  - `{found: false}` — chunk does not exist for this tenant.
  - `{found: true, chunk, lifecycle, status}` — chunk is visible; payload + overlay
    included.
  - `{found: true, hidden: true, status, tier, hidden_reason}` — chunk exists but is
    hidden by the lifecycle visibility policy. `hidden_reason` is one of `superseded`,
    `expired`, `history`, or `error`. For `superseded`, `expired`, and `history` the
    caller can retry with the matching `include_*` flag to unhide; `error` has no
    include knob (it flags a corrupted / unrecoverable row). `deleted` rows never
    surface through `memory.get` at all — they return `{found: false}`.
  - Clients pinned to the old shape must update. The in-tree `evals/harness` conformance
    suite was updated in this release.

### Added

- **`memory.supersede` MCP tool.** Atomically replaces an existing chunk with a new one
  and records the supersession edge in a single SQLite transaction. Old chunk becomes
  `status=Superseded`, new chunk gets `supersedes = old_id` and the old chunk gets
  `superseded_by = new_id`. Preflight rejects missing / deleted / non-head `old_id`,
  detects pre-existing cycles in the `superseded_by` chain before touching disk, and
  rolls back compensating-tombstone-style if the link transaction fails after the new
  chunk is already written.
- **Lifecycle overlay on `memory.get`.** `include_superseded`, `include_expired`, and
  `include_history` flags control whether hidden rows surface with their payload. The
  response always advertises `hidden_reason` on a hidden envelope so agents can retry
  with the right knob.
- **Lifecycle metadata columns + access-path indexes on `chunks`.** Seven new
  columns added by the A3 migration: `tier`, `supersedes`, `superseded_by`,
  `expires_at_ms`, `review_after_ms`, `lifecycle_updated_at_ms`, `canonical_text`
  (`status` already existed). Four new indexes: `idx_chunks_expiry` (partial, on
  `expires_at_ms IS NOT NULL`), `idx_chunks_supersedes` (partial, on
  `supersedes IS NOT NULL`), `idx_chunks_tier_status` (full), and
  `idx_chunks_canonical` (partial, on `canonical_text IS NOT NULL`). Legacy rows
  migrate with safe defaults (`Final` / `LongTerm`).
- **`ChunkStatus::Superseded` and `ChunkStatus::Expired`** with fail-closed `FromStr`.
- **Visibility policy primitives.** `VisibilityPolicy` with `is_visible` / `is_visible_at`;
  `MemoryTier`, `LifecycleDelta` (triple-state clear semantics), `ResolvedChunk`.

### Fixed

- **HIGH-1: `memory.supersede` is now actually atomic.** Pre-fix, a crash between the
  new-chunk write and the `atomic_supersede` SQL transaction left an orphan replacement
  visible in retrieval with no supersession edge. Fix: `atomic_supersede`'s UPDATE now
  carries a `superseded_by IS NULL` guard (head-only at SQL level), and
  `supersede_chunk` rolls the orphan back via the full `Store::delete_chunk` path if
  the link transaction fails after the new chunk was persisted. That path appends a
  WAL delete record (so restart recovery cannot resurrect the orphan), marks the
  segment tombstone, removes the chunk from the hybrid / sparse / dense / tiered
  indexes, and invalidates the cache — leaving no trace of the failed write.
- **HIGH-2: `memory.get` wire envelope (see Breaking changes above).**
- **MEDIUM-3: hidden `memory.get` envelope omitted the hiding cause.** Callers now get
  `hidden_reason` ∈ {`superseded`, `expired`, `history`, `error`} with precedence
  matching `VisibilityPolicy::is_visible_at` exactly (status → tier → wall-clock
  expiry) so the flag it names actually unhides the row.
- **MEDIUM-4: `supersede_chunk` accepted any non-deleted `old_id`.** A double-supersede
  on the same chunk used to fork the graph with two live successors. Now enforced as a
  two-layer invariant: preflight rejects when `old_id.superseded_by` is already set;
  SQL-layer UPDATE filters on `superseded_by IS NULL` so a concurrent race past the
  preflight rolls back rather than overwriting.
- **LOW-5: cycle detection in `supersede_chunk`.** Replaced bounded 64-hop
  return-to-start walk with a `HashSet<ChunkId>` visited-set scan that catches any
  revisit at any depth. A 65-hop cycle, or a cycle that re-enters mid-chain, used to
  pass; both now fail.

### Testing

- Four new lifecycle tests: `supersede_chunk_rejects_double_supersede_on_same_old_id`,
  `supersede_chunk_detects_non_start_cycle_mid_chain`,
  `supersede_chunk_walks_long_chain_past_old_64_hop_bound`,
  `memory_get_hidden_envelope_carries_hidden_reason`.
- Full suite: 699 tests pass (up from 695 on the 0.5.0 merge base).

### Review

- Codex CLI round-1 review of the Track A merge (session
  `019da31a-5f94-7942-ab14-40d84012f7b5`): REQUEST_CHANGES, 5 findings. All addressed
  in this release and re-reviewed; artifact `019da326-b1b3-7262-8d68-3734fdcac6f7`
  in memd.

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
  hybrid is off). New `PersistentStoreConfig::backfill_hnsw_on_startup` (opt-in via
  `MEMD_BACKFILL_HNSW_ON_STARTUP`) triggers a one-shot background task on the ambient
  Tokio runtime at `open()`. Non-blocking once scheduled; semantic search on older chunks
  degrades until the task completes.
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
