# Benchmark figure notebooks

Reproducible notebooks that turn the checked-in benchmark result snapshots into
the publication figures embedded in [`docs/benchmarking.md`](../../docs/benchmarking.md).

| Notebook | Renders | Figures (`docs/figures/`) |
|---|---|---|
| `locomo_cross_system.ipynb` | LoCoMo retrieval: memd vs mem0 vs SuperLocalMemory | `locomo_quality`, `locomo_quality_latency`, `locomo_per_category` |
| `beir_retrieval_gate.ipynb` | BEIR FiQA/SciDocs offline gate + regression vs baseline | `beir_ndcg_curves`, `beir_metrics_ci`, `beir_regression_gate` |

## Run

```bash
bash evals/notebooks/run_notebooks.sh
```

This regenerates the `.ipynb` files from their cell sources
(`build_notebooks.py`), executes them, and writes PNG+SVG to `docs/figures/`.
Requires [`uv`](https://docs.astral.sh/uv/); no global Python setup needed.

## Inputs (checked-in snapshots)

The notebooks read only `data/`, so they are deterministic and need no live
benchmark run:

- `data/locomo_2026-05-22.json` — summary slice of the LoCoMo results report
  (`evals/benchmarks/locomo/results/`).
- `data/beir_cross_corpus_2026-06-11.json` — current-code BEIR cross-corpus
  report (the CI `retrieval-gate` parameters).
- `data/beir_regression_2026-06-11.json` — paired-query nDCG@10 gate vs. the
  checked-in `evals/bench/baselines/beir_v1.json` baseline.

To refresh the inputs from a live run, regenerate the reports with the
commands in `docs/benchmarking.md` and copy the JSON into `data/`. Shared plot
styling and JSON loaders live in `memd_plotting.py`.
