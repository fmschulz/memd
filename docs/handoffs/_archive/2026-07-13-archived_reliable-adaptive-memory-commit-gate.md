# Handoff: Reliable adaptive memory - commit gate
**Date:** 2026-07-13
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006b`

## Context & Status

Milestones 1–6 and 10–11 of the active reliable-adaptive-memory goal are
implemented, reviewed, and green locally. The sibling `memd-bench` repository
has the reproducibility and manuscript-assertion infrastructure for milestones
7–9, but claim-bearing runs remain pending. Neither repository has been
committed because Chef has not authorized a commit. `outcome-v1` remains
shadow-only.

## Technical Implementation

### Work Completed

- Fixed the independent review's cross-tenant alias leak by scoping outcome
  priors to the retrieval episode requester tenant/project
  (`crates/memd/src/store/metadata/sqlite/episodes.rs`,
  `crates/memd/src/store/memory.rs`, `crates/memd/src/ops/mod.rs`).
- Made `off`, `shadow`, and rejected `serve` semantics explicit and covered;
  combined multi-query agent-context episodes now use `off` and no synthetic
  shadow ranks (`crates/memd/src/cli/search.rs`,
  `crates/memd/tests/outcome_attribution.rs`).
- Added NULL-project/project isolation, wrong-tenant access, alias isolation,
  agent-context SQLite privacy, corrected/abandoned eligibility, and linkage
  length tests.
- Clarified the legacy-feedback baseline, plaintext linkage fields, optional
  raw-query audit logs, and duplicate longitudinal v1 measures
  (`docs/self-improvement.md`, `docs/cli-reference.md`,
  `evals/bench/longitudinal/PROTOCOL.md`).
- Captured the two independent reviews in
  `tasks/cursor-review-milestones-4-6.md` and
  `tasks/cursor-review-outcome-scope-fix.txt`.

### Outcomes

- **What worked:** `cargo test --workspace` passed with 976 memd library tests,
  five intentional model-download ignores, every integration/binary target,
  64 evaluation-library tests, five evaluation-binary tests, and all doctests.
  Strict workspace Clippy and strict MkDocs passed. The follow-up Cursor review
  reported no blockers or high-severity findings.
- **What didn't:** the first attempted MkDocs command lacked its ephemeral
  dependency; the established `uv run --with mkdocs-material ...` command
  passed. The frozen full-loop longitudinal treatment still fails recall
  non-regression, so live outcome serving is not justified.

### File Map

| File | Change | Notes |
|---|---|---|
| `crates/memd/src/store/metadata/sqlite/episodes.rs` | Modified | requester-scoped SQL priors and tenant-joined outcome listing |
| `crates/memd/src/store/memory.rs` | Modified | in-memory requester-scope parity |
| `crates/memd/src/ops/mod.rs` | Modified | request-scope ranking and explicit off path |
| `crates/memd/src/cli/search.rs` | Modified | attribution-only agent-context episodes |
| `crates/memd/tests/outcome_attribution.rs` | Modified | isolation and policy-mode regressions |
| `docs/self-improvement.md` | Modified | exact adaptive-learning/privacy contract |
| `evals/bench/longitudinal/PROTOCOL.md` | Modified | frozen duplicate-metric disclosure |
| `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` | Modified | milestone state |

## Key Decisions

| Decision | Rationale |
|---|---|
| Scope priors to the requester episode, not chunk origin. | An alias must not let one tenant train another tenant's ranking. |
| Keep `outcome-v1` shadow-only. | Frozen protocol v1 failed full-loop recall non-regression (`0.734375` vs raw `0.953125`). |
| Treat agent-context combined episodes as attribution-only `off` records. | The multi-query merge does not retain expanded pools or compute one combined counterfactual order. |
| Preserve protocol v1's duplicate success/verifier fields with disclosure. | Changing a frozen schema after observing results would invalidate the run. |
| Do not start public benchmark runs from the dirty tree. | Claim-bearing manifests require an approved committed source identity. |

## Knowledge Capture

### Lessons Learned

- Attribution validation alone does not prevent learning leakage; aggregation
  scope must also follow the requester boundary.
- A response can be privacy-safe in SQLite while an explicitly requested audit
  log contains raw query summaries. Both persistence surfaces must be stated.

### Gotchas

- Twelve BEIR moves are already staged; the remainder of both repositories is
  unstaged. Do not commit until Chef approves the exact full scopes.
- With untracked directories expanded, the memd tree has 229 file entries and
  `memd-bench` has 96. Treat the current tree as one goal-owned change set; do
  not use `git add -A`.
- A merge to memd `main` after a version bump triggers tagging and publication.
  Commit approval is not push, merge, version, or release approval.

## Moving Forward

### Next Steps

1. Obtain Chef's approval for the exact two proposed commits, stage named paths,
   inspect each staged diff, and commit without pushing.
2. Use the resulting memd commit as `MEMD_SOURCE_COMMIT`; run the memd-bench
   clean-clone fetch/verify/reproduce path.
3. Configure pinned open answer/judge endpoints and produce immutable CodeIR,
   LoCoMo, MemoryData, and LongMemEval bundles.
4. Bind the nine pending manuscript claim groups, run assertion verification,
   and obtain independent scientific review.
5. Ask separately for version, push/merge, and release approvals.

### Blockers

- Chef approval is required before committing either repository.
- Claim-bearing QA/judge runs require configured pinned answer and judge
  endpoints; none are currently configured.
- Release version remains unchosen; `1.4.0` already exists on crates.io.
