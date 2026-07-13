# Handoff: Reliable Adaptive Memory - Commit and Rerun Gate
**Date:** 2026-07-13
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `1b6006b`

## Context & Status

The implementation, repository cleanup, benchmark provenance, and local quality
work for the reliable adaptive-memory goal are complete in two uncommitted
branches. memd remains at `1b6006b` with 231 status entries. memd-bench remains
at `bef5e98` on `feat/reproducible-benchmark-evidence` with 107 status entries.
No commit, push, version bump, merge, tag, public artifact, or release has been
performed.

Every safe pre-commit task is closed. The next action is Chef's approval for the
two proposed commits in `tasks/proposed-commit-scope.md`. The committed memd
identity is then required before clean-clone LoCoMo, CodeIR, MemoryData,
LongMemEval, and longitudinal runs can produce claim-bearing phase manifests.
Pinned local answer and judge endpoints are configured and smoke-tested; they
do not remove the clean-source requirement.

## Technical Implementation

### Work Completed

- Strengthened bundle verification so the inventory must equal the recursive
  source-manifest closure (`../memd-bench/benchmarks/bundle_artifacts.py`).
- Registered `ram-longitudinal`, synchronized runtime and JSON Schema
  registries, and required clean memd subjects to use the empty-patch digest
  (`../memd-bench/benchmarks/provenance.py`,
  `../memd-bench/benchmarks/schemas/phase-manifest.v1.schema.json`).
- Made policy selection use stable workstream IDs and reject LongMemEval or
  recursive policy evidence (`../memd-bench/benchmarks/select_policy.py`).
- Added strict bundle-driven regeneration for the abstract, protocol, run
  identity, all-treatment table, outcome-only deltas, full-loop effects, failed
  promotion gate, and numeric Limitations paragraph
  (`../memd-bench/manuscript/render_longitudinal.py`).
- Extended the claim audit to ten regions and excluded only tagged display,
  run, runtime, hash, and version identifiers from numeric bindings
  (`../memd-bench/manuscript/check_assertions.py`,
  `../memd-bench/manuscript/claims.v1.json`).
- Forced release reproduction to start from a fresh memd checkout and documented
  the tested write/check procedure (`../memd-bench/benchmarks/reproduce.sh`,
  `../memd-bench/manuscript/README.md`).
- Added a strict MemoryData result contract that validates exactly 600 rows and
  recomputes packed-context hashes, answer metrics, evidence recall, category
  counts, and aggregate summaries from raw row data
  (`../memd-bench/benchmarks/memorydata/result_contract.py`,
  `../memd-bench/benchmarks/schemas/memorydata-result.v1.schema.json`).
- Pinned the matched SuperLocalMemory comparator to version 3.6.22 at commit
  `e02c8abc2b83e9a996571feb7de5ef9b56dcb0a5`, Mode A, no embedder, no cross
  encoder, and disabled ingest gating. The adapter records a separate retrieval
  phase and maps returned fact IDs back to in-conversation MemoryData documents
  (`../memd-bench/benchmarks/memorydata/slm_adapter.py`).
- Bound both MemoryData systems to the same 20-result, 4,096-token QA path and
  added deterministic bundle-only PNG, SVG, PDF, and JSON figure regeneration
  (`../memd-bench/benchmarks/memorydata/runner.py`,
  `../memd-bench/manuscript/render_memorydata.py`).
- Reconciled the exact two-repository commit scope
  (`tasks/proposed-commit-scope.md`).
- Replaced a nonfunctional PEP 751 invocation and a hash-ignoring requirements
  detour with a root Python 3.12 uv project. Every phase binds `pyproject.toml`
  and `uv.lock`, requires the root `.venv`, records the uv version and
  executable digest, and checks the installed packages used by evaluated model
  and CodeIR paths (`../memd-bench/pyproject.toml`,
  `../memd-bench/uv.lock`, `../memd-bench/benchmarks/provenance.py`).
