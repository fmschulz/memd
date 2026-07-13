# Evals

Benchmark suites and harnesses for `memd`. Each family lives in its own
directory and is independently runnable.

## Active gates

| Suite | Purpose | One-command |
|---|---|---|
| `bench/scripts/run_task_memory_benchmark.sh` | Internal task-memory corpus (cli_warm + cli_batch lanes). | shell script |
| `bench/scripts/run_offline_retrieval_benchmark.sh` | BEIR fiqa + scidocs retrieval gate (used by `.github/workflows/retrieval-gate.yml`). | shell script |
| [`bench/longitudinal/`](bench/longitudinal/) | Frozen repeated-task ablation for admission, staged consolidation, exposure compatibility, and verified outcome ranking. | `./evals/bench/longitudinal/run.sh` |

Current public LoCoMo, CodeIR, MemoryData, and LongMemEval protocols live in
the sibling `memd-bench` repository. The old in-repository LoCoMo harness is
preserved under `legacy/locomo-2026-05/`. The incomplete BEIR figure snapshot
is preserved under [`legacy/beir-2026-06/`](legacy/beir-2026-06/). Neither
archive is current evidence.

## Internal / experimental

The rest of `bench/` holds in-flight experiments —
agent-cost stress tests, cross-project usefulness pilots, paper-artifact
coordination evals, etc. Their READMEs explain what's still load-bearing
and what was superseded; see
[`bench/BENCHMARK_INVENTORY.md`](bench/BENCHMARK_INVENTORY.md) for the
current catalogue.

The CLI also includes threshold-gated maintenance evals for the local
memory-quality contract:

- `memd eval-memory-md` checks default startup context usefulness and
  generated-wrapper suppression. With `--agent-usefulness`, it also gates the
  deterministic startup briefing: latest project state, git state, next
  actions, scope health, continuation-fragment suppression, boilerplate-action
  suppression, and the default machine-wide item cap. `--gold-file` can run
  the same structural checks over local multi-project fixtures.
- `memd eval-retrieval` checks fixed known-useful retrieval queries and
  sparse-judgment metrics. It reports precision@k, but the default bundled
  sparse judgments gate on hit-rate unless you pass explicit recall, MRR, or
  precision thresholds.
- `memd eval-write-quality` runs an isolated synthetic write session and
  checks low-value rejection/downgrade rate, exact duplicate reuse, bounded
  chunk/disk growth, default TTL assignment for routine progress summaries,
  durable retrieval before/after retention compaction, expired-row hiding, and
  hidden ephemeral progress.

## Conventions

- Per-suite results live under `<suite>/results/`. Generated stores and large
  datasets are gitignored. A public result is claim-bearing only after the
  corresponding `memd-bench` artifact bundle validates.
- Each suite has a `README.md` explaining what it measures, what it
  doesn't, and known limitations.
- Adapters are pluggable: drop a new file in
  `<suite>/tools/adapters/<system>.py`, wire it into the runner, and
  the harness picks it up.
