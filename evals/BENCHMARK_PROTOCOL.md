# Offline Retrieval Benchmark Protocol

This is the kept offline retrieval benchmark flow.

## Datasets

Challenging datasets:

- `evals/bench/datasets/retrieval/beir_fiqa.json`
- `evals/bench/datasets/retrieval/beir_scidocs.json`
- `evals/bench/datasets/retrieval/beir_trec-covid.json`

Smoke dataset:

- `evals/bench/datasets/retrieval/code_pairs.json`

Only `code_pairs.json` is tracked in git. The larger BEIR-format JSON exports are intentionally not tracked at branch tip.

Fetch the mirrored datasets with:

```bash
./evals/bench/scripts/fetch_offline_benchmark_datasets.sh
```

Current mirror coverage:

- `beir_fiqa.json`
- `beir_scidocs.json`

`beir_trec-covid.json` is not mirrored by that helper because the converted JSON is too large for the normal repository workflow. If you have a local converted copy, place it at the path above and pass it explicitly through `--dataset-path`.

## Metrics

- `Recall@10`
- `MRR`
- `Precision@10`
- `latency_ms`

Aggregate reports include bootstrap confidence intervals.

## Determinism

- `--seed` controls bootstrap randomness
- `--bootstrap-iterations` controls the CI sample count
- `--max-queries` and `--max-documents` keep runs bounded

## Single dataset

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/bench/datasets/retrieval/beir_fiqa.json \
  --embedding-model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42 \
  --report-json evals/bench/results/offline/beir_fiqa_all-minilm.json
```

## Cross-corpus run

```bash
./evals/bench/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

## Quality gates

Fast smoke gate:

- dataset: `code_pairs.json`
- thresholds: `Recall@10 >= 0.8`, `MRR >= 0.6`

Example:

```bash
cargo run -p memd-evals -- --suite benchmark --skip-build \
  --dataset-path evals/bench/datasets/retrieval/code_pairs.json \
  --threshold-recall 0.8 \
  --threshold-mrr 0.6
```

## Regression check

```bash
cargo run -p memd-evals -- --suite benchmark-regression --skip-build \
  --baseline-report evals/bench/baselines/code_pairs_hybrid_feature_baseline.json \
  --candidate-report /tmp/candidate.json \
  --significance-alpha 0.05 \
  --min-effect-size 0.1 \
  --regression-report-json /tmp/regression_gate.json
```
