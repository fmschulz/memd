# Handoff: CodeIR - Expanded-write preflight gate

**Date:** 2026-07-14
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `a4fbb4e`

## Context & Status

The product now exposes every physical ID created by a logical `memory.add`
while preserving the primary `chunk_id`. The CodeIR harness uses valid tenant
`codeir_preflight`, requires the response IDs to match the real throwaway
store's scoped SQLite inventory, and provides a `--preflight-only` gate before
the 20,000-document seed. Rust, documentation, and Python gates pass. The final
independent review found no blocker or medium issue. The product and harness
commits are published, the clean preflight passed, and CodeIR v5 is running.

The companion benchmark branch is `feat/reproducible-benchmark-evidence` at
`99fdbae`.

Independent work completed while the retry remained paused: the clean
longitudinal phase and LoCoMo v4 evidence each form a verified local bundle;
MemoryData preparation reproduces the frozen 600-question selection; and the
full cargo package verification passes. The public release baseline is v1.4.0.
Chef approved the tenant correction and deferred LoCoMo latency until the final
clean v1.5.0 subject exists.

## Technical Implementation

### Work Completed

- Added ordered `stored_chunk_ids` to single-add responses and CLI output
  (`crates/memd/src/ops/add.rs`, `crates/memd/src/cli/write_commands.rs`).
- Applied lifecycle overlays and supersession compensation to every split
  child (`crates/memd/src/store/persistent/lifecycle.rs`).
- Added complete-ID tests for persistent, in-memory, CLI, dedup, batch-dedup,
  and explicit supersession paths (`crates/memd/src/ops/tests.rs`,
  `crates/memd/src/cli/tests.rs`, `crates/memd/tests/fuzzy_dedup.rs`).
- Added CodeIR preflight and scoped inventory checks
  (`../memd-bench/benchmarks/codeir/seed_corpus.py`).
- Added fail-closed harness tests
  (`../memd-bench/benchmarks/tests/test_codeir_seed_identity.py`).
- Added a repository-local `--preflight-only` command. The old pinned subject
  reaches the corrected scope and fails exactly because it lacks
  `stored_chunk_ids`; the committed subject passed this gate before the fresh
  CodeIR seed.
- Published memd `a4fbb4e6c22eeceac52c68a21b18a00a2aded426`
  and memd-bench `99fdbaede7d89ec996aa65c3701b3e3b08d3999b`; the
  remote feature-branch heads match.
- Built the committed product inside a clean harness clone in 3m27s, passed all
  107 tests, and passed `--preflight-only` in 2.95 seconds.
- Seeded clean CodeIR v5: all 20,000 documents map to all 21,806 physical
  chunks, with exact SQLite equality, in 2,751.8 seconds. The lane sweep is
  active in unified session `31242`.
- Verified longitudinal bundle `746081ca77a3d23d` and LoCoMo partial bundle
  `7a97f96bf4f93280`; neither is the final all-workstream release bundle.
- Prepared MemoryData's pinned 600-question selection in fresh repo-local
  `run-output/memorydata-preflight-v2`.
- Verified the package without bypass flags: 204 files packaged and compiled.
- Pinned `memory.add_batch` arity and order with two inputs that each split
  into multiple physical rows; attribution-sensitive writers are directed to
  individual `memory.add` calls.

### Outcomes

- **Passed:** `cargo fmt --check`, workspace check, strict Clippy, all workspace
  tests (988 library tests; five intentional ignores; all other targets),
  strict MkDocs, Ruff lint/format, 107 Python tests, JSON parse, documentation,
  and diff checks.
- **Rejected diagnostic:** two CodeIR identity preflight attempts failed with
  the hyphenated tenant, but they invoked a sibling-repository binary. This
  violates `memd-bench/CLAUDE.md` and is not benchmark evidence. The tenant
  grammar is established by product source inspection.
- **Deferred measurement:** Chef approved running LoCoMo latency once from the
  final clean v1.5.0 subject. The earlier failed starts created no artifact.
- **Passed:** the product and benchmark changes passed their local gates and
  independent review. The change-gate marker only binds committed `HEAD`, so
  each repository must be marked again after its new commit and before push.

### File Map

| Area | Change | Notes |
|------|--------|-------|
| `crates/memd/src/store/` | Modified | Complete split-write identity and child lifecycle handling |
| `crates/memd/src/ops/` | Modified | MCP response contract and supersession output |
| `crates/memd/src/cli/` | Modified | CLI `stored_chunk_ids` output and test |
| `docs/` and `CHANGELOG.md` | Modified | Additive API contract |
| `../memd-bench/benchmarks/codeir/` | Modified | Preflight and exact inventory verification |
| `../memd-bench/benchmarks/tests/` | Modified/Added | Identity-contract tests |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Keep `chunk_id` primary and add `stored_chunk_ids` | Existing consumers retain their contract; agents and benchmarks can attribute split children. |
| Keep `memory.add_batch.chunk_ids` primary-per-input | Changing its arity would break positional compatibility. |
| Fail CodeIR before long seeding | An old or incomplete write-identity contract cannot produce valid retrieval evidence. |
| Compare the preflight response with SQLite rows | A response-shape check can miss omitted children and waste the full seed. |
| Defer LoCoMo latency to the final v1.5.0 subject | Rebuilding an already superseded subject would not support the release claim. |
| Recommend v1.5.0 after final evidence | v1.4.0 is already published; the branch adds backward-compatible features and fixes. |

## Knowledge Capture

### Lessons Learned

- Unit tests that mock add responses did not exercise tenant validation. Run a
  subject binary stored inside `memd-bench` before accepting the harness gate.
- A preflight must compare API output with stored rows, not only validate the
  response shape.
- External review claims remain hypotheses until real behavior confirms them.

### Gotchas

- `TenantId` allows ASCII alphanumerics and underscores; project IDs also allow
  hyphens and dots.
- The untouched failed `codeir-v4` directory contains 1,806 unmapped split
  children and must remain immutable.
- `memory.add_batch` still does not expose per-input split-child IDs. CodeIR
  uses single `memory.add`; document this limitation before release.

## Moving Forward

### Next Steps

1. Monitor unified session `31242` until the three CodeIR lanes finish, then
   validate the phase artifacts and report only regenerated metrics.
2. Run the pinned CodeIR baselines and judged comparison from the validated
   v5 retrieval phase.
3. Run final LoCoMo
   latency, MemoryData, policy selection, LongMemEval, and longitudinal. The
   answer/judge endpoint variables must be restored before scored runs.
4. Freeze one final bundle, regenerate/review the manuscript, then request
   merge and v1.5.0 release approval.

### Blockers

- Current answer and judge endpoint variables are absent.
- The CodeIR lane sweep is compute-bound and still running; no final retrieval
  metric exists yet.
- Commit/push, merge, artifact publication, and release each require their
  applicable approval gates.
