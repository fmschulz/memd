# Evals

Benchmark suites and harnesses for `memd`. Each family lives in its own
directory and is independently runnable.

## Active gates

| Suite | Purpose | One-command |
|---|---|---|
| `bench/scripts/run_task_memory_benchmark.sh` | Internal task-memory corpus (cli_warm + cli_batch lanes). | shell script |
| `bench/scripts/run_offline_retrieval_benchmark.sh` | Local retrieval benchmark on code_pairs and available BEIR datasets. | shell script |
| [`bench/longitudinal/`](bench/longitudinal/) | Frozen repeated-task ablation for admission, staged consolidation, exposure compatibility, and verified outcome ranking. | `./evals/bench/longitudinal/run.sh` |

Current public LoCoMo, CodeIR, MemoryData, and LongMemEval protocols live in
the sibling `memd-bench` repository. See
[`bench/BENCHMARK_INVENTORY.md`](bench/BENCHMARK_INVENTORY.md) for the
retained internal checks and fixtures.

`memd-evals --suite all` runs CLI contract, persistence, and retrieval
checks using the tracked `code_pairs.json` fixture. Benchmark and longitudinal
runs require their explicit inputs and use separate suite names.

The CLI also includes threshold-gated maintenance evals for the local
memory-quality contract:

- `memd eval-memory-md` checks startup context usefulness and generated-wrapper
  suppression. It writes `memory-eval.md` by default and rejects the project's
  `memory.md` as an output destination. `--gold-file` checks explicit text and
  count expectations over local project fixtures.
- `memd eval-retrieval` checks fixed known-useful retrieval queries and
  sparse-judgment metrics. It reports precision@k, but the default bundled
  sparse judgments gate on hit-rate unless you pass explicit recall, MRR, or
  precision thresholds.
## Conventions

- Per-suite results live under `<suite>/results/`. Generated stores and large
  datasets are gitignored. A public result is claim-bearing only after the
  corresponding `memd-bench` artifact bundle validates.
- Each suite has a `README.md` explaining what it measures, what it
  doesn't, and known limitations.
