# Phase 5 task memory benchmark

- Corpus: `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
- Version: `2026-03-21.v2`
- Generated at: `2026-05-12T01:15:51Z`
- Embedding model: `all-minilm`
- Search variant: `hybrid-feature`
- memd lanes: `cli_cold, cli_warm, cli_batch`

## Corpus design

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
| memd_cli_cold_chunk_baseline | cli_cold: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.91 | 3465.5 | 4519.8 | 3 | 100.00% | 0.32 | 21744.2 |
| memd_cli_cold_task_memory | cli_cold: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 318.2 | 330.0 | 1 | 100.00% | 0.33 | 166098.3 |
| memd_cli_warm_chunk_baseline | cli_warm: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 28.9 | 30.6 | 3 | 100.00% | 14.45 | 4571.7 |
| memd_cli_warm_task_memory | cli_warm: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 9.6 | 10.1 | 1 | 100.00% | 9.26 | 8447.8 |
| memd_cli_batch_chunk_baseline | cli_batch: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 25.0 | 19.8 | 3 | 100.00% | 0.22 | 5221.7 |
| memd_cli_batch_task_memory | cli_batch: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 0.6 | 1.1 | 1 | 100.00% | 0.24 | 8995.0 |

## Why memd-native modes differ

- `cli_warm` chunk baseline flattened the corpus into `56` generic chunks and searched them with `memory.search`. In this task-level run it reached `hit@3=1.00` / `MRR=0.98` with average search latency `28.9ms`.
- `cli_warm` task memory wrote `112` lifecycle projections and searched them with `task.search`, exact artifact filters, and candidate reranking. In this run it reached `hit@3=1.00` / `MRR=0.87` with average search latency `9.6ms`; seed time changed from `4.6s` to `8.4s` (`+3.9s`) and average search latency changed by `-19.2ms` versus the chunk baseline.

## Live external comparison

| System | Mode | hit@3 | MRR | avg search ms | p95 search ms | Fresh rank | Concurrency success | Concurrency ops/s | Seed ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| gpt54_live_cli | gpt54 live CLI context search over the same seeded benchmark corpus | 0.91 | 0.83 | 426.1 | 446.5 | 1 | 100.00% | 2.21 | 45711.3 |
| gpt54_live_daemon | gpt54 live daemon context search over the same seeded benchmark corpus | 0.91 | 0.83 | 142.0 | 155.3 | 1 | 100.00% | 2.01 | 27055.0 |
| gpt54_live_tantivy | gpt54 live warm Tantivy service search over the same seeded benchmark corpus | 0.91 | 0.83 | 19.4 | 28.5 | 1 | 100.00% | 2.09 | 27330.6 |
| claude_live | claude ark live CLI artifact search on the same benchmark corpus | 0.00 | 0.00 | 352.6 | 366.6 | 1 | 100.00% | 2.34 | 10027.1 |
| geminipro_live | geminipro live CLI search on the same benchmark corpus | 0.96 | 0.83 | 6613.8 | 7138.5 | 1 | 100.00% | 0.23 | 137384.5 |
| geminiultra_live | geminiultra live CLI search on the same benchmark corpus | 1.00 | 0.93 | 6447.9 | 6575.2 | 1 | 100.00% | 0.23 | 137865.0 |

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

- Chunk-baseline rows show how well plain chunk retrieval works when the same knowledge is flattened into generic memory chunks.
- Task-memory rows show the latency and filtering behavior of the structured task lifecycle path. In the current run they matched the chunk baseline on hit@3, had lower MRR, and searched much faster because exact task filters narrowed the candidate set.
- The GenesisM reference section is included to preserve continuity with the prior cross-system benchmark work.
