# Offline Retrieval Benchmark Protocol

This document defines the canonical Phase 6 benchmark procedure for retrieval quality in `memd`.

## Goal

Measure retrieval quality on labeled corpora with reproducible commands and machine-readable outputs suitable for CI, release gates, and regression tracking.

## Datasets

Primary challenging datasets:

- `evals/datasets/retrieval/beir_fiqa.json`
- `evals/datasets/retrieval/beir_scidocs.json`
- `evals/datasets/retrieval/beir_trec-covid.json`
- `LongMemEval` (`s`/`m` cleaned splits via `evals/scripts/run_longmemeval_benchmark.sh`)

Smoke dataset for fast gates:

- `evals/datasets/retrieval/code_pairs.json`

## Metrics

Per query:

- `Recall@10`
- `MRR`
- `Precision@10`
- `latency_ms`

Aggregate:

- Mean + 95% bootstrap confidence interval for each metric
- Query-count `n`

## Determinism

- Bootstrap is seeded (`--seed`, default `42`).
- Bootstrap iterations are explicit (`--bootstrap-iterations`, default `1000`).
- Optional caps (`--max-queries`, `--max-documents`) make CI/manual runs bounded and reproducible.

## Runtime Throughput Knobs

For indexing-heavy runs (especially LongMemEval), these env vars are supported:

- `MEMD_EVAL_INGEST_BATCH_SIZE`: harness-side size for each `memory.add_batch` call.
  default: `32`.
- `MEMD_EMBED_BATCH_SIZE`: memd dense embed batch size used by Candle embedder.
  default: `32`.

Example:

```bash
MEMD_EVAL_INGEST_BATCH_SIZE=128 MEMD_EMBED_BATCH_SIZE=64 \
  cargo run -p memd-evals -- --suite benchmark --skip-build ...
```

## Command (single dataset)

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/datasets/retrieval/beir_fiqa.json \
  --embedding-model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42 \
  --report-json evals/results/offline/beir_fiqa_all-minilm.json
```

## Command (all three challenging datasets)

```bash
./evals/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

Outputs:

- One normalized cross-corpus JSON report per model:
  `evals/results/offline-<timestamp>/cross_corpus_<model>.json`
- `datasets[]` section includes per-dataset summaries and quality gate results.

Equivalent direct harness invocation:

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/datasets/retrieval/beir_fiqa.json \
  --dataset-path evals/datasets/retrieval/beir_scidocs.json \
  --dataset-path evals/datasets/retrieval/beir_trec-covid.json \
  --embedding-model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42 \
  --report-json evals/results/offline/cross_corpus_all-minilm.json
```

## Baseline Matrix Runner

Use this runner to compare strong retrieval variants on the same datasets:

```bash
./evals/scripts/run_variant_matrix_benchmark.sh \
  --model all-minilm \
  --with-longmemeval-s \
  --max-queries 200 \
  --max-sessions-per-query 40 \
  --seed 42
```

Variants covered by default:

- `hybrid-feature`
- `hybrid-cross-encoder`
- `dense-only`
- `bm25-only`

## Quality Gates

Two gate tiers are defined:

1. CI/release smoke gate (fast):
   - dataset: `code_pairs.json`
   - thresholds: `Recall@10 >= 0.8`, `MRR >= 0.6`
2. Offline benchmark gate (manual/nightly):
   - challenging BEIR datasets
   - thresholds are configured per run or maintained externally in release criteria

Example thresholded run:

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/datasets/retrieval/code_pairs.json \
  --threshold-recall 0.8 \
  --threshold-mrr 0.6
```

## LongMemEval Setup (Public Long-Term Memory Corpus)

The benchmark harness now accepts LongMemEval raw JSON directly and converts it
to session-level retrieval format at load time.

Run:

```bash
./evals/scripts/run_longmemeval_benchmark.sh \
  --split s \
  --model all-minilm \
  --max-queries 200 \
  --max-sessions-per-query 40 \
  --bootstrap-iterations 1000 \
  --seed 42
```

Notes:

- `--split s` (about 265 MB) is the recommended default for routine offline benchmarks.
- `--split m` (about 2.5 GB) is intended for large-scale/nightly evidence runs.
- Abstention questions are excluded by default to match LongMemEval retrieval conventions.
- Use `--include-abstention` only when explicitly measuring abstention retrieval behavior.
- You may also run directly via harness with:
  - `--dataset-path /path/to/longmemeval_s_cleaned.json`
  - optional `--max-sessions-per-query <n>`
  - optional `--include-abstention`

## Report Schema

Each benchmark JSON report includes:

- dataset metadata (`dataset_description`, `dataset_version`, `dataset_path`)
- run config (`embedding_model`, `bootstrap_iterations`, `seed`)
- thresholds used
- gate result (`quality_gate_passed`, `quality_gate_message`)
- aggregate metric block with CIs
- full `query_metrics` vector

For multi-dataset runs, the report switches to a cross-corpus schema:

- shared run config (`embedding_model`, `bootstrap_iterations`, `seed`, max limits)
- system variant (`system_variant`)
- normalization method (`macro_average_by_dataset`)
- `datasets[]` with per-dataset summaries
- `normalized_summary` across datasets (each dataset weighted equally)
- cross-corpus gate result (`quality_gate_passed`, `quality_gate_message`)

## Continuous Regression Gate (Statistical Significance)

Use `benchmark-regression` to compare a candidate run against a stored baseline report.

```bash
cargo run -p memd-evals -- --suite benchmark-regression --skip-build \
  --baseline-report evals/results/offline/baseline.json \
  --candidate-report evals/results/offline/candidate.json \
  --significance-alpha 0.05 \
  --min-effect-size 0.1 \
  --regression-report-json evals/results/offline/regression_gate.json
```

CI/release smoke baseline report is pinned at:

- `evals/baselines/code_pairs_hybrid_feature_baseline.json`

Gate behavior:

- aligns query metrics by `query_id`
- evaluates `recall_at_10`, `mrr`, `precision_at_10`
- fails only when degradation is both statistically significant (`p <= alpha`) and practically meaningful (`|Cohen's d| >= min_effect_size`)
- emits machine-readable report JSON when `--regression-report-json` is provided
