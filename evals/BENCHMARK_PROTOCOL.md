# Offline Retrieval Benchmark Protocol

This document defines the canonical Phase 6 benchmark procedure for retrieval quality in `memd`.

## Goal

Measure retrieval quality on labeled corpora with reproducible commands and machine-readable outputs suitable for CI, release gates, and regression tracking.

## Datasets

Primary challenging datasets:

- `evals/datasets/retrieval/beir_fiqa.json`
- `evals/datasets/retrieval/beir_scidocs.json`
- `evals/datasets/retrieval/beir_trec-covid.json`

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

## Command (single dataset)

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/datasets/retrieval/beir_fiqa.json \
  --embedding-model all-minilm \
  --bootstrap-iterations 1000 \
  --seed 42 \
  --report-json evals/results/offline/beir_fiqa_all-minilm.json
```

## Command (all three challenging datasets)

```bash
./evals/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --bootstrap-iterations 1000 \
  --seed 42
```

Outputs:

- One JSON report per dataset/model
- `statistical_analysis.md`
- `statistical_analysis.json`

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

## Report Schema

Each benchmark JSON report includes:

- dataset metadata (`dataset_description`, `dataset_version`, `dataset_path`)
- run config (`embedding_model`, `bootstrap_iterations`, `seed`)
- thresholds used
- gate result (`quality_gate_passed`, `quality_gate_message`)
- aggregate metric block with CIs
- full `query_metrics` vector
