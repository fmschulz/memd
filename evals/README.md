# Evals

Benchmark suites and harnesses for `memd`. Each family lives in its own
directory and is independently runnable.

## Public-facing

| Suite | Purpose | One-command |
|---|---|---|
| [`benchmarks/locomo/`](benchmarks/locomo/) | **Headline.** Cross-system retrieval on upstream LoCoMo. memd vs Mem0 vs SuperLocalMemory. | `./evals/benchmarks/locomo/run.sh` |
| `bench/scripts/run_task_memory_benchmark.sh` | Internal task-memory corpus (cli_warm + cli_batch lanes). | shell script |
| `bench/scripts/run_offline_retrieval_benchmark.sh` | BEIR fiqa + scidocs retrieval gate (used by `.github/workflows/retrieval-gate.yml`). | shell script |

## Internal / experimental

The rest of `bench/` holds in-flight and historical experiments —
agent-cost stress tests, cross-project usefulness pilots, paper-artifact
coordination evals, etc. Their READMEs explain what's still load-bearing
and what was superseded; see
[`bench/BENCHMARK_INVENTORY.md`](bench/BENCHMARK_INVENTORY.md) for the
current catalogue.

The CLI also includes threshold-gated maintenance evals for the local
memory-quality contract:

- `memd eval-memory-md` checks default startup context usefulness and
  generated-wrapper suppression.
- `memd eval-retrieval` checks fixed known-useful retrieval queries and
  sparse-judgment metrics.
- `memd eval-write-quality` runs an isolated synthetic write session and
  checks low-value rejection/downgrade rate, exact duplicate reuse, bounded
  chunk/disk growth, durable retrieval before/after retention compaction,
  expired-row hiding, and hidden ephemeral progress.

## Conventions

- Per-suite results live under `<suite>/results/`. Result data
  directories (qdrant, sqlite, etc.) and the raw LoCoMo dataset are
  gitignored; curated baselines and the per-system markdown summaries
  are checked in.
- Each suite has a `README.md` explaining what it measures, what it
  doesn't, and known limitations.
- Adapters are pluggable: drop a new file in
  `<suite>/tools/adapters/<system>.py`, wire it into the runner, and
  the harness picks it up.
