# Handoff: LoCoMo - Hybrid Cache Commit Gate

**Date:** 2026-07-13
**Updated:** 2026-07-14
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `67326c5`

## Context & Status

Fresh LoCoMo v2 seeding and retrieval ablations completed from published
source identities. The required paired event-time gate failed 1,320 of 1,531
questions, so downstream QA stopped. The root cause is a memd
tiered-hybrid cache bug, compounded by a benchmark harness data-directory bug.
Chef approved both fixes. They were committed and pushed as memd `e59067e` and
memd-bench `99c37fe`; both remote feature-branch heads match.

The companion memd-bench checkout is on
`feat/reproducible-benchmark-evidence` at `c06b5e6`.

## Technical Implementation

### Work Completed

- Published LoCoMo source identity fix `c06b5e6` and seeded 10 conversations /
  5,882 turns into the new v2 directory.
- Evaluated hybrid, dense-only, and BM25-only retrieval over 1,531 eligible
  questions; exact metrics are recorded in `tasks/METHODS.md`.
- Fixed tiered semantic-cache hits so they re-run and fuse the sparse leg
  (`crates/memd/src/store/hybrid.rs`).
- Added a cold/warm ranking-parity regression that proves sparse fusion and an
  actual cache hit (`crates/memd/src/store/hybrid.rs`).
- Fixed the harness to use `$HOME/.config/memd/config.toml` and pass
  `--data-dir` before `batch` (`../memd-bench/benchmarks/locomo/common.py`).
- Added TOML-location and command-order regression tests
  (`../memd-bench/benchmarks/tests/test_locomo_common.py`).
- Preflighted the v3 execution gate without creating claim artifacts: the new
  output names are unused, both model configurations validate, the pinned
  local endpoints pass live answer and judgment smokes, and the locked Qwen
  tokenizer loads.

### Outcomes

- **What worked:** The full memd workspace/all-target test gate, strict clippy,
  release build, formatting, 93 benchmark tests, Ruff, diff checks, explicit
  store diagnostic, final independent Cursor review, and v3 model/runtime
  preflight all passed.
- **What didn't:** LoCoMo v2's paired-context gate exposed cold/warm ranking
  drift; QA did not run. Earlier Claude review lanes were operationally
  incomplete and are not counted as final review evidence.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/store/hybrid.rs` | Modified | Always fuse sparse results on tiered cache hits; add regression |
| `../memd-bench/benchmarks/locomo/common.py` | Modified | Use the actual config path and pin the requested data directory |
| `../memd-bench/benchmarks/tests/test_locomo_common.py` | Added | Enforce config and CLI isolation contracts |
| `tasks/todo.md` | Working note | Updated v2 results and v3 gate |
| `tasks/METHODS.md` | Working note | Recorded commands, metrics, diagnosis, and verification |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Treat semantic cache entries as dense-tier results | The final hybrid result depends on current sparse retrieval and must be fused on both cold and warm paths |
| Do not use v2 for QA | Its retrieval parent failed the mandatory paired-context invariance gate |
| Seed a new v3 after clean commits | Claim-bearing evidence must bind committed source and binary identities and must not relabel a failed run |
| Keep failed and diagnostic directories untouched | They preserve the failure evidence and are not valid claim artifacts |

## Knowledge Capture

### Lessons Learned

- Cache-hit equivalence needs an explicit repeated-query contract; a cache of
  one retrieval leg cannot bypass downstream fusion.
- Setting `XDG_CONFIG_HOME` does not isolate a program that resolves config
  directly from `$HOME/.config`; critical paths should also be explicit CLI
  arguments.

### Gotchas

- `cargo fmt` mechanically reindented the existing fusion branch, so the memd
  diff is 179 lines although the behavior change is small and confined to one
  file.
- `git diff --stat` in memd-bench omits the untracked new test file; name it
  when staging.
- `run-output/locomo-clean-v1`, `run-output/locomo-clean-v2`, and
  `run-output/diagnostic-hybrid-cache-20260713-v1` must not be deleted,
  overwritten, or promoted as new evidence.

## Moving Forward

### Next Steps

1. Build a clean pinned memd binary from the new commit, seed a new LoCoMo v3
   directory from a fresh harness clone, rerun retrieval and paired-context
   invariance, and run QA only if invariance passes.
2. Continue the frozen CodeIR, MemoryData, LongMemEval, artifact bundle, and
   manuscript gates.

### Blockers

- None for the clean v3 build and benchmark reruns. Merge, release, and artifact
  publication still require separate approval.
