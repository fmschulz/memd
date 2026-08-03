# Handoff: Longitudinal - Retrieval Gate
**Date:** 2026-07-15
**Branch:** feat/reliable-adaptive-memory
**HEAD:** eaa9592

## Context & Status

The same-build longitudinal run failed recall nonregression because consolidated
memories disappeared for one paraphrase. The failure exposed product retrieval
bugs rather than a bad recall metric. The current uncommitted patch fixes those
bugs and passes the unchanged seven-gate protocol in a source-honest diagnostic
run. It is not release evidence because the source is uncommitted and the final
post-fix independent review did not complete.

## Technical Implementation

### Work Completed

- Normalize query boundaries, avoid alphanumeric substring collisions, retain
  dotted and Unicode identifier matching, and reject punctuation-only queries
  in `score_candidate_chunk` (`crates/memd/src/store/mod.rs`).
- Scan the existing exact-rescue window while retaining only the ordered top k;
  delete the test-only rescue twin (`crates/memd/src/ops/mod.rs`).
- Cover punctuation, `ion`/`isolation`, dotted identifiers, punctuation-only
  queries, and production exact-rescue ordering (`crates/memd/src/ops/tests.rs`,
  `crates/memd/src/store/mod.rs`).
- Keep the deterministic consolidation oracle source-supported and bump its
  identity to `fixture-oracle-v2`/prompt v2
  (`evals/harness/src/suites/longitudinal.rs`).

### Outcomes

- **What worked:** `cargo check -p memd --tests`, six focused product tests,
  and all 65 evaluator tests passed. Source-honest diagnostic run
  `1784187418169-7790f2dc275b` passed all seven unchanged gates; full-loop
  success, recall@3, and MRR were 1.0, with zero harmful memories and zero
  scope/crash violations.
- **What didn't:** The first independent review correctly rejected the initial
  patch for probe leakage, dotted-token loss, O(scan-window) candidate memory,
  and test-twin coverage. Those findings were fixed. Two post-fix Claude
  follow-ups timed out after 600 and 300 seconds without a verdict.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/store/mod.rs` | Modified | Shared candidate scorer and ordering tests |
| `crates/memd/src/ops/mod.rs` | Modified | Bounded production exact rescue |
| `crates/memd/src/ops/tests.rs` | Modified | Production rescue regressions |
| `evals/harness/src/suites/longitudinal.rs` | Modified | Source-honest oracle v2 and 32-cluster test |
| `tasks/todo.md` | Modified, ignored | Current gate and blocker |
| `tasks/METHODS.md` | Modified, ignored | Commands, metrics, and review history |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Keep the recall/MRR thresholds unchanged | The failure came from retrieval and oracle fidelity defects, not an invalid threshold. |
| Rank the entire bounded scan while retaining only top k | This removes insertion-order bias without O(scan-window) candidate memory. |
| Keep the diagnostic out of manuscript/release evidence | Its source tree is dirty and has no committed identity. |

## Knowledge Capture

### Lessons Learned

- Exact-token rescue must rank before truncation; otherwise listing order can
  suppress the best candidate.
- Benchmark oracle text must derive from source memories, not evaluation probes.
- A punctuation fix needs separate behavior for plain words and dotted or
  identifier-like terms.

### Gotchas

- The first green diagnostic used probe-flavored oracle wording and must not be
  cited. Only v3 is source-honest, and v3 is still diagnostic.
- The complete first review is in
  `tasks/claude-review-retrieval-rescue-longitudinal.stream.jsonl`. The two
  follow-up artifacts contain partial inspection only.
- Pre-existing untracked handoffs are unrelated and must remain untouched.

## Moving Forward

### Next Steps

1. Chef chooses whether to accept the complete first review plus verified fixes,
   use another peer CLI for a post-fix review, or wait and retry Claude later.
2. Run the project change gate after the review decision.
3. Show the exact four-file commit scope and conventional message for approval;
   do not commit or push before approval.
4. Rebuild a clean subject and rerun CodeIR, LoCoMo, longitudinal, then the
   still-embargoed LongMemEval memd/Hindsight/Mem0 comparison with pinned Qwen.

### Blockers

- Final post-fix independent review has no verdict after two bounded timeouts.
- No claim-bearing benchmark can proceed until a clean committed subject exists.
