# Handoff: Longitudinal - Pre-commit Source Gate
**Date:** 2026-07-16
**Branch:** feat/reliable-adaptive-memory
**HEAD:** eaa9592

## Context & Status

The four-file retrieval and longitudinal correction has passed its local
pre-commit source gate. The gate found two additional regressions: punctuation-
only `_`/`-` queries entered exact rescue, and strict token equality broke
`parameter`/`parameters` task search. Both are fixed and covered by tests.

No file is staged. No commit, push, benchmark claim, tag, or release action has
occurred. The existing change-gate marker belongs to `eaa9592`; mark the new
HEAD only after Chef approves and the four-file commit exists.

## Technical Implementation

### Work Completed

- Require an alphanumeric character in normalized scorer and rescue tokens;
  retain boundary-anchored natural-word inflections of at least four characters
  (`crates/memd/src/store/mod.rs`, `crates/memd/src/ops/mod.rs`).
- Add punctuation-only, natural-inflection, production rescue-ordering, dotted-
  identifier, and task-search regressions (`crates/memd/src/store/mod.rs`,
  `crates/memd/src/ops/tests.rs`).
- Rerun the source-supported longitudinal oracle v2 through the unchanged seven-
  gate protocol (`evals/harness/src/suites/longitudinal.rs`).
- Record exact commands, failures, metrics, hashes, and non-claim status in
  `tasks/METHODS.md`; update the next action in `tasks/todo.md`.

### Outcomes

- **What worked:** All-target workspace check, strict Clippy, formatting, every
  workspace target, strict MkDocs, version consistency, memory usefulness,
  locked package/publish dry-runs, release build, longitudinal diagnostic, and
  frozen retrieval regression gate passed.
- **What didn't:** The first broad test run failed task-search inflection
  matching. Two Claude post-fix reviews timed out, and Cursor returned no text;
  final post-fix peer review remains a recorded residual.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/store/mod.rs` | Modified | Bounded lexical normalization, scoring, ordering, and tests |
| `crates/memd/src/ops/mod.rs` | Modified | O(k) full-window exact rescue and punctuation-only rejection |
| `crates/memd/src/ops/tests.rs` | Modified | Production rescue and task-search regressions |
| `evals/harness/src/suites/longitudinal.rs` | Modified | Source-supported oracle and identity v2 |
| `tasks/METHODS.md` | Modified, ignored | Commands, failures, results, and hashes |
| `tasks/todo.md` | Modified, ignored | Exact approval is next |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Match natural words only at token starts | Keeps `parameter`/`parameters` while preventing `ion`/`isolation`. |
| Reject punctuation-only identifier tokens before rescue | Avoids false matches and a needless 50,000-row scan. |
| Keep all current runs diagnostic | The source tree has no committed identity yet. |
| Delay the change-gate marker | The marker binds `HEAD`, not uncommitted files. |

## Knowledge Capture

### Lessons Learned

- Replacing substring matching with equality can break valid inflections. A
  token-boundary test must cover both the false-positive and prior true-positive
  contracts.
- A punctuation test must include identifier punctuation such as `_` and `-`.

### Gotchas

- The complete workspace command must run with shell fail-fast; an earlier
  targeted sequence continued after a formatting failure and is not gate proof.
- Diagnostic longitudinal run `1784191224723-7790f2dc275b` passed all seven
  gates but cannot support manuscript claims.
- Preserve the unrelated untracked handoffs and the ignored downloaded BEIR
  datasets.

## Moving Forward

### Next Steps

1. Obtain Chef's approval for the exact four-file commit and push, including
   acceptance of the missing post-fix peer verdict as a residual risk.
2. Stage only the four named files, inspect the staged diff, commit, mark the new
   HEAD with the change gate, then push the feature branch.
3. Build the committed source inside memd-bench and run the claim-bearing
   longitudinal, CodeIR, LoCoMo, MemoryData, and LongMemEval protocols with the
   pinned Qwen answerer and Mem0/Hindsight competitors.

### Blockers

- Git commit and feature-branch push need exact approval.
- The post-fix independent review has no written verdict after three failed
  bounded attempts; Chef must accept that residual or name another reviewer.
