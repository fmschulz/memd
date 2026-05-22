# Benchmarking

`memd` ships three checked-in benchmark families. They exercise different
parts of the system, so their numbers should not be mixed without the
workload context.

## Quick runs

```bash
# Task-memory benchmark (internal corpus)
./evals/bench/scripts/run_task_memory_benchmark.sh

# Offline retrieval benchmark (BEIR fiqa + scidocs)
./evals/bench/scripts/run_offline_retrieval_benchmark.sh
```

## Task memory (internal corpus)

Recommended local execution modes:

| Lane | Retrieval setup | Hit@3 | MRR | Avg search latency |
| --- | --- | ---: | ---: | ---: |
| `cli_warm` | private warm worker | 1.00 | 0.87 | 9.7 ms |
| `cli_batch` | streaming JSONL in one loaded process | 1.00 | 0.87 | 0.6 ms |

The same report includes a flattened chunk baseline with `hit@3 = 1.00`,
`MRR = 0.98`. The structured mode writes more retrieval projections, but
warm and batch execution keep interactive retrieval latency low. The raw
benchmark artifact also retains a startup-overhead diagnostic lane for
reproducibility; the public summary focuses on the two modes agents
should normally use.

Full report: [Task-memory benchmark report](scientific-task-memory/benchmark-results/README.md).

## Bright-Pro scoped adapter (biology q5/d141)

| Method | alpha-nDCG@25 | Recall@25 | Search time |
| --- | ---: | ---: | ---: |
| BM25 subset | 0.77393 | 0.81111 | not separately timed |
| SuperLocalMemory Mode A | 0.78406 | 0.85333 | 31.713 s total, 6.343 s/query |
| `memd` first search | 0.87035 | 0.98000 | 42.521 s total, 8.504 s/query |
| `memd` repeat search | 0.87035 | 0.98000 | 33.260 s total, 6.652 s/query |
| `memd` + MemReranker-4B | 0.90409 | 1.00000 | +92.987 s rerank |

The Bright-Pro result is a scoped gold-plus-decoy adapter check, not a
full-corpus benchmark. It uses 5 biology queries, 41 gold documents, and 100
decoys. Repeat search is the fairer retrieval-speed number because it
excludes fresh indexing and reuses the already-built store.

## Multi-turn agent benchmark

| Interface | Main purpose | Result summary |
| --- | --- | --- |
| `agent-context` prefetch | bounded context before the agent starts | retrieved 10/10 expected priors in the full suite5 CLI-prefetch run |
| CLI search | retrieval by shell command during the solve | strongest token condition in the interface comparison, but slower for agents |
| Warm and batch execution | reduce local retrieval overhead | preserve retrieval quality while avoiding repeated startup costs |

## Raw artifacts

- [Task-memory report](scientific-task-memory/benchmark-results/README.md)
- [Bright-Pro adapter](https://github.com/fmschulz/memd/tree/main/evals/bench/bright-pro-memd)
- [Multi-turn token benchmark](https://github.com/fmschulz/memd/tree/main/evals/bench/memd-multiturn-token-savings)

## Cross-system retrieval (LoCoMo)

A cross-system LoCoMo retrieval benchmark is in preparation. It will compare
`memd` to popular open-source memory systems (Mem0, Cognee, Letta) on
upstream `locomo10.json` using evidence-ID scoring. Results will land here
once the protocol is finalized.
