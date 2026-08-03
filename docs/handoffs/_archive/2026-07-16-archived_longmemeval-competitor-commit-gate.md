# Handoff: LongMemEval - Competitor Commit Gate
**Date:** 2026-07-16
**Branch:** feat/reliable-adaptive-memory
**HEAD:** eaa9592

## Context & Status

The four-file product retrieval correction and the nine-file memd-bench
LongMemEval competitor harness are ready for explicit commit approval. Neither
repository has staged files, a new commit, or a new push. The LongMemEval
dataset remains unopened, and no claim-bearing competitor result exists.

## Technical Implementation

### Work Completed

- Recovered the current benchmark state from repository-local records without
  using external memd state inside memd-bench.
- Confirmed that the corrected CodeIR judge is already committed as memd-bench
  `fe82a1d`; only the LongMemEval competitor layer is dirty.
- Verified the local Qwen3-8B service and the frozen model, runtime, and
  container identities.
- Reran the complete harness gate: 143 tests, Ruff, JSON parsing, command-entry
  checks, whitespace checks, and a secret-pattern scan passed.
- Reran Hindsight 0.8.4 against the final adapter in
  `run-output/provider-smoke-hindsight-v6`; it returned verified
  `synthetic-s1` lineage with zero failures and stable retrieval. The container
  was stopped.
- Corrected the working record: two post-fix competitor review streams ended
  without a verdict and are not approval evidence.

### Outcomes

- **What worked:** Mem0 1.0.11 and Hindsight 0.8.4 both pass real synthetic
  extraction and retrieval with the pinned local Qwen service. The harness
  rejects unfrozen identities and suppresses evidence metrics when source
  lineage is incomplete.
- **What didn't:** The post-fix independent review did not produce a final
  verdict after two attempts. Per the repeated-obstacle rule, no third review
  was launched. The memd warm worker held the writer lock and then failed to
  return two bounded write/read requests, so the progress record could not be
  verified; this file is the durable fallback.

### File Map

| Repository | File | Change | Notes |
|---|---|---|---|
| memd | `crates/memd/src/store/mod.rs` | Modified | Candidate normalization, scoring, ordering, and tests |
| memd | `crates/memd/src/ops/mod.rs` | Modified | Bounded O(k) exact rescue |
| memd | `crates/memd/src/ops/tests.rs` | Modified | Production rescue regressions |
| memd | `evals/harness/src/suites/longitudinal.rs` | Modified | Source-supported oracle v2 |
| memd-bench | `benchmarks/README.md` | Modified | LongMemEval entry point |
| memd-bench | `benchmarks/longmemeval/README.md` | Modified | Confirmatory and competitor procedure |
| memd-bench | `benchmarks/longmemeval/runner.py` | Modified | Embargo, external contexts, lineage-aware QA |
| memd-bench | `benchmarks/tests/test_longmemeval_runner.py` | Modified | Runner and manifest regressions |
| memd-bench | `benchmarks/longmemeval/competitors.v1.json` | Added | Frozen competitor protocol |
| memd-bench | `benchmarks/longmemeval/mem0_adapter.py` | Added | Pinned Mem0 provider lane |
| memd-bench | `benchmarks/longmemeval/hindsight_adapter.py` | Added | Pinned Hindsight provider lane |
| memd-bench | `benchmarks/tests/test_longmemeval_mem0_adapter.py` | Added | Mem0 adapter tests |
| memd-bench | `benchmarks/tests/test_longmemeval_hindsight_adapter.py` | Added | Hindsight adapter tests |

## Key Decisions

| Decision | Rationale |
|---|---|
| Use Mem0 and Hindsight for LongMemEval | Both are free, self-hosted, provider-native memory systems that pass the pinned Qwen gate. |
| Keep SuperLocalMemory in LoCoMo and MemoryData | Those existing lanes already provide a third free comparison without adding another confirmatory adapter before the dataset opens. |
| Keep LongMemEval closed | The confirmatory dataset may open only after one policy is frozen from same-build LoCoMo, CodeIR, and longitudinal evidence. |
| Treat missing post-fix verdicts as residual risk | A successful first review does not imply approval of its later fixes. |

## Knowledge Capture

### Lessons Learned

- Repository records can lag the worktree. CodeIR was already committed even
  though the prior cross-session summary described it as pending.
- Provider smoke evidence must postdate the source it validates. The final
  Hindsight adapter needed a fresh run because the earlier smoke was older.

### Gotchas

- Never invoke external memd state from memd-bench or inspect paths outside
  that repository while operating inside it.
- The current product and benchmark trees have no committed joint identity.
  Diagnostics from either dirty tree cannot support manuscript claims.
- Do not stage the unrelated untracked handoffs in memd.

## Moving Forward

### Next Steps

1. Obtain Chef's exact approval for the four-file memd commit/push and the
   nine-file memd-bench commit/push, including both recorded review residuals.
2. Stage only the named paths, inspect both staged diffs, commit, run the
   post-commit change gate in each repository, and push both feature branches.
3. Build the committed memd subject in a clean repo-local benchmark clone;
   produce same-build longitudinal and LoCoMo manifests, freeze the policy,
   then fetch and run all 500 LongMemEval questions for memd, Mem0, and
   Hindsight with shared Qwen answers and Gemma judgments.

### Blockers

- Both commits and pushes require exact approval.
- Both post-fix review streams lack final verdicts; Chef must accept the
  residual before commit or name a different review disposition.
- Shared-memory persistence is unavailable through the running warm worker.
  Do not restart it while another process may depend on it; retry after the
  worker is healthy or Chef authorizes a restart.
