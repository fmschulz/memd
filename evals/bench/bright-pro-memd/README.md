# Bright-Pro memd Adapter

This adapter runs a scoped static-retrieval benchmark for `memd` against the
Bright-Pro framework from:

- Paper: <https://arxiv.org/abs/2605.04018>
- Code: <https://github.com/yale-nlp/Bright-Pro>
- Dataset: <https://huggingface.co/datasets/yale-nlp/Bright-Pro>

The full Bright-Pro static benchmark embeds tens of thousands of documents per
task with large retriever models. The full agentic protocol also requires an
LLM search agent and LLM-as-judge credentials. This adapter is intentionally a
small, reproducible bridge: it loads Bright-Pro data, indexes a selected static
subset into `memd`, emits Bright-Pro-compatible `score.json`, and computes the
same standard retrieval metrics plus Bright-Pro alpha-nDCG.

## Setup

Clone the upstream framework outside this repository:

```bash
git clone --depth 1 https://github.com/yale-nlp/Bright-Pro.git /tmp/bright-pro
python3 -m pip install --user pytrec-eval-terrier
```

Run the current scoped biology smoke benchmark:

```bash
python3 evals/bench/bright-pro-memd/run_memd_static.py \
  --bright-pro-root /tmp/bright-pro \
  --task biology \
  --max-queries 5 \
  --decoy-docs 100 \
  --top-k 50 \
  --memd-bin target/release/memd
```

Outputs are written under `evals/bench/bright-pro-memd/results/`. Full per-run
payloads are local artifacts and are ignored by default; only the compact
comparison summary is intended for normal commits.

## Current Result

The current smoke run uses `biology`, 5 queries, 41 gold documents, and 100
decoys. It indexed 141 documents through `memd batch --jsonl`, searched the 5
queries through `memd batch --jsonl`, wrote Bright-Pro-compatible `score.json`,
and evaluated the result with Bright-Pro metric code.
The adapter over-fetches raw `memd` results, keeps only corpus chunks tagged
with `doc_id:*`, and then truncates to the requested corpus top-k before
scoring. This run used `top_k = 50` and `memd_k = 100`.

Local result directory:

```text
evals/bench/bright-pro-memd/results/biology_memd_q5_d141/
```

Summary:

| Metric | Value |
|---|---:|
| NDCG@25 | 0.92914 |
| Recall@25 | 0.98000 |
| Recall@50 | 1.00000 |
| MAP@25 | 0.86723 |
| MRR | 0.90000 |
| alpha-nDCG@25 | 0.87035 |
| Add time | 107.307 s |
| First search time | 42.521 s total, 8.504 s/query |
| Repeat search time | 33.260 s total, 6.652 s/query |

A previous smaller sanity run used 2 queries and 63 documents, with
alpha-nDCG@25 0.87926, NDCG@25 1.00000, and Recall@25 1.00000.

This result is intentionally a scoped integration benchmark, not the official
full Bright-Pro biology result. A larger 541-document attempt was stopped after
it exceeded the smoke-test budget, which indicates that full-corpus evaluation
should be run as a longer benchmark job or optimized with a reusable indexed
store.

## Same-Subset Comparison

To make the smoke result easier to interpret, the 5-query subset now includes
side-by-side runs over the exact same 141 selected documents. These are not the
paper's full-corpus results, but they are apples-to-apples for this adapter
subset.

| Method | alpha-nDCG@25 | NDCG@25 | Recall@25 | Recall@50 | MAP@25 | MRR |
|---|---:|---:|---:|---:|---:|---:|
| BM25 subset check | 0.77393 | 0.80910 | 0.81111 | 0.87333 | 0.68419 | 1.00000 |
| SuperLocalMemory Mode A | 0.78406 | 0.83681 | 0.85333 | 0.89556 | 0.70460 | 1.00000 |
| `memd` CLI adapter | 0.87035 | 0.92914 | 0.98000 | 1.00000 | 0.86723 | 0.90000 |
| BM25 + MemReranker-4B | 0.85625 | 0.85893 | 0.87333 | 0.87333 | 0.73770 | 1.00000 |
| SuperLocalMemory + MemReranker-4B | 0.87282 | 0.88105 | 0.89556 | 0.89556 | 0.77252 | 1.00000 |
| `memd` + MemReranker-4B | 0.90409 | 0.95428 | 1.00000 | 1.00000 | 0.87787 | 1.00000 |

Committed result summary JSON:

```text
evals/bench/bright-pro-memd/results/biology_q5_d141_comparison.json
```

On this smoke subset, `memd` improves alpha-nDCG@25 by 0.09642 absolute over
the same-subset BM25 check and by 0.08629 over the SuperLocalMemory lane. Adding
MemReranker-4B improves all three candidate sets, but the `memd` candidate set
still gives the best reranked result.

