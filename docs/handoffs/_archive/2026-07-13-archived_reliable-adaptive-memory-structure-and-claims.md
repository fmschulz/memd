# Handoff: Reliable Adaptive Memory - Structure and Claims

**Date:** 2026-07-13
**Branch:** feat/reliable-adaptive-memory
**HEAD:** 1b6006b

## Context & Status

The reliability/adaptive-memory goal remains active. Milestone 10 is complete:
the five oversized Rust surfaces were split along their existing subsystem
boundaries, each focused suite passed, and independent Cursor reviews approved
the final boundaries. The provenance-incomplete June 2026 BEIR notebook was
removed from the active evaluation surface and preserved as a documented legacy
snapshot.

The benchmark manuscript now has a fail-closed claim registry and assertion
checker, but its nine quantitative claim groups remain pending until immutable
claim-bearing bundles exist. Outcome ranking remains shadow-only because the
frozen longitudinal run failed the recall non-regression gate.

No commit, push, version bump, public benchmark run, or release occurred. The
worktree is intentionally large and uncommitted. The release goal cannot advance
to clean-clone evidence until the source has a committed identity; claim-bearing
runs also need pinned answer and judge endpoints.

## Technical Implementation

### Work Completed

- Split CLI dispatch tests and `memory_md` state, collection, ranking,
  rendering, evaluation, and action logic (`crates/memd/src/cli/`).
- Split SQLite metadata into schema, chunks, lifecycle, feedback, episodes,
  consolidation, task, and test modules while keeping one `MetadataStore`
  implementation (`crates/memd/src/store/metadata/sqlite/`).
- Split persistent storage into read, write, retrieval, lifecycle, indexing,
  recovery, and test modules (`crates/memd/src/store/persistent/`).
- Split operation handlers into add, search, lifecycle, feedback, task,
  context, maintenance, shared types, and tests while preserving `memd::ops`
  re-exports (`crates/memd/src/ops/`).
- Added a fail-closed manuscript claim registry and checker in the sibling
  benchmark repository (`../memd-bench/manuscript/claims.v1.json`,
  `../memd-bench/manuscript/check_assertions.py`).
- Hardened benchmark bundle path/inventory validation
  (`../memd-bench/benchmarks/bundle_artifacts.py`).
- Archived the incomplete BEIR notebook snapshot and documented its missing
  candidate-source provenance (`evals/legacy/beir-2026-06/`).
- Updated the goal, todo, and methods records with the exact proof
  (`tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md`, `tasks/todo.md`,
  `tasks/METHODS.md`).

### Outcomes

- **What worked:** focused suites passed for CLI dispatch (48), `memory_md`
  (39), SQLite (43), persistent storage (44), and ops (90). The post-split
  workspace gate passed format, check, strict all-target Clippy, 974 memd
  library tests with five intentional model-download ignores, every integration
  target, 64 evaluation-harness library tests, five harness binary tests, and
  all doctests.
- **What worked:** Cursor approved every structural boundary. Verified findings
  were fixed before rerunning the relevant focused suite and strict Clippy.
- **What worked:** benchmark lint and all 36 Python benchmark tests passed;
  manuscript assertion audit passed with nine explicit pending groups.
- **What did not:** Cursor plan-mode reviews often returned empty after about
  five minutes. One concise ask-mode retry produced a verdict; do not keep
  retrying a silent route beyond the two-attempt limit.
- **What did not:** manuscript verification correctly exits nonzero because no
  immutable bundle binds the pending claims.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/cli/mod.rs` | Modified | Dispatch shell; tests moved to `cli/tests.rs`. |
| `crates/memd/src/cli/memory_md/` | Added | Action, collection, evaluation, ranking, rendering, state, and tests. |
| `crates/memd/src/store/metadata/sqlite/` | Added | Explicit schema/chunk/lifecycle/feedback/episode/consolidation/task boundaries. |
| `crates/memd/src/store/persistent/` | Added | Read/write/retrieval/lifecycle/indexing/recovery boundaries. |
| `crates/memd/src/ops/` | Added | Handler families, shared public types, and tests. |
| `evals/legacy/beir-2026-06/` | Added/Moved | Historical only; missing `/tmp/memd-bench/candidate-final.json` provenance. |
| `../memd-bench/manuscript/claims.v1.json` | Added | Nine claim groups remain pending. |
| `../memd-bench/manuscript/check_assertions.py` | Added | Audit passes; verify fails closed until bundle binding. |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Keep outcome-v1 serving in shadow mode. | The frozen longitudinal full-loop treatment failed the recall non-regression gate. |
| Treat the June 2026 BEIR notebook as legacy, not evidence. | Its candidate JSON points to an unavailable temporary source, so the result cannot be regenerated. |
| Make manuscript verification fail on every pending claim. | Quantitative prose must resolve to immutable artifacts rather than inherit authority from the draft. |
| Preserve public module paths with parent re-exports and trait delegates. | Structural cleanup must not change callers or create a second `MetadataStore` implementation. |
| Do not fabricate a benchmark source commit or release version. | The source is uncommitted, 1.4.0 already exists on crates.io, and release publication requires Chef's approval. |

## Knowledge Capture

### Lessons Learned

- Mechanical module moves need explicit checks for documentation attributes and
  derives at range boundaries; compilation caught several comments left on the
  next item.
- A focused test suite plus strict Clippy is effective for structural moves, but
  independent review still found ownership issues that tests could not express.
- A single Rust trait implementation can retain its public contract while
  delegating SQL bodies to responsibility-specific inherent methods.

### Gotchas

- The warm worker was restarted on the current source. Earlier migration errors
  came from a stale pre-migration worker, not from the migration itself.
- `main` version bumps auto-create tags, which trigger crates.io and GitHub
  release workflows. Commit/push permission does not imply release permission.
- The sibling benchmark repository has its own worktree and must retain its
  clean-source provenance discipline. Do not set `MEMD_SOURCE_COMMIT` to the
  current uncommitted HEAD.
- Ignored notebook exports remain under `../memd-bench/manuscript/notebooks/`;
  they are stale local outputs, but deletion needs Chef's approval.

## Moving Forward

### Next Steps

1. Obtain Chef's approval for a scoped commit plan, then create a clean memd
   source identity without mixing benchmark-result changes into the structural
   cleanup commit.
2. Configure pinned answer and judge endpoints and run the frozen CodeIR,
   LoCoMo, MemoryData, and LongMemEval protocols from a clean clone into one
   immutable validated bundle.
3. Bind all manuscript claims and figures to the bundle, run assertion verify,
   and obtain an independent scientific review.
4. Resolve the still-missing independent review of Milestones 4-6 through a new
   review route; do not retry the two timed-out route attempts unchanged.
5. Choose a release version, rerun release gates, show the exact commit file
   list and conventional message, and request separate approval for commit,
   push, and release-triggering merge.

### Blockers

- Clean-clone benchmark evidence requires a committed source identity.
- Claim-bearing public runs require pinned answer and judge endpoints.
- Version selection, commit, push, and release-triggering merge require Chef's
  explicit decisions and approvals.
