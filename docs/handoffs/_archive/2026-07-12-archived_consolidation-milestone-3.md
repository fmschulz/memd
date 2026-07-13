# Handoff: Consolidation - Milestone 3

**Date:** 2026-07-12
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006b`

## Context & Status

Milestones 1–3 of the reliable adaptive-memory goal are complete in the
working tree. Milestone 3 unified write preparation, grounded consolidation
output in source evidence, recorded exact consolidator provenance, and
changed automatic consolidation from immediate promotion to staged review.
No commit or push has been made. The next unit of work is Milestone 4,
outcome-attributed retrieval episodes.

## Technical Implementation

### Work Completed

- Added protocol-neutral `PreparedWrite` with normalized tags, admission,
  retention, priority, lifecycle, and trust output
  (`crates/memd/src/write_service.rs`).
- Routed CLI, structured-operation, batch, supersession, OMF import, episode,
  and LLM-consolidation writes through that service.
- Required each synthesized entry to contain a concrete agent action, exact
  evidence IDs, and bounded confidence
  (`crates/memd/src/consolidate/prompt.rs`).
- Added journaled consolidator command/model/version, bounded raw-response
  audit artifacts, durable promotion intent, staged-run listing, explicit
  accept/reject, and conservative recovery
  (`crates/memd/src/consolidate/service.rs`,
  `crates/memd/src/store/metadata/sqlite.rs`,
  `crates/memd/src/cli/consolidate.rs`).
- Updated the CLI reference, self-improvement explanation, bundled skill, and
  upgrade notes for staging-by-default.

### Outcomes

- **What worked:** The full workspace test suite, normal all-target clippy,
  formatting, whitespace, and strict MkDocs gates pass. The recovery target
  has 21 passing tests, including eight real `SIGKILL` boundaries, concurrent
  accept/reject, transient audit recovery, tamper detection, and malformed
  audit rejection.
- **What did not:** The first broad independent review timed out after 900
  seconds without a verdict. A narrower review completed, its five findings
  were fixed, and a post-fix review found no blocker. Strict warning-free
  clippy still fails on the repository's existing warning backlog and remains
  a later final-goal gate.

### File Map

| File | Change | Notes |
| --- | --- | --- |
| `crates/memd/src/write_service.rs` | Added | Shared write-preparation contract. |
| `crates/memd/src/consolidate/journal.rs` | Added | Typed run, entry, and lineage records. |
| `crates/memd/src/consolidate/service.rs` | Added | Crash-safe stage, validate, review, promote, and recover flow. |
| `crates/memd/src/consolidate/prompt.rs` | Modified | Grounded response schema and rejection rules. |
| `crates/memd/src/store/metadata/sqlite.rs` | Modified | Journal schema, migrations, listing, intent, and atomic state transitions. |
| `crates/memd/src/cli/consolidate.rs` | Modified | Stage-by-default and review/list workflow. |
| `crates/memd/tests/consolidation_recovery.rs` | Added | State-machine, concurrency, tamper, and process-death proof. |
| `crates/memd/tests/write_preparation_contract.rs` | Added | Cross-surface policy equivalence. |
| `docs/cli-reference.md` | Modified | Consolidation review reference. |
| `docs/self-improvement.md` | Modified | Proposal, review, and promotion model. |
| `tasks/METHODS.md` | Modified | Exact commands, review findings, and gate results. |

## Key Decisions

| Decision | Rationale |
| --- | --- |
| Default and session-start consolidation stop at `Validated`. | Model synthesis is a proposal; validation alone must not authorize replacement. |
| Promotion intent is journaled before the atomic transaction. | Recovery can finish an accepted run without promoting a staged-only run. |
| Synthesized lessons start as `SemanticCandidate`. | A model cannot grant its own output verified trust. |
| Audit JSON parse failures are permanent validation failures. | A corrupt present artifact will not loop forever as a transient error. |
| Legacy validated rows cannot be promoted without the new audit artifact. | Upgrade behavior fails closed and preserves source visibility. |

## Knowledge Capture

### Lessons Learned

- Separate deterministic content validation from the authority to promote.
- Recovery must distinguish permanent validation failures from transient I/O
  and storage errors.
- Staging-by-default requires a discovery surface; background run IDs cannot
  disappear into detached-process output.

### Gotchas

- A process death between audit-file creation and journal insertion can leave
  one unreferenced file capped at 256 KiB. A later hygiene milestone can add a
  journal-aware sweep.
- The watermark advances when a proposal is staged. Sources from a rejected
  proposal re-enter through later retrieval evidence, not the NewWrite path.
- The worktree contains all Milestones 1–3 and remains intentionally
  uncommitted at baseline HEAD `1b6006b`.

## Moving Forward

### Next Steps

1. Run a Milestone 4 blindspot pass over current search logging, task/run
   artifacts, ranking, and benchmark fixtures; freeze the retrieval-episode
   schema before editing.
2. Add RED tests for explicit outcome attribution and prove retrieval exposure
   alone does not count as success.
3. Implement shadow-scored outcome features before any live ranking change,
   then benchmark the expanded candidate pool against frozen controls.

### Blockers

- None.
