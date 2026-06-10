# Benchmarking

`memd` ships four benchmark families. They exercise different parts of the
system, so their numbers should not be mixed without the workload context.
The cross-system [LoCoMo retrieval](#cross-system-retrieval-locomo) result
is the public headline; the others are internal-corpus or scoped checks.

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
| `agent-context` prefetch | bounded context before the agent starts | retrieved 10/10 expected priors in the full-suite CLI-prefetch run |
| CLI search | retrieval by shell command during the solve | strongest token condition in the interface comparison, but slower for agents |
| Warm and batch execution | reduce local retrieval overhead | preserve retrieval quality while avoiding repeated startup costs |

## Raw artifacts

- [Task-memory report](scientific-task-memory/benchmark-results/README.md)
- [Bright-Pro adapter](https://github.com/fmschulz/memd/tree/main/evals/bench/bright-pro-memd)
- [Multi-turn token benchmark](https://github.com/fmschulz/memd/tree/main/evals/bench/memd-multiturn-token-savings)

## Cross-system retrieval (LoCoMo)

Direct retrieval benchmark on upstream
[`locomo10.json`](https://github.com/snap-stanford/locomo): each system is
seeded with the same conversation turns and scored against LoCoMo
evidence IDs (MRR@10 over categories 1–4: 10 conversations, 5,882 turns,
1,536 queries).

| System | MRR@10 | Hit@1 | Hit@3 | Hit@10 | Avg search | Seed |
|---|---:|---:|---:|---:|---:|---:|
| **`memd` v0.50.0** | **0.420** | **0.322** | **0.490** | **0.621** | **26.7 ms** | 108 s |
| `superlocalmemory` v3.4.46 (lexical) | 0.369 | 0.245 | 0.469 | 0.599 | 804.5 ms | 1.8 s |
| `mem0` v2.0.2 (LLM-extracted) | 0.354 | 0.255 | 0.412 | 0.591 | 40.9 ms | 13,424 s |

`memd` leads on MRR@10 (+14% vs SuperLocalMemory, +19% vs Mem0), Hit@10, and
search latency. Seeding cost trades off against
quality — SuperLocalMemory has the cheapest seed (no embeddings in this
configuration), Mem0 the most expensive (LLM extraction).

### Per-category

`memd` wins all four LoCoMo categories.

| Category | Description | `memd` | `mem0` | `slm` |
|---|---|---:|---:|---:|
| 1 | multi-hop | **0.359** | 0.292 | 0.259 |
| 2 | specific facts | **0.513** | 0.390 | 0.433 |
| 3 | open-domain | **0.279** | 0.255 | 0.227 |
| 4 | long-form | **0.421** | 0.372 | 0.397 |

### Three design philosophies

- **`memd`** — chunk-native dense + sparse hybrid retrieval. No LLM
  extraction during seed, no LLM rerank during search.
- **`mem0`** — LLM-extracts memory units from raw turns (here using a
  local vLLM `gemma4-31b` endpoint), then vector-searches over the
  extracted memories.
- **`superlocalmemory`** — atomic-fact graph with Fisher-Rao retrieval.
  Reported here in the lexical-only fallback because the published
  Mode A 74.8% MRR@10 number was not reproducible in our workspace; SLM's
  subprocess embedding-worker singleton deadlocked under the LoCoMo
  workload. The lexical result (0.369) does match prior independent
  fallback runs in this workspace (0.368), so the configuration itself
  is reproducible.

### Reproducibility

The full benchmark harness lives in the sibling `memory-benchmark`
workspace (separate repo). A self-contained in-repo
`evals/benchmarks/locomo/` is in progress and will land here once the
SLM embedded mode is debugged upstream and the OpenAI / Anthropic /
self-hosted vLLM LLM choice is documented for `mem0` reproducers.

Same-LLM caveat: `mem0` numbers above use a self-hosted vLLM
`gemma4-31b` endpoint, not the GPT-4-class model the upstream Mem0
README benchmarks against. Numbers are directly comparable across
the three systems in this table but not directly comparable to the
upstream Mem0 leaderboard.
