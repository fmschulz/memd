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
- `nDCG@{1, 5, 10, 100}` (BEIR standard, `2^rel - 1` gain with `log2(rank + 1)` discount)

Aggregate reports include bootstrap confidence intervals for recall / MRR / precision / latency. `nDCG@k` is reported as a mean per cutoff via `ndcg_at_k` (BTreeMap for deterministic JSON output); per-query nDCG values live on `query_metrics[].ndcg_at_k` so the regression gate can pair across baseline / candidate reports.

Graded qrels are supported via an additive `queries[].relevance_grades: {doc_id: u8}` field on each dataset JSON. Binary-relevance datasets (the currently mirrored fiqa / scidocs / trec-covid exports) read as grade-1 for every entry in `relevant[]`.

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

## BEIR nDCG@10 regression gate

The BEIR baseline at `evals/bench/baselines/beir_v1.json` is the cross-corpus reference used by `.github/workflows/retrieval-gate.yml`. The gate pairs queries by `(dataset_path, query_id)` and runs a paired `t`-test on `nDCG@10` with Cohen's `d`.

Pinned parameters (keep these in lockstep with the CI env vars and the baseline regeneration command — any change to any one invalidates the committed baseline):

| Parameter | Value | Source |
|---|---|---|
| datasets | `beir_fiqa.json`, `beir_scidocs.json` | mirror fetcher (trec-covid excluded until mirrored) |
| embedding model | `all-minilm` | `--embedding-model` default |
| system variant | `hybrid-feature` | `--system-variant` default |
| bootstrap iterations | `1000` | CI env `BEIR_BOOTSTRAP_ITERATIONS` |
| seed | `42` | CI env `BEIR_SEED` |
| max queries per dataset | `30` | CI env `BEIR_MAX_QUERIES` |
| max documents per dataset | `500` | CI env `BEIR_MAX_DOCUMENTS` |
| gate metric | `ndcg_at_10` | CI env `BEIR_METRIC` |
| significance alpha | `0.05` | CI env `BEIR_SIGNIFICANCE_ALPHA` |
| min effect size | `0.05` | CI env `BEIR_MIN_EFFECT_SIZE` |

## Regenerating `beir_v1.json`

Any change to the pinned parameters — embedding model, system variant, max-queries, max-documents, or the retrieval path under test — invalidates the committed baseline. Follow this ritual to refresh it:

1. Fetch the mirror (`fiqa` + `scidocs` at minimum):
   ```bash
   ./evals/bench/scripts/fetch_offline_benchmark_datasets.sh
   ```
2. Build release memd + memd-evals:
   ```bash
   cargo build --release -p memd -p memd-evals
   ```
3. Regenerate the baseline against the pinned parameter set:
   ```bash
   ./target/release/memd-evals \
     --memd-path ./target/release/memd \
     --skip-build \
     --suite benchmark \
     --dataset-path evals/bench/datasets/retrieval/beir_fiqa.json \
     --dataset-path evals/bench/datasets/retrieval/beir_scidocs.json \
     --system-variant hybrid-feature \
     --seed 42 \
     --bootstrap-iterations 1000 \
     --max-queries 30 \
     --max-documents 500 \
     --report-json evals/bench/baselines/beir_v1.json
   ```
4. Two-PR change process:
   - **PR 1**: land the substantive change (new embedding model, new indexing behavior, tuned retrieval parameters). The retrieval-gate CI job will fail because the committed baseline no longer describes the current system. That's expected.
   - **PR 2**: commit the regenerated `beir_v1.json` with a written justification in the PR description (what changed, why the baseline had to move, which metric means shifted by how much).

   Splitting the change this way keeps the baseline's moves traceable and auditable. A single PR that bundles code + baseline makes review noisier and hides whether a baseline move is intentional.
