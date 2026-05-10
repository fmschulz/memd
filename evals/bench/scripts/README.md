# Benchmark Scripts

This directory contains the benchmark entrypoints that still matter in this repo.

For a map of completed runs and where their result artifacts live, see
[`../BENCHMARK_INVENTORY.md`](../BENCHMARK_INVENTORY.md). For the next proposed
agent benchmark, see
[`../memd-multiturn-token-savings/README.md`](../memd-multiturn-token-savings/README.md).

## Keep

- `run_offline_retrieval_benchmark.sh`
- `run_variant_matrix_benchmark.sh`
- `run_longmemeval_benchmark.sh`
- `run_task_memory_benchmark.sh`

These scripts run directly with local `cargo` and Python. Docker is not required.

## Offline retrieval benchmark

Fetch the mirrored large datasets first when you want more than the smoke benchmark:

```bash
./evals/bench/scripts/fetch_offline_benchmark_datasets.sh
```

If you skip that step, the offline benchmark script will still run on the tracked `code_pairs.json` smoke dataset only.

```bash
./evals/bench/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

## Variant matrix benchmark

```bash
./evals/bench/scripts/run_variant_matrix_benchmark.sh \
  --model all-minilm \
  --with-longmemeval-s \
  --max-queries 200 \
  --max-sessions-per-query 40 \
  --seed 42
```

## LongMemEval benchmark

```bash
./evals/bench/scripts/run_longmemeval_benchmark.sh \
  --split s \
  --model all-minilm \
  --system-variant hybrid-feature
```

## Task-memory benchmark

```bash
./evals/bench/scripts/run_task_memory_benchmark.sh
```

Useful options:

```bash
./evals/bench/scripts/run_task_memory_benchmark.sh \
  --workers 1 \
  --ops-per-worker 1 \
  --top-k 3 \
  --memd-lanes cli_cold cli_warm cli_batch \
  --genesism-root ../../genesisM
```

`--memd-lanes` accepts `cli_cold`, `cli_warm`, and `cli_batch`. The warm lane
uses the private `memd warm` CLI worker, and the batch lane uses
`memd batch --jsonl`; neither lane registers external client tools.

Outputs:

- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.json`
- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.md`

Corpus source:

- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
