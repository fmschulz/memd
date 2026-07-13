# Handoff: memd - Code and Benchmark Review

**Date:** 2026-07-12
**Branch:** main
**HEAD:** 1b6006b

## Context & Status

Reviewed the memd 1.4.0 source tree, its iterative self-improvement mechanisms, repository hygiene, and the benchmark/manuscript in `../memd-bench`. No product or benchmark source was changed. The Rust workspace tests pass; strict clippy does not. Three independent manuscript-review lanes reproduced the main local LoCoMo summaries and recommended major revision.

## Technical Implementation

### Work Completed

- Traced consolidation, hit statistics, explicit feedback, `memory.md` ranking, write admission, and session-start behavior.
- Checked the LoCoMo and CodeIR harnesses, local artifacts, manifests, statistical analysis, and manuscript assertions.
- Inventoried large modules, broken symlinks, stale provenance files, duplicated benchmark surfaces, and ignored disk use.
- Recorded exact commands and findings in `tasks/METHODS.md`; recorded implementation order in `tasks/todo.md`.

### Outcomes

- **What worked:** `cargo fmt --all -- --check`, `cargo check --workspace`, and `cargo test --workspace` passed. Local primary LoCoMo table values match their JSON summaries.
- **What failed:** `cargo clippy --workspace --all-targets -- -D warnings` reported 83 library errors and 108 all-target diagnostics. Clean-clone reproduction of the manuscript is impossible because named source artifacts are ignored or external.

### File Map

| File | Change | Notes |
|---|---|---|
| `tasks/todo.md` | Modified, ignored | Ranked implementation order and completion state |
| `tasks/METHODS.md` | Modified, ignored | Commands, results, and confirmed findings |
| `tasks/lessons.md` | Modified, ignored | Reusable design lessons |
| `memory.md` | Generated, ignored | Session-start memory view; exposed a cross-section duplicate |
| `docs/handoffs/2026-07-12_memd-code-benchmark-review.md` | Added | This handoff |

## Key Decisions

| Decision | Rationale |
|---|---|
| Fix consolidation visibility and selection before adding new learning features | Current behavior can exclude freshly retrieved old lessons and hide project sources behind tenant-only replacements. |
| Replace exposure reinforcement with outcome attribution | Every rendered result is currently marked selected, so the +8 score is popularity rather than utility. |
| Stage and validate synthesized lessons before promotion | Consolidation writes high-priority model output and tombstones sources without an atomic run or task-level validation. |
| Treat the manuscript as major revision | The local arithmetic is sound, but the causal controls, inference unit, comparator scope, and artifact provenance do not support the present broad claims. |

## Knowledge Capture

### Lessons Learned

- Recent retrieval and recent creation need separate eligibility rules.
- A replacement must be visible in every scope where its sources were visible before the sources are hidden.
- Agent learning needs retrieval episodes tied to verifier or task outcomes; rank exposure alone creates a popularity loop.

### Gotchas

- Tenant-wide consolidation already warns that project-scoped searches cannot see its output, yet it still tombstones project sources.
- `memory.md` calculates duplicate counts separately for project and machine-wide sections, so the same chunk can appear in both while the metric reports zero.
- `../memd-bench` has its own policy that forbids external memd access and editing; all review work there remained read-only and repo-local.

## Moving Forward

### Next Steps

1. Add regression tests and fixes for consolidation hit eligibility and cross-scope source visibility.
2. Introduce staged, idempotent consolidation with lineage and promotion/rollback.
3. Add retrieval episodes and task outcomes, then evaluate the loop longitudinally against raw-memory and no-memory controls.
4. Repair benchmark provenance and controls, rerun affected arms, and revise manuscript claims.
5. Remove or archive stale repository surfaces, then split the five largest Rust modules.

### Blockers

- None. The next work is implementation and rerunning affected benchmarks; no unresolved discovery blocker remains.
