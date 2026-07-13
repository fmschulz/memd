# Handoff: Consolidation - Milestone 2

**Date:** 2026-07-12
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006b`

## Context & Status

Milestones 1 and 2 of `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` are implemented in the uncommitted worktree. Consolidation is now scope-safe, journaled, crash-recoverable, idempotent for an exact source set, and atomic at promotion. The Milestone 2 change gate passes. No commit or push has been made.

The next implementation slice is Milestone 3: unify write admission, trust assignment, and consolidation validation. Do not reopen the consolidation protocol unless a new failing invariant requires it.

## Technical Implementation

### Work Completed

- Added the internal-only `Candidate` lifecycle and public-surface visibility exclusions (`crates/memd/src/types.rs`, `crates/memd/src/store/memory.rs`, CLI/report/export/task query paths).
- Added typed consolidation journals, entries, and relational lineage with idempotent legacy migration (`crates/memd/src/consolidate/journal.rs`, `crates/memd/src/store/metadata/sqlite.rs`).
- Added journal-first execution, validation, guarded atomic promotion, exact-input settlement, bounded recovery, poison-run isolation, and durable sparse-cleanup retry (`crates/memd/src/consolidate/service.rs`).
- Routed CLI and episode consolidation through the service and added session-start recovery before context refresh (`crates/memd/src/cli/consolidate.rs`, `crates/memd/src/ops/mod.rs`, `crates/memd/src/cli/session_start.rs`).
- Fixed WAL replay so payload-coordinate repair cannot overwrite authoritative SQLite lifecycle state (`crates/memd/src/store/persistent.rs`).
- Added public-surface, scope, concurrency, migration, BM25 cleanup, and 8-boundary real-SIGKILL recovery coverage (`crates/memd/tests/candidate_public_surfaces.rs`, `crates/memd/tests/consolidation_recovery.rs`, related integration tests).
- Updated CLI/self-improvement reference material, the installed skill contract, and the changelog (`docs/cli-reference.md`, `docs/self-improvement.md`, `memd-skill/SKILL.md`, `CHANGELOG.md`).

### Outcomes

- **What worked:** `cargo test --workspace` passed; the main library ran 961 tests with 956 passing and five model-download tests ignored. The 15-test recovery target includes real SIGKILL at all eight durable boundaries. Formatting, workspace check, normal all-target clippy, strict MkDocs, and diff checks pass. Three independent read-only reviews ended with no blocking finding.
- **What didn't:** Crash testing revealed that pre-existing WAL replay could revert `Final` to `Candidate` and resurrect other terminal lifecycle states. The repair path now preserves SQLite lifecycle fields. Review also exposed poison-run starvation, live-run recovery races, stale settlement, non-durable sparse cleanup, and post-commit misclassification; all have regression coverage and are fixed.

### File Map

| File | Change | Notes |
|---|---|---|
| `crates/memd/src/consolidate/journal.rs` | Added | Run IDs, states, entries, lineage, promotion outcomes |
| `crates/memd/src/consolidate/service.rs` | Added | Execute, settle, validate, promote, recover, sparse cleanup |
| `crates/memd/src/store/metadata/sqlite.rs` | Modified | Schema migration and guarded transactional repository methods |
| `crates/memd/src/store/persistent.rs` | Modified | Candidate persistence hooks, lifecycle-safe WAL replay, index refresh |
| `crates/memd/src/cli/consolidate.rs` | Modified | Region fixes plus shared staged service |
| `crates/memd/src/ops/mod.rs` | Modified | Episode consolidation uses the shared service |
| `crates/memd/src/cli/session_start.rs` | Modified | Bounded recovery through a short-lived writer |
| `crates/memd/tests/consolidation_recovery.rs` | Added | 15 recovery, concurrency, cleanup, and SIGKILL tests |
| `tasks/METHODS.md` | Modified | Exact implementation and proof record |

## Key Decisions

| Decision | Rationale |
|---|---|
| Candidate text is hidden from every public read path | Partially written or unvalidated synthesis must never influence an agent |
| Same-project runs use `supersedes`; tenant-wide runs use `derives_from` | A tenant-wide replacement cannot safely erase project-visible facts |
| SQLite lifecycle metadata is authoritative over WAL payload metadata | WAL replay repairs storage location; it must not roll back later lifecycle commits |
| Recovery ignores runs updated in the last 30 seconds | Prevent session-start from claiming a live writer while still recovering stale work |
| Sparse cleanup has its own durable completion marker | SQLite promotion is atomic, while Tantivy deletion is a separate retryable side effect |
| Exact source set, tenant, scope, and relation define run identity | Concurrent and repeated requests converge without depending on prompt or model wording |

## Knowledge Capture

### Lessons Learned

- Test crashes at WAL append and metadata insert separately; a generic “candidate persisted” failpoint misses lifecycle-replay defects.
- After a transaction may have committed, classify errors from the re-read journal state rather than from the failed call site.
- Index-disabled recovery must distinguish “no sparse index exists” from “a sparse index exists but this handle did not open it.”

### Gotchas

- `ChunkStatus` embedded in segment payloads can be stale. Public gets overlay the current metadata status.
- `finish_pending_sparse_cleanup` must remain bounded and must not scan lineage when a sparse index exists on disk but the current recovery handle lacks a writer.
- Normal clippy passes with 107 warnings, matching the frozen baseline. Strict clippy is still an overall-goal gate, not a completed Milestone 2 gate.
- The worktree is deliberately uncommitted. Ask Chef before any commit or push.

## Moving Forward

### Next Steps

1. Start Milestone 3 RED tests for equivalent admission decisions across CLI, call, batch, episode, and consolidation inputs.
2. Extract a protocol-neutral typed `PreparedWrite` service and route each write surface through it.
3. Extend consolidation output validation with agent action, evidence, confidence, exact source coverage, and bounded raw-response audit storage.
4. Run independent review and the same workspace/change gate before moving to outcome-attributed retrieval.

### Blockers

- None for Milestone 3. Strict-clippy baseline debt and the remaining benchmark/manuscript milestones are still open in the overall goal.
