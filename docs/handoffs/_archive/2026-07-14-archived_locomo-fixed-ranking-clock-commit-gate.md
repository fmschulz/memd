# Handoff: LoCoMo - Fixed Ranking Clock Commit Gate

**Date:** 2026-07-14
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `e59067e`

## Context & Status

LoCoMo v3 retrieval completed in the clean clone, but paired event-time
invariance retained one failure among 1,531 questions. The failure was a
rank-20 tie evaluated with two request-time recency clocks. memd and memd-bench
now implement a fixed ranking clock and fail-closed benchmark acknowledgement.
Both diffs pass their full local gates and independent review. They are not
committed; the next action requires Chef's approval of the exact file lists and
commit messages.

## Technical Implementation

### Work Completed

- Added optional `memory.search.ranking_time_ms` and propagated it through
  standard, debug, repaired-query, widened-scope, summary-preferred,
  in-memory, and persistent ranking paths.
- Fixed-clock searches exclude future feedback/outcomes and do not write
  retrieval episodes or usage-ledger entries.
- Debug-tier search now metadata-reranks the same over-fetched candidate pool
  as served search before truncating to `k`.
- memd-bench captures one clock per run, sends it to both arms, records it in
  the manifest, accepts replay through `--ranking-time-ms`, and requires an
  explicit `retrieval_episode_id: null` response.
- Centralized arm order as SHA-256 question-ID last-byte parity and recorded
  the exact rule in the protocol and manifest.

### Outcomes

- **What worked:** memd passed 984 library tests with five network-model tests
  ignored, every integration target, strict Clippy, formatting, and diff
  checks. memd-bench passed 96 tests, Ruff 0.12.9, formatting, protocol JSON,
  and diff checks. Final independent review found no blocker or high issue;
  all confirmed medium findings were fixed.
- **What didn't:** v3 paired-context invariance failed `conv-50:96` because the
  old committed binary had no fixed clock. QA was correctly withheld. Initial
  pytest invocations were invalid for this repository; the owned suite uses
  the locked project interpreter with `unittest discover`.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/ops/` | Modified | Public parameter, propagation, read-only replay, tests |
| `crates/memd/src/store/` | Modified | Fixed-time reranking, feedback/outcome clocks, debug parity |
| `crates/memd/src/retrieval/reranker.rs` | Modified | Explicit reranker clock and regression coverage |
| `crates/memd/tests/outcome_attribution.rs` | Modified | Future-event exclusion coverage |
| `CHANGELOG.md` | Modified | Fixed-clock and debug-parity contracts |
| `../memd-bench/benchmarks/locomo/` | Modified | Clocked paired requests, replay CLI, protocol, arm order |
| `../memd-bench/benchmarks/tests/` | Modified | Fail-closed acknowledgement and request-contract tests |
| `../memd-bench/benchmarks/README.md` | Modified | Frozen-corpus replay scope |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| `ranking_time_ms` pins ranking, not lifecycle visibility | A partial as-of view would be misleading because lifecycle state and later chunk writes remain live. |
| Fixed-clock search suppresses durable attribution and usage | Replaying a benchmark must not train or mutate the adaptive ranking loop. |
| The harness requires explicit null episode acknowledgement | Older daemons ignore unknown JSON fields; the null response makes that version skew fail closed. |
| Outcome serving remains shadow-only | The clean longitudinal full-loop recall@3 gate regressed to 0.734375 versus raw memory 0.953125. |

## Knowledge Capture

### Lessons Learned

- Paired searches need one ranking clock even when all stored timestamps are
  fixed; millisecond recency drift can flip a boundary tie.
- A benchmark request field needs an observable acknowledgement when older
  subjects deserialize unknown fields permissively.

### Gotchas

- The current clean v3 source and artifacts remain under
  `../memd-bench/run-output/clean-clone-v3/`; do not overwrite them.
- memd-bench's pinned source cannot recognize `ranking_time_ms` until the memd
  diff is committed and repinned. Commit product first, then harness.
- The prior untracked handoff
  `docs/handoffs/2026-07-13_locomo-hybrid-cache-commit-gate.md` is separate
  session state; do not include it in the source commit without a new decision.

## Moving Forward

### Next Steps

1. Show Chef the exact two file lists and proposed conventional commit
   messages; wait for explicit approval.
2. Stage named paths only, inspect staged diffs, commit memd first and
   memd-bench second, then push the two feature branches.
3. Repin both clean source identities, build the release binary, and rerun
   paired-context invariance under a new immutable run ID.
4. If invariance passes 1,531/1,531, run paired LoCoMo QA/judging, then CodeIR,
   MemoryData, policy selection, and LongMemEval.
5. Regenerate the immutable bundle and manuscript, obtain scientific review,
   choose the version, and request separate merge/release approval.

### Blockers

- New commit and push approval is required because the previous scoped
  approval covered only commits `e59067e` and `99c37fe`.
