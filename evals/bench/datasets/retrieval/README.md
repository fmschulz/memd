# Retrieval Datasets

Only the small smoke dataset is tracked in git:

- `code_pairs.json`

Large offline benchmark datasets are intentionally not tracked:

- `beir_fiqa.json`
- `beir_scidocs.json`
- `beir_trec-covid.json`

Why:

- they are large enough to bloat normal clones
- they change infrequently
- they are benchmark inputs, not source code

## Fetch the mirrored datasets

For the currently mirrored JSON exports, run:

```bash
./evals/bench/scripts/fetch_offline_benchmark_datasets.sh
```

That script downloads:

- `beir_fiqa.json`
- `beir_scidocs.json`

into this directory.

## `beir_trec-covid.json`

`beir_trec-covid.json` is not mirrored in git because the converted JSON is too large for the normal repository workflow.

If you already have a local converted copy, place it at:

```text
evals/bench/datasets/retrieval/beir_trec-covid.json
```

and the benchmark harness can use it through repeated `--dataset-path` arguments.

## Run benchmarks without the large datasets

If only `code_pairs.json` is present, the offline benchmark entrypoint still works as a smoke benchmark.

## Reproducibility note

The mirror script currently fetches pinned JSON exports from commit:

- `7e1702284a382160ba5ef0493e741bdba95fccf2`

This keeps the fetch path reproducible even though the large files are no longer tracked in the main branch tip.
