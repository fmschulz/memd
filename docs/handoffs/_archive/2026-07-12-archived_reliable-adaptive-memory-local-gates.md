# Handoff: Reliable adaptive memory local gates

## Status

Milestones 1–6 are implemented. The frozen longitudinal run keeps
`outcome-v1` shadow-only because the full loop failed recall non-regression.
Milestone 7 benchmark infrastructure and most Milestone 10 hygiene are
implemented in the working trees. The current Rust tree passes the complete
workspace gate and strict all-target clippy.

## Verified results

- `cargo fmt --all -- --check && cargo check --workspace && cargo test --workspace`
  passes. The main library passed 974 tests with five intentional ignores;
  every integration target, 69 evaluation-harness tests, and doctests passed.
- `cargo clippy --workspace --all-targets -- -D warnings` passes. The cleanup
  reduced 109 diagnostics to zero and converted test-global locks to async
  mutexes.
- `memd eval-memory-md --agent-usefulness` passes with a useful ratio of 1.0,
  no generated wrappers, and all answerability checks true.
- `../memd-bench` passes Ruff, compileall, shell syntax, whitespace, and 26
  benchmark-infrastructure tests. No public claim-bearing run has been made.
- Version consistency passes and `cargo publish --dry-run --allow-dirty`
  packages and compiles 172 files.

## Decisions

- Keep outcome-aware serving disabled. Preserve longitudinal protocol v1 and
  its failed recall gate unchanged.
- Do not manufacture `MEMD_SOURCE_COMMIT` from a dirty tree and do not put old
  LoCoMo, CodeIR, or MemoryData values back into the manuscript.
- Treat a merge of a version bump to `main` as the release action: it creates a
  tag that triggers crates.io publication and the GitHub Release workflow.
- Do not continue the large-module refactors without an independent review
  path; two configured reviewer attempts timed out.

## Important correction

An initial warm `agent-context` failure was not a migration-code defect. A
pre-migration warm worker remained live while both old and new development
binaries reported version 1.4.0. Cold retrieval succeeded, and stopping and
starting the worker with the current binary restored warm retrieval. A real
release version bump will make the existing version/protocol compatibility
check reject the old worker.

## Required decisions and blockers

1. Chef must choose the next release version. Version 1.4.0 already exists on
   crates.io.
2. Chef must approve commits before either repository receives a commit.
3. Public QA and confirmatory runs need pinned answer and judge endpoint
   configurations. No endpoint is currently configured.
4. Module splits and final manuscript review need a working independent review
   route; the current Claude review route timed out twice.

## Next steps

1. On approval, commit the memd tree first so the benchmark has a real source
   identity; rerun the exact local gates on that commit.
2. Configure the pinned answer and judge endpoints, run the frozen CodeIR,
   LoCoMo, MemoryData, and LongMemEval protocols, and build the immutable
   artifact bundle.
3. Bind every manuscript number and figure to that bundle, run assertion
   checks, and obtain independent scientific review.
4. Complete the behavior-preserving module splits only with an available
   independent reviewer.

Detailed commands and results are in `tasks/METHODS.md`; live checklist state
is in `tasks/2026-07-12-memd-reliable-adaptive-memory-goal.md` and
`tasks/todo.md`.
