# Phase 5 Task Memory Benchmark

- Corpus: `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
- Version: `2026-03-21.v2`
- Generated at: `2026-03-21T23:20:30Z`
- Embedding model: `all-minilm`
- Search variant: `hybrid-feature`

## Corpus Design

- Cases: `8`
- Queries: `23`
- Shared-project sibling groups: `4`
- Purpose: Compare memd chunk-native baseline search against memd task-memory search on the same underlying task knowledge.
- Hardening: Sibling tasks share project scopes, tools, datasets, and overlapping vocabulary so project-scoped systems cannot separate cases trivially.

| Project | Cases | Shared datasets | Shared tools |
|---|---|---|---|
| phase5_auth_reliability | jwt_timezone_fix<br>jwt_refresh_grace_window | auth_logs@2026-03-21 | cargo-test |
| phase5_regulator_screening | mmseqs_marker_search<br>mmseqs_sigma_factor_search | screen_counts@v3 | mmseqs |
| phase5_event_bus_selection | kafka_queue_selection<br>nats_jetstream_selection | platform_requirements@adr-input-v2 | benchmark-runner |
| phase5_repo_indexing | codebase_indexing<br>frontend_route_indexing | repository_snapshot@HEAD | index-codebase.sh |

## memd-native comparison

| System | Mode | hit@3 | MRR | avg search ms | p95 search ms | Fresh rank | Concurrency success | Concurrency ops/s | Seed ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| memd_chunk_baseline | memory.search over flattened chunk-native benchmark artifacts | 0.00 | 0.00 | 145.4 | 256.9 | 1 | 100.00% | 5.20 | 65574.3 |
| memd_task_memory | task.* lifecycle writes plus task.search over exact-filtered task artifacts | 0.96 | 0.82 | 2.9 | 5.7 | 1 | 100.00% | 1.16 | 106875.9 |

## Why memd-native Modes Differ

- `memd_chunk_baseline` flattened the corpus into `56` generic chunks and searched them with `memory.search`. On the hardened sibling-task corpus, that representation did not recover the correct task in the top 3 results.
- `memd_task_memory` wrote `144` lifecycle projections and searched them with `task.search`, exact artifact filters, and candidate reranking. That increased seed time from `65.6s` to `106.9s`, but improved retrieval from `hit@3=0.00` / `MRR=0.00` to `hit@3=0.96` / `MRR=0.82` and reduced average search latency from `145.4ms` to `2.9ms`.

## Live external comparison

| System | Mode | hit@3 | MRR | avg search ms | p95 search ms | Fresh rank | Concurrency success | Concurrency ops/s | Seed ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| gpt54_live_cli | gpt54 live CLI context search over the same seeded benchmark corpus | 0.91 | 0.83 | 477.7 | 503.7 | 1 | 100.00% | 2.12 | 40243.6 |
| gpt54_live_daemon | gpt54 live daemon context search over the same seeded benchmark corpus | 0.91 | 0.83 | 146.9 | 151.8 | 1 | 100.00% | 2.52 | 35768.0 |
| gpt54_live_tantivy | gpt54 live warm Tantivy service search over the same seeded benchmark corpus | 0.91 | 0.83 | 21.2 | 31.6 | 1 | 100.00% | 2.46 | 28918.1 |
| claude_live | claude ark live CLI artifact search on the same benchmark corpus | 0.00 | 0.00 | 397.6 | 426.9 | 1 | 100.00% | 3.16 | 10335.0 |
| geminipro_live | geminipro live CLI search on the same benchmark corpus | 0.96 | 0.84 | 6192.3 | 6293.0 | 1 | 100.00% | 0.76 | 121102.0 |
| geminiultra_live | geminiultra live CLI search on the same benchmark corpus | 1.00 | 0.93 | 6205.7 | 6316.1 | 1 | 100.00% | 0.76 | 132541.4 |

## GenesisM unified benchmark reference

These numbers are imported from GenesisM's unified benchmark and are not directly comparable to the memd-native Phase 5 task benchmark because the old GenesisM `memd` measurement predated memd's task-lifecycle tools.

| External system | Search backend | lifecycle ms | hit@3 | MRR | avg search ms | fresh rank | concurrency success | concurrency ops/s |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| gpt54 | Tantivy daemon by default when configured, fallback SQLite/ DuckDB | 1943.1 | 1.00 | 1.00 | 42.2 | 1 | 100.00% | 3.02 |
| memd | Warm MCP process with hybrid dense+sparse+rereanking retrieval | 702.2 | 1.00 | 0.97 | 18.5 | 1 | 100.00% | 8.51 |

## Reproducibility

- Primary entrypoint: `python3 evals/bench/tools/task_memory_benchmark.py --memd-path target/debug/memd`
- Corpus source-of-truth: `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
- External checkouts expected nearby: `gpt54`, `claude`, `geminipro`, and `geminiultra` under a GenesisM workspace, or passed explicitly with the `--*-root` flags.
- The runner prints stage progress so long external sections are visible during execution.
- External CLI timeouts: `ark=30s`, `geminipro=180s`, `geminiultra=180s`.

## Interpretation

- `memd_chunk_baseline` shows how well plain chunk retrieval works when the same knowledge is flattened into generic memory chunks.
- `memd_task_memory` shows how much the structured task lifecycle plus exact filters improve retrieval of failures, parameters, evidence, and why-chosen rationale.
- The GenesisM reference section is included to preserve continuity with the prior cross-system benchmark work.
