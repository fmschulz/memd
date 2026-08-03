# Handoff: LoCoMo - Fixed-Clock Wire Contract

**Date:** 2026-07-14
**Branch:** `feat/reliable-adaptive-memory`
**HEAD:** `e59067e`

## Context & Status

The fixed-clock product and LoCoMo harness changes remain uncommitted while
waiting for approval specific to this diff. The source gates pass in both
repositories. A real batch probe exposed one missing wire guarantee:
`SearchResult.retrieval_episode_id = None` was omitted by Serde, so the harness
could not distinguish a fixed-clock response from an older binary that ignored
`ranking_time_ms`. The field now serializes as explicit JSON `null`, and a Rust
test checks the serialized MCP payload.

The sibling benchmark repository is on
`feat/reproducible-benchmark-evidence` at `99c37fe`. Its eight fixed-clock
harness files remain uncommitted.

## Technical Implementation

### Work Completed

- Removed `skip_serializing_if` from
  `SearchResult.retrieval_episode_id` and documented explicit null as the
  read-only replay acknowledgement (`crates/memd/src/ops/shared_types.rs`).
- Added a serialized-payload assertion to the fixed-clock attribution test
  (`crates/memd/src/ops/tests.rs`).
- Documented the response contract in `CHANGELOG.md`.
- Reran the complete Rust workspace gate and the 96-test benchmark gate.
- Obtained an independent Claude source review with no blocker. The review is
  stored at
  `tasks/reviews/20260714-fixed-clock-wire/claude.stream.jsonl`.
- Audited the diagnostic command against `memd-bench/AGENTS.md`. The observed
  1,531/1,531 match is rejected as benchmark evidence because the command used
  a binary outside the benchmark repository.

### Outcomes

- **What worked:** 984 Rust library tests passed with five intentional model
  download ignores; every integration target, workspace check, strict Clippy,
  formatting, and diff check passed. All 96 benchmark tests, Ruff checks, and
  protocol JSON validation passed.
- **What didn't:** The first batch probe failed closed because the response
  omitted `retrieval_episode_id`. Explicit-null serialization fixed the wire
  contract. The later full diagnostic crossed the benchmark repository
  boundary and therefore cannot support a release or manuscript claim.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `CHANGELOG.md` | Modified | Documents fixed-clock ranking and explicit-null acknowledgement. |
| `crates/memd/src/ops/shared_types.rs` | Modified | Defines `ranking_time_ms` and the always-serialized episode field. |
| `crates/memd/src/ops/tests.rs` | Modified | Pins explicit null at the serialized MCP layer. |
| `crates/memd/src/ops/search.rs` | Modified | Applies read-only fixed-clock search behavior. |
| `crates/memd/src/store/` | Modified | Propagates the fixed clock through ranking and feedback paths. |
| `../memd-bench/benchmarks/locomo/` | Modified | Pins one clock across paired retrieval and rejects missing acknowledgement. |
| `../memd-bench/benchmarks/tests/` | Modified | Covers clock propagation, arm order, and acknowledgement. |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Always serialize `retrieval_episode_id`, using null when no episode exists. | Older binaries silently accept unknown request fields. Explicit null lets the harness fail closed without changing normal string consumers. |
| Keep the full cross-boundary run as debugging information only. | `memd-bench/AGENTS.md` permits only repo-local binaries and stores. Publication requires a clean, repo-local build from committed source. |
| Do not start paired QA before immutable 1,531/1,531 invariance. | QA is expensive and cannot support the event-time claim if retrieval contexts differ. |

## Knowledge Capture

### Lessons Learned

- An internal `Option::None` assertion does not prove JSON field presence.
  Test the serialized response when key presence is part of a protocol.
- Read the sibling repository instructions before running commands there. A
  technically sound result can still be inadmissible when the execution
  crosses its declared workspace boundary.

### Gotchas

- The existing v3 artifact remains bound to memd `e59067e` and memd-bench
  `99c37fe`; do not overwrite or relabel it.
- `ranking_time_ms` pins ranking inputs over the current corpus. It does not
  provide historical lifecycle visibility.
- The two existing handoff files are untracked and excluded from the proposed
  source commit.

## Moving Forward

### Next Steps

1. Obtain approval for the exact current memd and memd-bench commits and
   feature-branch pushes.
2. Stage named paths only, inspect both cached diffs, commit memd as
   `feat(search): add reproducible ranking clock`, and commit memd-bench as
   `fix(locomo): pin paired retrieval clock`.
3. Push both feature branches and verify the remote heads.
4. In a fresh memd-bench clone, run `benchmarks/reproduce.sh` for the new
   40-character memd commit, seed a fresh store, and require immutable
   1,531/1,531 invariance.
5. Run paired QA only after that gate, then complete CodeIR, MemoryData,
   longitudinal evidence, bundle validation, and manuscript regeneration.

### Blockers

- Commit and push approval must name these current fixed-clock diffs. Prior
  approvals covered earlier commits and have expired.
