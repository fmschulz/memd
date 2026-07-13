# Handoff: memd - Reliable Adaptive Memory Goal

**Date:** 2026-07-12
**Branch:** main
**HEAD:** 1b6006b

## Context & Status

Converted the code, self-improvement, benchmark, manuscript, and hygiene review into an executable implementation goal. Planning is complete; implementation has not started. The goal lives at `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` and is referenced by the open item in `tasks/todo.md`.

## Technical Implementation

### Work Completed

- Defined the consolidation state machine, scope rules, lineage schema, and admission contract.
- Defined retrieval episodes, explicit outcome events, privacy rules, expanded-pool reranking, and shadow evaluation.
- Sequenced product fixes, longitudinal evaluation, benchmark provenance, reruns, manuscript revision, cleanup, and release gates.
- Added acceptance criteria and proof requirements for every milestone.

### Outcomes

- **What worked:** Every confirmed review finding maps to a milestone and verification gate.
- **What didn't:** No implementation was attempted; current failures and benchmarks remain unchanged.

### File Map

| File | Change | Notes |
|---|---|---|
| `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` | Added, ignored | Authoritative implementation goal |
| `tasks/todo.md` | Modified, ignored | Open pointer to Milestone 0 |
| `docs/handoffs/2026-07-12_memd-reliable-adaptive-memory-goal.md` | Added | Resumption context |

## Key Decisions

| Decision | Rationale |
|---|---|
| Add a hidden candidate lifecycle and journaled promotion | A crash must leave sources visible and synthesis hidden until commit. |
| Treat tenant-wide synthesis as `derives_from` | A tenant-only replacement cannot safely supersede project-visible sources. |
| Reinforce only explicit use or harm | Rendering a chunk is exposure, not evidence that it helped. |
| Use immutable phase manifests and a pinned open answer model | A clean clone must reproduce the primary benchmark without hidden model defaults or overwritten provenance. |
| Delay module splitting until behavior is green | Mechanical movement must not obscure correctness or benchmark changes. |

## Knowledge Capture

### Lessons Learned

- The existing `supersede_chunk` compensation and SQLite transaction are the right base for multi-source promotion.
- The separate `memory.consolidate_episode` add/delete path must join the same consolidation service.

### Gotchas

- `ChunkStatus::Draft` is not a safe substitute for a candidate until every visibility path is audited.
- The benchmark repository forbids external memd access and remains read-only until implementation is explicitly started there.
- This checkout is on `main`; branch before source edits.

## Moving Forward

### Next Steps

1. Start Milestone 0: branch, freeze baselines, and add failing regressions for recent-hit selection and tenant-wide source visibility.
2. Review the fixed data-model decisions before writing migrations.
3. Implement the region/scope hotfix independently of the larger consolidation redesign.

### Blockers

- None. Implementation awaits Chef's direction to begin.
