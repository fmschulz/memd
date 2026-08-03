# Handoff: CodeIR v6 committed execution

**Date:** 2026-07-15
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `eaa9592`

## Context & Status

CodeIR v5 is rejected as benchmark evidence. Its seed reconciled all 20,000
documents and 21,806 physical chunks, and the hybrid and dense lanes finished,
but source inspection showed that `bm25-only` still applied feature reranking.
The run was stopped at 702 of 3,000 BM25 queries before it could write a
retrieval manifest. Preserve the partial directory as diagnostic state only.

The reviewed corrections are committed and published as memd `eaa9592` and
memd-bench `9339d9e`; both remote heads and post-commit change-gate markers
match. A clean-clone CodeIR v6 run from those identities completed seed,
retrieval, and external baselines with three validated manifests. Judged-answer
execution is paused until the direct-artifact judge correction receives a
committed harness identity.

## Technical Implementation

### Work Completed

- Made `bm25-only` sparse-only and disabled metadata/query feature reranking
  (`crates/memd/src/main.rs`, `crates/memd/src/store/hybrid.rs`).
- Preserved BM25 order through persistent retrieval while retaining the
  documented stored-feedback adjustment (`crates/memd/src/store/persistent.rs`,
  `crates/memd/src/store/persistent/retrieval.rs`).
- Added fail-closed sparse-only index opening, including the explicit missing
  read-only-store exception and regression tests
  (`crates/memd/src/store/persistent/tests.rs`).
- Replaced no-op sparse compaction with an explicit waited Tantivy merge and
  wired it into background and CLI maintenance
  (`crates/memd/src/index/bm25.rs`,
  `crates/memd/src/compaction/segment_merge.rs`,
  `crates/memd/src/cli/maintenance.rs`).
- Updated CLI help, configuration, data-layout, and changelog text
  (`crates/memd/src/cli/args.rs`, `docs/`, `CHANGELOG.md`).
- Made CodeIR seed dry-run, apply, and verify sparse compaction with the exact
  evaluated repo-local binary; the harness rejects missing/zero/inconsistent
  reports and unexpected orphan cleanup
  (`../memd-bench/benchmarks/codeir/seed_corpus.py`).
- Added harness unit coverage and updated the benchmark run guide
  (`../memd-bench/benchmarks/tests/test_codeir_seed_identity.py`,
  `../memd-bench/benchmarks/README.md`).
- Replaced judge-time retrieval with fail-closed reuse of the exact immutable
  CodeIR retrieval rows and bound the judge manifest to the validated parent
  subject, environment, and result artifact.
- Added the locked Transformers version to external-baseline runtime identity;
  the pinned Jina failure remains explicit rather than monkeypatched.
- Added one scope resolver for typed and structured CLI operations. Missing
  structured tenant/project fields inherit `.memd/project_scope.json` on the
  client. Explicit tenants remain tenant-wide unless a project is also given.
- Pre-scoped warm batches never consult the worker's working directory. Cold
  and warm `--continue-on-error` batches preserve successful receipts around a
  malformed-scope line, while unreadable or malformed scope files fail closed.
- Added direct, batch, and cross-working-directory warm tests, including a
  worker in project A, a call from project B, and an unscoped batch.

### Outcomes

- **Product gate:** 1,000 library tests passed with five intentional ignores;
  every integration, binary, eval, and doc-test target passed. Strict Clippy,
  formatting, and strict MkDocs passed.
- **Package gate:** the locked package and publish dry-run each packaged 204
  files (4.1 MiB, 837.8 KiB compressed) and compiled the packaged crate. The
  optimized build passed in 2m09s.
- **Behavior gate:** a release-binary structured write inherited
  `fschulz/memd`; the sparse store had three segments; aggressive maintenance
  merged them to one; the next dry-run reported zero merges; and BM25-only
  returned the rare-term document.
- **Harness gate:** 109 tests passed; Ruff formatting and lint passed for all
  50 Python files.
- **Judge-correction gate:** 15 focused and 124 total harness tests pass; Ruff
  formatting/lint, the diff check, and real preprocessing of 600 sampled jobs
  across all four v6 lanes pass. Two independent reviews found no blocker,
  high, or medium issue; every actionable low finding was fixed.
- **Independent review:** the first review's two medium findings were fixed.
  The follow-up found no blocker, high, or medium issue; its two low notes were
  also closed.
- **Structured-scope review:** the first review's high and two medium findings
  were fixed. Three follow-ups found no blocker, high, or medium issue. Their
  warm/cold receipt divergence, unreadable-scope fallthrough, double-wrapped
  error, and spoofable reserved-field notes were fixed and tested.