- Configured ignored, secret-free Qwen3-8B answer and Gemma 4 31B judge files at
  exact model revisions and container digests. The model API returned `BLUE`
  and strict parseable `{"correct": true}`; the pinned tokenizer loaded as
  `Qwen2Tokenizer` (`../memd-bench/benchmark-data/answer-model.json`,
  `../memd-bench/benchmark-data/judge-model.json`).

### Outcomes

- **What worked:** 80 benchmark tests, Ruff lint and format, compileall,
  `bash -n`, Draft 2020-12 schema validation, two local model-config validations,
  the ten-claim audit, exact proposed-scope reconciliation, `git diff --check`,
  both endpoint health probes, and answer/judge model-API smokes pass. Fixture
  coverage runs both longitudinal and MemoryData `write` and `check` commands
  end to end. A real frozen MemoryData preflight validates 10 samples and 600
  questions.
- **What didn't:** the first full gate exposed an unused test variable and a
  missing ephemeral `jsonschema` dependency; both were corrected before the
  fail-fast rerun. Independent review found an unreferenced-inventory hole,
  incomplete renderer provenance, name-based policy families, rewrite test
  gaps, and future identifier/source-reuse blockers. All were reproduced or
  verified, fixed, and regression-tested.
- A second independent review reproduced a blocking upstream MemoryData category
  type mismatch. Boundary normalization fixed it. The same review also exposed
  unlocked documented QA commands, insufficient comparator pinning tests, an
  unchecked installed SuperLocalMemory runtime, incomplete figure binding, and
  claim-cache reuse; each was corrected and regression-tested.
- The answer-model preflight proved that the documented `pylock.toml` command
  installed no packages under uv 0.11.19. Independent review then proved that
  the temporary requirements invocation ignored hashes. Native uv project
  locking replaced both approaches. A broad review found no blocker and a
  narrow post-fix review found no blocker, high, or medium regression
  (`tasks/claude-review-runtime-lock.txt`,
  `tasks/claude-review-uv-project.txt`,
  `tasks/claude-review-uv-project-final.txt`).
- The development manuscript's neutral stylometry check is not a submission
  pass (`pass=false`, one dash-excess hard tell, style distance 2.16). Final
  prose revision remains correctly deferred until release evidence replaces
  the development values.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `tasks/proposed-commit-scope.md` | Modified | Exact 231-path and 107-path commit scopes |
| `tasks/METHODS.md` | Modified | Commands, gates, review findings, and outcomes |
| `tasks/claude-review-longitudinal-provenance.txt` | Added | Independent-review record and resolutions |
| `tasks/claude-review-memorydata.txt` | Added | MemoryData contract review and resolutions |
| `tasks/claude-review-memorydata-slm.txt` | Added | Comparator review and resolutions |
| `../memd-bench/benchmarks/bundle_artifacts.py` | Added | Exact recursive bundle closure verification |
| `../memd-bench/benchmarks/provenance.py` | Added | Stable workstreams and clean-source contracts |
| `../memd-bench/manuscript/render_longitudinal.py` | Added | Strict eight-region regeneration and drift check |
| `../memd-bench/manuscript/check_assertions.py` | Added | Fail-closed numeric claim audit |
| `../memd-bench/benchmarks/memorydata/result_contract.py` | Added | Row-recomputed MemoryData result contract |
| `../memd-bench/benchmarks/memorydata/slm_adapter.py` | Added | Pinned comparator retrieval phase |
| `../memd-bench/manuscript/render_memorydata.py` | Added | Deterministic bundle-only figure renderer |
| `../memd-bench/pyproject.toml` | Added | Python 3.12 benchmark runtime declaration |
| `../memd-bench/uv.lock` | Added | Native uv lock with 94 selected packages |
| `../memd-bench/benchmarks/tests/` | Added | 80-test benchmark/provenance/manuscript gate |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Bundle inventories must contain exactly manifest-reachable files | A self-hashed extra file is not provenance and cannot support a claim |
| Longitudinal prose regenerates as one eight-region unit | Abstract, methods, results, gate text, and limitations cannot drift independently |
| Strict renderer check precedes claim verification | Numeric bindings intentionally exclude identifiers and hashes; renderer check binds those strings to the presented bundle |
| Clean source means `source_dirty=false` plus the empty-patch digest | A syntactically valid arbitrary patch digest cannot represent a clean checkout |
| Release reproduction refuses reused source directories | Untracked or ignored build configuration must not influence a supposedly clean binary |
| Outcome ranking remains shadow-only | Frozen longitudinal v1 failed recall non-regression; no gate was weakened after observing it |
| MemoryData compares configured memd and SuperLocalMemory 3.6.22 through one QA contract | Matching retrieval depth, packed-token budget, tokenizer, prompt, answer model, and scorer isolates the retrieval systems |
| MemoryData figures accept only a complete validated evidence bundle | Result JSON or copied summary values alone cannot support a manuscript comparison |
| Use the root native uv project for every benchmark phase and renderer | `uv run --locked` enforces lock freshness; manifests bind project, lock, interpreter, and uv identities |
| Use Qwen3-8B for answers and Gemma 4 31B for semantic judgment | Separate exact open-weight model/runtime identities passed real API and parser preflights; Gemma requires a 256-token cap so hidden reasoning cannot consume the entire response allowance |

