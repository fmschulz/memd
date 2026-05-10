# Phase 5 Task Memory Benchmark

- Corpus: `docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json`
- Version: `2026-03-21.v2`
- Generated at: `2026-05-10T23:44:02Z`
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
| memd_chunk_baseline | memory.search over flattened chunk-native benchmark artifacts | 1.00 | 0.98 | 301.6 | 409.1 | 3 | 100.00% | 2.98 | 100159.5 |
| memd_task_memory | task.* lifecycle writes plus task.search over exact-filtered task artifacts | 1.00 | 0.84 | 5.1 | 11.1 | 1 | 100.00% | 1.41 | 176854.3 |

## Why memd-native Modes Differ

- `memd_chunk_baseline` flattened the corpus into `56` generic chunks and searched them with `memory.search`. On the hardened sibling-task corpus, that representation produced `hit@3=1.00` and `MRR=0.98`.
- `memd_task_memory` wrote `112` lifecycle projections and searched them with `task.search`, exact artifact filters, and candidate reranking. That increased seed time from `100.2s` to `176.9s`, changed retrieval from `hit@3=1.00` / `MRR=0.98` to `hit@3=1.00` / `MRR=0.84`, and changed average search latency from `301.6ms` to `5.1ms`.

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
