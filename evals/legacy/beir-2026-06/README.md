# Archived BEIR development snapshot

This directory preserves the June 2026 BEIR fixture reports, their plotting
notebook, and the rendered figures. They are historical development artifacts,
not current release or cross-system evidence.

The paired regression report records its candidate input as
`/tmp/memd-bench/candidate-final.json`. That source report is absent, and the
snapshot has no immutable phase manifest, source commit, binary digest, or
content-addressed bundle. The reported values therefore cannot satisfy the
current provenance contract.

Use [`evals/bench/scripts/run_offline_retrieval_benchmark.sh`](../../bench/scripts/run_offline_retrieval_benchmark.sh)
for the active in-repository regression gate. Public comparisons belong to the
sibling `memd-bench` protocols and require verified artifact bundles.

## Inventory

- `notebooks/data/` contains the original cross-corpus and paired-regression
  JSON snapshots.
- `notebooks/beir_retrieval_gate.ipynb` and its helpers contain the original
  figure workflow. Paths inside these files reflect their former location.
- `figures/` contains the rendered PNG and SVG outputs.

Keep this directory frozen. Retrieve later revisions from Git history instead
of editing these artifacts in place.