## Knowledge Capture

### Lessons Learned

- A content-addressed inventory is insufficient unless every inventoried file is
  reachable from a validated source manifest.
- Quantitative claim audits need a separate exact-render check for hashes,
  versions, run IDs, and other identifiers that are intentionally not numbers.
- Regenerating only a results table can leave abstract and limitation counts
  stale while a local table check still passes.
- Real upstream data should be validated at the adapter boundary; the frozen
  MemoryData loader emits category strings although the prepared benchmark
  contract uses integer category IDs.
- A lock file is not proof that a command materialized it. Exercise an import in
  the declared environment, assert installed versions at evaluated call sites,
  and bind the lock plus runtime tool identity into phase evidence.

### Gotchas

- `manuscript/check_assertions.py verify` must continue to fail while any of the
  ten claims is pending; there is no override.
- `manuscript/render_longitudinal.py write` refuses verified claims. Run it
  before converting final claims from pending to verified bindings.
- The ignored stale notebook exports under
  `../memd-bench/manuscript/notebooks/` remain local. Do not delete them without
  Chef's separate destructive-cleanup approval.
- The pinned SuperLocalMemory retrieval worker has not run against the real
  600-question benchmark because a clean committed harness identity is required
  first. Fixture integration and real-source preflight have passed.
- memd and SuperLocalMemory retrieval latency measurements use different
  execution paths. Do not plot or claim a direct latency comparison.
- The current llama.cpp container inherits a health check for port 8080 while
  its API listens on 8000. The host health endpoint returns HTTP 200, but Docker
  labels the container unhealthy. Override the health port when recreating it.
- crates.io already contains memd 1.4.0. Choose a later version only after the
  evidence and manuscript gates pass; merging a version bump to main can
  trigger publication.

## Moving Forward

### Next Steps

1. Obtain Chef's approval, stage only the paths in
   `tasks/proposed-commit-scope.md`, inspect both staged diffs, and create the two
   proposed commits without pushing.
2. Reproduce the clean-clone build with the preflighted answer and judge
   endpoints; run LoCoMo, CodeIR, MemoryData, LongMemEval, and longitudinal
   protocols from
   fresh committed sources, validate every phase manifest, and build one compact
   immutable evidence bundle.
3. Regenerate the manuscript and MemoryData figure from that bundle, add exact
   claim bindings, pass strict rendering plus assertion verification, and run
   independent scientific review.
4. Choose the release version and request separate approval for push, merge,
   artifact publication, and release.

### Blockers

- Explicit approval for the two local commits.
- Final version choice and separate outward-facing release approval after the
  evidence gates pass.