Timing:

| Method | Index/add time | Candidate search time | Model load time | Rerank time | End-to-end measured time |
|---|---:|---:|---:|---:|---:|
| BM25 subset check | n/a | not separately timed | n/a | n/a | not separately timed |
| SuperLocalMemory Mode A | 70.048 s | 31.713 s total, 6.343 s/query | n/a | n/a | 101.761 s |
| `memd` CLI adapter | 107.307 s | 42.521 s total, 8.504 s/query; repeat search 33.260 s total, 6.652 s/query | n/a | n/a | 149.828 s first search; 140.567 s repeat search |
| BM25 + MemReranker-4B | n/a | not separately timed | 11.263 s | 136.747 s total, 27.349 s/query | 148.010 s plus candidate generation |
| SuperLocalMemory + MemReranker-4B | source lane above | source lane above | 11.200 s | 126.060 s total, 25.212 s/query | 168.973 s after SLM indexing, 239.021 s including SLM indexing |
| `memd` + MemReranker-4B | source lane above | source lane above | 11.936 s | 92.987 s total, 18.597 s/query | 147.444 s after memd indexing, 254.751 s including memd indexing |

The MemReranker rows report reranking over already generated candidate scores.
For source-aware totals, the table adds the corresponding source search time
and the measured model load time. These timings are single-run wall-clock
measurements on this workstation, not confidence intervals.

The `memd` search lane now filters `memory.search` to document chunks, sets
`oversample_factor = 1`, and runs benchmarked commands with `RUST_LOG=error`.
The first search timing includes opening the persistent store and loading the
warm index; the repeat-search timing reruns the same JSONL query batch against
the already-built store. The repeat-search number is the fairer comparison to
the SuperLocalMemory search number because both exclude fresh indexing.

Local token-volume estimates:

| Method | Returned/scored docs | Query tokens once | Top-50 doc tokens | Cross-encoder pair tokens |
|---|---:|---:|---:|---:|
| BM25 subset check | 250 | 397 | 258,762 | n/a |
| SuperLocalMemory Mode A | 250 | 397 | 239,509 | n/a |
| `memd` CLI adapter | 195 | 397 | 179,336 | n/a |
| BM25 + MemReranker-4B | 250 | 397 | 258,762 | 278,612 |
| SuperLocalMemory + MemReranker-4B | 250 | 397 | 239,509 | 259,359 |
| `memd` + MemReranker-4B | 195 | 397 | 179,336 | 195,317 |

These are regex token estimates over the local query and document text in the
adapter subset. They are included to make context and reranker input volume
auditable, but they are not provider billing tokens. The MemReranker pair-token
column counts each query/document pair presented to the cross-encoder; repeated
documents across queries are intentionally counted again.

The SuperLocalMemory run uses the published `superlocalmemory==3.4.41`
package, Mode A, a benchmark-local data directory, direct document facts, and
the package's default `nomic-ai/nomic-embed-text-v1.5` embedding model loaded
in-process. Its CLI/worker embedding path hit a stale-PID/cache setup issue in
this environment, so the adapter avoids the worker while preserving the same
retrieval engine and model. MemReranker-4B is not a memory store; it is reported
only as a reranker over already-retrieved candidates.

## Full Bright-Pro Context

The Bright-Pro paper reports full static retrieval results as alpha-nDCG@25
multiplied by 100 across all seven tasks and full task corpora. Those numbers
are not directly comparable to the smoke subset above because the smoke subset
uses only 5 biology queries and 141 selected documents. They do provide the
right context for where specialized reasoning retrievers sit on the full
benchmark:

| Full Bright-Pro method | Biology | Overall |
|---|---:|---:|
| BGE-Reasoner-8B | 73.5 | 68.0 |
| DIVER-4B-1020 | 72.8 | 63.7 |
| DIVER-4B | 67.3 | 59.9 |
| RTriever-4B | 63.1 | 55.3 |
| INF-Retriever-Pro | 62.6 | 53.8 |
| Qwen3-8B | 52.7 | 49.5 |
| OpenAI-Embed-3L | 53.5 | 45.8 |
| BM25 | 41.9 | 40.3 |

`memd` is therefore best described here as a strong local agent-memory
retrieval adapter that performs well on a scoped gold-plus-decoy integration
test. The specialized Bright-Pro retrievers remain the right comparison target
for a full-corpus run, especially because they are trained and evaluated as
large retrieval models rather than as a local memory executable.

## Interpretation

This is not the official full Bright-Pro number unless `--full-corpus` is used
for each task. The default run is a fast integration benchmark that proves the
adapter path and reports metrics on a gold-plus-decoy subset. Use it to compare
retrieval behavior, verify score format compatibility, and estimate whether a
full-corpus run is worth the indexing cost.
