# Retrieval Datasets

Only the small smoke dataset is tracked in git:

- `code_pairs.json`

Large offline benchmark datasets are intentionally not tracked. They split
into three tiers by how the fetcher treats them:

| Tier | Dataset | Fetcher behavior | Consumer |
|---|---|---|---|
| required | `beir_fiqa.json` | downloads, fails on 404 | `--suite benchmark` |
| required | `beir_scidocs.json` | downloads, fails on 404 | `--suite benchmark` |
| optional | `beir_trec-covid.json` | attempts, skips on 404 | `--suite benchmark` |
| manual | `beir_scifact_fixed.json` | never fetched | `--suite scifact` |
| manual | `beir_nfcorpus.json` | never fetched | `--suite nfcorpus` |

The `scifact` and `nfcorpus` suites are hardwired to those file names
(`evals/harness/src/suites/scifact.rs:109`,
`evals/harness/src/suites/nfcorpus.rs:107`). Place local copies at those
paths before invoking those suites.

Why:

- they are large enough to bloat normal clones
- they change infrequently
- they are benchmark inputs, not source code

## Fetch the mirrored datasets

```bash
./evals/bench/scripts/fetch_offline_benchmark_datasets.sh
```

Downloads the required tier into this directory; attempts the optional tier
and silently skips it when the pinned commit mirror doesn't carry it
(currently `beir_trec-covid.json` — place manually if you need it).

## Driving the harness with a manifest

`--suite benchmark` accepts a dataset manifest so you don't need to repeat
`--dataset-path` for every file:

```bash
cargo run -p memd-evals -- --suite benchmark \
  --dataset-manifest evals/bench/beir_manifest.toml
```

Manifest entries can coexist with explicit `--dataset-path` flags — the
two sets concatenate. Relative paths in the manifest resolve against the
manifest's own directory.

## Run benchmarks without the large datasets

If only `code_pairs.json` is present, the offline benchmark entrypoint
still works as a smoke benchmark.

## Reproducibility note

The mirror script currently fetches pinned JSON exports from commit:

- `7e1702284a382160ba5ef0493e741bdba95fccf2`

This keeps the fetch path reproducible even though the large files are no
longer tracked in the main branch tip. `beir_trec-covid.json` is not in
that commit's tree (see `try_fetch_one` in
`evals/bench/scripts/fetch_offline_benchmark_datasets.sh`), so the fetcher
treats it as optional; a future commit bump can move it into the required
tier.
