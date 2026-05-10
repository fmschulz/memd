# Phase 5 Task Memory Benchmark

- Corpus: `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
- Version: `2026-03-21.v2`
- Generated at: `2026-05-10T05:01:41Z`
- Embedding model: `all-minilm`
- Search variant: `hybrid-feature`
- memd lanes: `cli_cold, cli_warm, cli_batch`

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
| memd_cli_cold_chunk_baseline | cli_cold: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 4211.6 | 4988.0 | 3 | 100.00% | 0.34 | 21138.1 |
| memd_cli_cold_task_memory | cli_cold: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 524.2 | 378.4 | 1 | 100.00% | 0.34 | 179484.3 |
| memd_cli_warm_chunk_baseline | cli_warm: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 39.0 | 55.3 | 3 | 100.00% | 27.89 | 4813.9 |
| memd_cli_warm_task_memory | cli_warm: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 9.7 | 11.4 | 1 | 100.00% | 8.03 | 8973.8 |
| memd_cli_batch_chunk_baseline | cli_batch: memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 22.4 | 30.6 | 3 | 100.00% | 0.34 | 6898.0 |
| memd_cli_batch_task_memory | cli_batch: task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.87 | 0.6 | 0.9 | 1 | 100.00% | 0.35 | 8763.5 |

## Why memd-native Modes Differ

- `cli_warm` chunk baseline flattened the corpus into `56` generic chunks and searched them with `memory.search`. In this task-level run it reached `hit@3=1.00` / `MRR=0.98` with average search latency `39.0ms`.
- `cli_warm` task memory wrote `112` lifecycle projections and searched them with `task.search`, exact artifact filters, and candidate reranking. In this run it reached `hit@3=1.00` / `MRR=0.87` with average search latency `9.7ms`; seed time changed from `4.8s` to `9.0s` (`+4.2s`) and average search latency changed by `-29.3ms` versus the chunk baseline.

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
