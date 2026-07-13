# Longitudinal memory-policy benchmark

## Purpose

This benchmark tests whether repeated, verified task outcomes help an agent
recover from stale memory without introducing cross-scope or consolidation
regressions. It is a deterministic policy benchmark. It does not estimate
open-ended software-engineering performance or replace LoCoMo, CodeIR, or
MemoryData.

The machine-readable protocol is `protocol.v1.json`; the frozen task clusters
are in `fixtures.v1.json`. Changing either file creates a new protocol version
and invalidates comparisons with prior runs.

## Experimental unit

The resampling unit is one task cluster. Each of the 32 clusters represents a
different subsystem and contains a correct rule, a later-arriving stale rule,
a correction record, and lexical distractors. The stale rule is deliberately
written after the correction record. This creates the failure mode under test:
recency can put a plausible but obsolete fact ahead of the correction.

Each treatment runs the same clusters and round order in a fresh persistent
store. A deterministic task agent selects the first actionable retrieved rule
for that cluster. The verifier passes only when the selected rule matches the
frozen corrected value. Selecting the stale value is a harmful-memory event.
This makes task success reproducible while keeping retrieval order load-bearing.

## Timeline

1. Seed the correct record, correction record, stale record, and distractors.
2. Run one pre-feedback task and record its result.
3. Apply treatment-specific feedback from that result.
4. At the configured checkpoint, stage deterministic consolidation. Treatments
   that include consolidation promote only a validated candidate through the
   journaled review path.
5. Run four post-feedback variants of the same task. Outcome treatments replay
   the recorded shadow order as the counterfactual served order; production
   `serve` mode remains disabled.
6. Run a project-scope probe and collect store, latency, token-proxy, outcome,
   and consolidation counters.

The evaluator never reports an outcome for a chunk that was not rendered. A
successful result credits only the selected corrected chunk. A failed result
marks only the selected stale chunk as harmful. Missing-memory failures remain
unattributed.

## Treatments

| ID | Admission | Consolidation | Ranking |
|---|---:|---:|---|
| `no_memory` | no | no | none |
| `raw_memory` | bypassed | no | base |
| `admission_only` | yes | no | base |
| `staged_consolidation` | yes | staged and explicitly promoted | base |
| `exposure_compat` | yes | no | rendered-count compatibility heuristic |
| `outcome_only` | yes | no | outcome-v1 shadow replay |
| `full_loop` | yes | staged and explicitly promoted | outcome-v1 shadow replay |

`exposure_compat` exists only as a historical ablation. The evaluator adds
`0.05 * min(prior renders, 4)` to the base score, for a maximum adjustment of
`0.20`. It must not re-enter product ranking.

## Measures

The primary measure is post-feedback task success. Secondary measures are
verifier pass rate, stale-error recurrence, harmful-memory rate, recall@3,
MRR, served-versus-shadow top-k changes, bytes rendered, latency, active-memory
growth, consolidation source coverage, and counts of staged, promoted,
rejected, rolled-back, recoverable, and scope-violation events.

Protocol v1 defines task success as passing its deterministic verifier, so
`post_task_success` and `verifier_pass_rate` are numerically identical. Both
fields are retained in the frozen schema, but they are one measure rather than
independent corroboration. A future protocol with separate task completion and
verification states must version the schema.

Latency is wall-clock time around the retrieval call. The token proxy is
`ceil(rendered UTF-8 bytes / 4)` and is labeled as a proxy. Store growth is the
change in the treatment data directory's allocated file bytes after seeding.

## Frozen promotion gates

All confidence intervals use 10,000 cluster bootstrap replicates with seed
`20260712`.

- The 95% interval for `full_loop - raw_memory` post-feedback task success has
  a lower bound above zero.
- Full-loop stale-error recurrence does not exceed raw memory.
- The upper 95% bound for `full_loop - raw_memory` harmful-memory rate is at
  most `0.02` absolute.
- Full-loop recall@3 and MRR do not regress relative to raw memory.
- Scope and crash/recovery invariant violations are zero. Crash recovery is
  also gated by the product's table-driven SIGKILL integration test; the
  benchmark manifest records that test artifact rather than treating a normal
  trial as a crash.
- Token, latency, memory-growth, and consolidation-cost measures are always
  reported. This protocol sets no performance cap.

If any gate fails, outcome ranking remains disabled by default. Correctness and
telemetry changes may ship with `outcome-v1` in shadow mode.

## Artifacts

One run writes an immutable directory named by its run ID. It contains:

- `manifest.<run_id>.json`: source, binary, protocol, fixture, environment,
  parent, command, and output identities;
- `rows.<run_id>.jsonl`: one row per treatment, cluster, and round;
- `summary.<run_id>.json`: treatment aggregates, paired deltas, confidence
  intervals, and gate decisions;
- `counterfactual.<run_id>.json`: served-versus-shadow rows;
- `inventory.<run_id>.sha256`: hashes for every file in the directory.

The runner refuses to overwrite an existing run directory. A later phase may
copy this compact directory into `memd-bench`, but it must preserve all hashes.

## Interpretation limits

The fixtures intentionally make stale and corrected facts lexically similar.
They test policy adaptation under controlled ambiguity, not semantic breadth.
The deterministic agent removes answer-model variance and therefore supports
causal comparison of memory policies, but its success rate must not be written
up as general agent-task accuracy.
