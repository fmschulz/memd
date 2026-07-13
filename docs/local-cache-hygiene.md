# Local cache hygiene

Large local caches are expected during development and benchmark work. They
are ignored by Git, but they should be reviewed before removal because some
contain expensive or non-reproducible intermediate results.

## Inventory

The repository audit found these local surfaces:

| Path | Observed size | Regeneration and risk |
| --- | ---: | --- |
| `target/` | 51 GB | Cargo build output; reproducible from the source and lockfile |
| `site/` | 5.8 MB | MkDocs output; reproducible with the strict documentation build |
| `../memd-bench/fable-work/` | 64 GB | External review workspace; remove only after its useful outputs are frozen elsewhere |
| `../memd-bench/run-output/` | 1.1 GB | Benchmark runs; preserve until manifests and bundles have been verified |
| `../memd-bench/benchmark-data/` | 116 MB | Datasets, source checkouts, and model/build metadata; some downloads are costly |
| `../memd-bench/.ruff_cache/` | 40 KB | Linter cache; disposable |

Sizes are a point-in-time audit, not limits. Recheck before cleanup:

```bash
du -sh target site \
  ../memd-bench/fable-work \
  ../memd-bench/run-output \
  ../memd-bench/benchmark-data \
  ../memd-bench/.ruff_cache 2>/dev/null
```

## Reviewed cleanup commands

Run these commands manually from the `memd` repository root. Inspect the paths
first; none is run automatically by the project.

```bash
# Reproducible Rust and documentation output.
cargo clean
rm -rf -- site

# Disposable benchmark linter cache.
rm -rf -- ../memd-bench/.ruff_cache
```

The following caches require an evidence check first:

```bash
# List immutable manifests and bundles before considering benchmark cleanup.
find ../memd-bench/run-output -type f \
  \( -name 'seed.*.json' -o -name 'retrieve.*.json' \
     -o -name 'qa.*.json' -o -name 'judge.*.json' \) -print
find ../memd-bench/benchmark-artifacts -maxdepth 2 -type f -print 2>/dev/null
```

Do not remove `run-output/`, `benchmark-data/`, or `fable-work/` until every
needed result is reproducible from a validated bundle. Never use `git clean`
as a cache-management shortcut: it cannot distinguish disposable output from
valuable ignored benchmark data.
