# Benchmark Scripts

This directory contains the benchmark entrypoints that still matter in this repo.

## Keep

- `run_offline_retrieval_benchmark.sh`
- `run_variant_matrix_benchmark.sh`
- `run_longmemeval_benchmark.sh`
- `run_task_memory_benchmark.sh`

These scripts run directly with local `cargo` and Python. Docker is not required.

## Offline retrieval benchmark

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
  --genesism-root ../../genesisM
```

Outputs:

- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.json`
- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.md`

Corpus source:

- `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
