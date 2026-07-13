# Handoff: Reliable Adaptive Memory - Provenance Commit Gate

**Date:** 2026-07-13
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006bc0100e1c020a404891d214ac5e0188a46`

## Context & Status

The product implementation, local quality gates, structural cleanup, frozen
longitudinal evaluation, benchmark infrastructure, and independent code
reviews are complete in the working trees. The remaining goal path begins with
two clean source commits. No commit, push, merge, benchmark publication,
version bump, tag, or release has been performed.

The memd worktree has 230 status entries. The sibling memd-bench worktree is on
`feat/reproducible-benchmark-evidence` at
`bef5e98b75aef147696b120c33b0785b9809ae7b` with 97 status entries. The exact
proposed paths and messages are in `tasks/proposed-commit-scope.md`; both lists
match `git status --short --untracked-files=all` exactly.

## Technical Implementation

### Work Completed

- Registered stable workstream IDs in the goal and required them in benchmark
  manifests and parent lineage (`../memd-bench/benchmarks/provenance.py`).
- Made the phase-manifest runtime and JSON Schema registries fail closed on
  unknown IDs and answer-model digest drift
  (`../memd-bench/benchmarks/schemas/phase-manifest.v1.schema.json`).
- Made evidence bundles include subject binaries, recursively collect parent
  dependencies, bind source manifests into the logical digest, and replay
  manifest validation against bundled files
  (`../memd-bench/benchmarks/bundle_artifacts.py`).
- Closed policy-manifest discovery, policy-sidecar lineage, and LoCoMo external
  manifest binding gaps (`../memd-bench/benchmarks/validate_artifacts.py`,
  `../memd-bench/benchmarks/select_policy.py`,
  `../memd-bench/benchmarks/locomo/run_qa.py`).
- Added regression coverage for workstream registry parity, parent input
  closure, recursive bundle closure, semantic bundle revalidation, and all
  registered phases (`../memd-bench/benchmarks/tests/`).
- Completed independent initial and post-fix Cursor reviews. The follow-up
  found no claim-publication blocker in the reviewed provenance scope
  (`tasks/cursor-review-benchmark-provenance-followup.txt`).

### Outcomes

- **What worked:** 46 benchmark tests, compileall, Ruff, Draft 2020-12 schema
  validation, diff whitespace checks, and the nine-claim manuscript audit pass.
  The current phase-manifest tree contains no claim-bearing run artifact, so
  completing v1 did not invalidate prior evidence. memd progress record
  `019f5c0c-90a5-7183-af04-ab8e3712d41c` points to this handoff; durable
  decision `019f5c0c-b59e-7253-a4c4-2a3244cd21f9` records the bundle contract.
- **What didn't:** the first Cursor plan-mode call exited zero with no output
  and is not counted. Ask mode returned a real review. Its publication blockers
  were verified locally, fixed, and approved on follow-up. No answer or judge
  configuration is available in the environment.

### File Map

| File | Change | Notes |
|---|---|---|
| `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` | Modified | Stable workstream registry and completed independent-code-review gate |
| `tasks/proposed-commit-scope.md` | Modified | Exact 230-file and 97-file commit scopes |
| `../memd-bench/benchmarks/provenance.py` | Modified | Workstream and answer-model integrity contract |
| `../memd-bench/benchmarks/bundle_artifacts.py` | Modified | Recursive, self-validating bundle closure |
| `../memd-bench/benchmarks/validate_artifacts.py` | Modified | All phase types discovered |
| `../memd-bench/benchmarks/tests/test_validate_artifacts.py` | Added | Phase-discovery regression |

## Key Decisions

| Decision | Rationale |
|---|---|
| Finalize workstream identity in manifest v1 now | No phase manifest or claim-bearing bundle exists, so the uncommitted contract can be completed without invalidating evidence |
| Bundle datasets and subject binaries with a 512 MiB per-file ceiling | A single self-contained artifact can carry the 277,383,467-byte LongMemEval dataset and the current 31,622,784-byte binary while excluding stores and caches |
| Keep `outcome-v1` shadow-only | The frozen full loop failed recall non-regression: 0.734375 versus raw-memory 0.953125 |
| Do not fabricate `MEMD_SOURCE_COMMIT` from a dirty tree | Public manifests must identify the clean commit that built the evaluated binary |

## Knowledge Capture

### Lessons Learned

- Inventory hashes alone do not make a benchmark bundle self-validating. The
  verifier must replay each manifest against the bundled file tree and require
  the complete parent closure.
- A successful peer-review process exit with an empty artifact is not a review.

### Gotchas

- The installed 1.4.0 warm worker owns the writer lock. Route local memd writes
  through `--warm auto`; `--warm off` fails while that worker is active.
- No answer/judge config or endpoint environment is present. Empirical QA and
  LongMemEval judging cannot start after the commits until those are supplied.
- Crates.io already contains 1.4.0. Version choice and release authorization
  remain separate from commit authorization.

## Moving Forward

### Next Steps

1. Obtain Chef's approval for the two exact commits in
   `tasks/proposed-commit-scope.md`, then stage only those named paths, inspect
   each staged diff, and commit each repository separately.
2. Configure a pinned open-weight answer and judge endpoint without recording
   credentials; build the clean memd commit through `benchmarks/reproduce.sh`.
3. Run CodeIR, LoCoMo, MemoryData, policy selection, and untouched LongMemEval;
   build and verify one immutable bundle from the complete phase closure.
4. Generate final tables and figures from that bundle, bind every pending
   manuscript claim, rerun scientific review, choose a new version, and request
   separate push, merge, publication, and release approvals.

### Blockers

- Commit authorization for the two exact local commits.
- A pinned answer/judge endpoint and config after the commits exist.
- Separate later approval for artifact publication and release actions.