- **Performance diagnostic:** candidate reduction and a 42-to-1 force merge
  did not materially reduce the frozen 20-query latency. Make no speed claim.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/main.rs` | Modified | Pure BM25 variant configuration |
| `crates/memd/src/store/` | Modified | Sparse-only construction and rank-preserving retrieval |
| `crates/memd/src/index/bm25.rs` | Modified | Waited force-merge operation |
| `crates/memd/src/compaction/segment_merge.rs` | Modified | Real segment merge |
| `crates/memd/src/cli/` | Modified | Maintenance execution, report, and help |
| `crates/memd/src/cli/{scope,batch,warm}.rs` | Modified | Client-side structured scope and warm-wire receipt handling |
| `crates/memd/tests/{cli_contract,warm_write_routing}.rs` | Modified | Cold and cross-working-directory warm scope contracts |
| `docs/` and `CHANGELOG.md` | Modified | User-facing behavior and operational cost |
| `../memd-bench/benchmarks/codeir/seed_corpus.py` | Modified | Seed-time compaction gate |
| `../memd-bench/benchmarks/tests/test_codeir_seed_identity.py` | Modified | Fail-closed report tests |
| `../memd-bench/benchmarks/codeir/run_judge.py` | Modified | Reuse validated retrieval rows without rerunning memd |
| `../memd-bench/benchmarks/codeir/run_baselines.py` | Modified | Record locked Transformers identity |
| `../memd-bench/benchmarks/tests/test_codeir_judge_subject.py` | Added | Fail-closed parent/result evidence contracts |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Reject CodeIR v5 | The lane label did not match the retrieval path, so its partial numbers cannot support a baseline claim. |
| Preserve v5 files | They document the failure without being promoted into a manifest or bundle. |
| Force-merge before lane copies | Each lane starts from the same compacted write-only seed snapshot. |
| Fail closed on a missing BM25 index | An explicit lexical baseline must not silently degrade to substring search. |
| Keep stored-feedback adjustment | `bm25-only` defines lexical candidate ranking; the existing served feedback policy remains a separate documented stage. |
| Resolve structured scope on the client | A long-lived worker can have a different working directory and must not choose the request's tenant or project. |
| Fail closed on unreadable scope | Falling through to a parent or default scope can misroute a flagless write. |
| Do not claim a latency gain | Frozen diagnostics did not show a material improvement. |
| Judge immutable retrieval rows | Downstream answer accuracy must use the same candidates as the reported retrieval metrics. |
| Preserve Jina as an explicit failure | Two independent Transformers 5 incompatibilities make incremental monkeypatching unsound. |
| Target v1.5.0 only after final evidence | v1.4.0 already exists on origin and crates.io; the pending work is not yet in a release. |

## Knowledge Capture

### Lessons Learned

- Benchmark labels need executable contract tests before expensive runs.
- Exact candidate and segment reductions are not substitutes for measured
  end-to-end latency.
- A recovery fixture invoking BM25-only must seed a real sparse index; the
  fail-closed behavior is intentional.
- The installed v1.4.0 structured CLI still needs explicit tenant/project JSON
  until this branch is released.
- Final durable scope/gate record: memd chunk
  `019f65cb-7334-7e32-81e2-b13c6ac95c51` under `fschulz/memd`.
- V6 cache/readiness record: memd chunk
  `019f65ca-707e-7dd3-b362-9e8fae853e6d` under `fschulz/memd`.

### Gotchas

- The partial v5 directory has seed artifacts and incomplete lane stores but no
  valid retrieval result or manifest.
- `perf` cannot attach on this host because `perf_event_paranoid=4`; the
  available `strace -c` result was not an interpretable query profile.
- A merge to `main` without a version bump does not publish this code. Release
  requires the later v1.5.0 bump, whose merge is the irreversible release act.
- Five handoff files are untracked in the product worktree. Do not include
  them in the product commit unless Chef explicitly expands the scope.
- The reserved warm-wire batch marker is rejected on both the cold path and at
  client pre-scoping; only the version-matched worker accepts it.
- Do not reuse the top-level ignored CodeIR cache; its legacy metadata fails
  the package-identity validator. The clean v5 clone's 20,000-document and
  3,000-query cache validates and may be copied into v6 only if validation
  passes again inside the new clean repository.
- Budget eight to nine hours for CodeIR v6. The earlier five-hour estimate was
  too short: v5 spent 46 minutes seeding, about two hours in each completed
  hybrid/dense lane, and projected about three hours for BM25.

## Moving Forward

### Next Steps

1. Finish independent review, commit and publish the three-file harness
   correction after explicit approval, and reproduce it in a fresh clean clone.
2. Run the direct-artifact judged answers, validate the result and manifest,
   and preserve both earlier judge attempts as diagnostic state only.
3. Retain the pinned Jina baseline's locked-runtime incompatibility explicitly,
   then promote the validated CodeIR closure into the final immutable bundle.
4. Complete final LoCoMo latency, MemoryData, policy
   selection, LongMemEval, and longitudinal evidence before the manuscript and
   v1.5.0 release gates.

### Blockers

- No implementation blocker. The next claim run is gated on explicit approval
  for the reviewed commit and feature-branch push.

The review is now closed. The exact three-file commit and feature-branch push
were presented to Chef and are awaiting explicit approval. Read-only readiness
checks found both pinned model services live, 1,002 GiB free, and 1,704 unique
question/context pairs for the four-lane 600-query judged sample; 696 of 2,400
lane-query evaluations are exact cache reuses.

### Active Run

- memd remote head: `eaa9592f5f92e4d7bf7c937041c2e230783a5474`.
- memd-bench remote head: `9339d9ef28cf090f4764bd2bbfec229d7c462f25`.
- Clean clone: `../memd-bench/run-output/clean-clone-v6/memd-bench`.
- Retrieval runner: complete at 12:16:00 with two valid manifests.
- External baseline: complete with a third valid manifest.
- Judge runners: the first launch failed before work; v2 was deliberately
  stopped after the provenance flaw was found; neither has an artifact.
- Log: `run-output/clean-clone-v6/memd-bench/run-output/codeir-v6.log`.
- Durable run-start record: memd chunk
  `019f65d4-56de-7b81-a88b-8c79dd392580` under `fschulz/memd`.
- Endpoint-readiness record: memd chunk
  `019f65dc-38e2-7942-b38d-e1b25876c6c8` under `fschulz/memd`.
- Validated CodeIR result and direct-judge correction record: memd chunk
  `019f6810-03f7-7961-a0e2-24c64330529d` under `fschulz/memd`.
- Pre-run gates: committed release build, 109 harness tests, dataset validation,
  expanded-write identity, and sparse merge 12-to-1 with idempotent verification.
