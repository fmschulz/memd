# Handoff: Consolidation - Milestone 1

**Date:** 2026-07-12
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006b`

## Context & Status

Milestone 1 of `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md`
is implemented in the uncommitted working tree. Consolidation now selects old
chunks retrieved after the watermark, applies current lifecycle visibility to
both region inputs, rejects invalid search-log timestamps and foreign-project
hits, and preserves project sources during tenant-wide synthesis.

The change gate is valid for the current tree. No commit or push was made.
Milestone 2 is next: replace immediate synthesis and tombstoning with a run
journal, hidden candidates, relational lineage, atomic promotion, and crash
recovery.

## Technical Implementation

### Work Completed

- Added `RegionReason::{NewWrite, RecentHit}` and reason-specific watermark
  handling in `collect_region` (`crates/memd/src/cli/consolidate.rs`).
- Applied `VisibilityPolicy::default()` to the current lifecycle overlay for
  both recent writes and search-log candidates.
- Made search-log event parsing fail closed and region ordering deterministic.
- Made the SQLite project-list scan deterministic at equal timestamps
  (`crates/memd/src/store/metadata/sqlite.rs`).
- Changed tenant-wide output to `derives_from:<csv>` without source
  tombstones. Project-scoped runs retain `supersedes:<csv>` and tombstones.
- Added 14 consolidation tests covering watermark revival, lifecycle changes,
  project isolation, invalid log names, scan ordering, scope visibility, and
  an unchanged rerun.
- Updated the changelog, CLI reference, self-improvement guide, and bundled
  memd skill.

### Outcomes

- **What worked:** Regression-first tests reproduced five faults. All targeted
  tests and the workspace gate now pass. Two independent review passes found
  lifecycle gaps; both region paths now use the central visibility policy.
- **What didn't:** The first independent reviewer ran `memd add` despite a
  read-only prompt. It changed no source file. A later prompt explicitly
  prohibited memd and produced no memd write. The equal-timestamp boundary
  test passed before the SQL tie-breaker because the current SQLite plan
  happened to return UUID order; treat it as contract coverage, not a RED
  reproduction.

### File Map

| File | Change | Notes |
|---|---|---|
| `crates/memd/src/cli/consolidate.rs` | Modified | Region reasons, lifecycle checks, scope-safe lineage, tests |
| `crates/memd/src/store/metadata/sqlite.rs` | Modified | Stable `timestamp_created`, `chunk_id` ordering |
| `CHANGELOG.md` | Modified | Unreleased behavior fixes |
| `docs/cli-reference.md` | Modified | Project and tenant-wide semantics |
| `docs/self-improvement.md` | Modified | Source visibility and lineage |
| `memd-skill/SKILL.md` | Modified | Agent-facing consolidation contract |
| `tasks/METHODS.md` | Modified, ignored | RED/GREEN commands, review, and gate evidence |
| `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` | Modified, ignored | Milestone status and open idempotency item |

## Key Decisions

| Decision | Rationale |
|---|---|
| Tenant-wide synthesis uses `derives_from` and retains project sources | A tenant-only lesson is not directly visible to project-scoped search and cannot replace project-visible facts. |
| Both region inputs use current lifecycle visibility | Immutable payload status can lag corrections, expiry, errors, and tier changes recorded in metadata. |
| Fresh-event cross-run dedup waits for Milestone 2 | A tag scan would be a temporary, bounded approximation. The run journal and lineage table can enforce source-set idempotency directly. |
| Untracked handoff archives stay outside the source diff | `docs/handoffs/` would be published by MkDocs if committed; decide its repository home during the hygiene milestone. |

## Knowledge Capture

### Lessons Learned

- Event time and chunk creation time are separate eligibility signals.
- Every destructive lineage edge must be checked against current lifecycle
  metadata, not the immutable payload.
- Deterministic in-memory sorting does not define which rows cross a database
  `LIMIT`; the query needs the same tie-breaker.

### Gotchas

- Tenant-wide sources remain active. A fresh later search event over the same
  source set can still create another derived lesson. Milestone 2 must make
  this idempotent through run and lineage records.
- `collect_region` now performs a lifecycle-aware lookup for every row returned
  by `list_chunks_for_project`. Keep correctness during the state-machine
  refactor, then measure whether a resolved-list API is warranted.
- Strict clippy remains a frozen baseline failure. Normal clippy passes with
  107 existing all-target warnings.
- The first evidence memory was downgraded to priority 7 because `Inspect` was
  not in the write gate's action-verb allowlist. The corrected priority-8
  record is `019f59dc-de89-78e1-99dd-fdf09afb2683`.

## Moving Forward

### Next Steps

1. Write failing migration and visibility fixtures for `ChunkStatus::Candidate`,
   `consolidation_runs`, `consolidation_entries`, and `memory_lineage`.
2. Implement typed run-state repository methods and exact source-set
   idempotency, reusing the compensation and SQLite transaction patterns in
   `PersistentStore::supersede_chunk`.
3. Add staged validation, atomic same-project promotion, rejection, recovery,
   and crash injection; route episode consolidation through the same service.
4. Rerun the independent review and workspace/change gates before moving to
   outcome-attributed retrieval.

### Blockers

- None.
