# Comparison with other memory tools

`memd` is one of several systems aimed at giving agents memory that survives
the context window. The closest neighbours are `mem0`, `superlocalmemory`,
`Letta` (formerly `MemGPT`), `Zep`, the newer research-grade systems
`A-Mem`, `Cognee`, and `Memary`, and raw vector databases used as memory
backends (`Chroma`, `FAISS`, `Qdrant`, others).

This page is design-first: how each system represents memory, retrieves it,
runs as a process, persists state, and assigns trust. The cross-system
retrieval numbers live in [Benchmarking](benchmarking.md).

## Headline benchmark

LoCoMo retrieval, same upstream `locomo10.json`, 10 conversations, 5,882
turns, 1,536 queries, MRR@10 over categories 1–4:

| System | MRR@10 | Hit@10 | Avg search | Seed |
|---|---:|---:|---:|---:|
| **`memd` v0.50.0** | **0.420** | **0.621** | **26.7 ms** | 108 s |
| `superlocalmemory` v3.4.46 (lexical) | 0.369 | 0.599 | 804.5 ms | 1.8 s |
| `mem0` v2.0.2 (LLM-extracted) | 0.354 | 0.591 | 40.9 ms | 13,424 s |

`memd` wins quality on every metric and is the fastest at search. Seed cost
trades off against retrieval quality. Full per-category numbers,
configuration, and reproducibility caveats: [Benchmarking](benchmarking.md).

## Design comparison

| System | Memory unit | Retrieval | Process model | Persistence | Trust model |
|---|---|---|---|---|---|
| **`memd`** | chunk-native + `task.*`/`artifact.*` records | hybrid dense (HNSW) + sparse (BM25); optional rerankers | local CLI binary; warm worker + JSONL batch | SQLite (WAL) + segments + WAL + tantivy + HNSW | candidate · canonical · digest · verified; distinct-writer rule |
| **`mem0`** | LLM-extracted memory units from raw turns | dense vector search over extracted memories | server + SDK; LLM-dependent at write time | vector store + metadata | implicit; trusts extractor and ranker |
| **`superlocalmemory`** | atomic-fact graph; Fisher–Rao retrieval | graph + embedding; lexical-only fallback under load | local Python service; embedding-worker subprocess | graph store | implicit; provenance via fact IDs |
| **`Letta` / `MemGPT`** | tiered context (core, archival, recall) managed by an LLM controller | dense vector search over archival; function-calling for promotion | server + SDK; LLM controller in the loop | Postgres or SQLite + vector backend | implicit; controller decides what to keep |
| **`Zep`** | session messages + extracted facts + temporal knowledge graph | dense + graph traversal | server (Go) + SDK; LLM extraction pipeline | Postgres + vector backend + graph | implicit; trusts pipeline-extracted facts |
| **`A-Mem`** | atomic notes with LLM-inferred links (Zettelkasten-style) | dense vector search; link traversal | library; LLM in the write path | vector store + link graph | implicit; trusts the linker |
| **`Cognee`** | entity-relation graph extracted by LLM | graph + vector hybrid | library or service; LLM at write | graph DB + vector backend | implicit; trusts extractor |
| **`Memary`** | knowledge-graph memory with recency/importance scoring | graph + dense | library; LLM at write | graph DB + vector backend | implicit |
| **Raw vector DBs** (Chroma, FAISS, Qdrant) | whatever the caller chunks and writes | dense vector search; sparse is BYO | server or in-process library | vector index + side metadata | none; caller's responsibility |

The columns above are deliberately narrow. They describe how each system
shapes the work an agent has to do *around* the store, not whether one
system is universally better.

## What `memd` does differently

### No LLM in the write path

`mem0`, `Letta`, `Zep`, `A-Mem`, `Cognee`, and `Memary` all run an LLM at
ingest time to extract or curate memory units, link entities, or annotate
facts. This has three consequences:

- **Seed cost is large.** On LoCoMo, `mem0` seeded in 13,424 s against
  `memd`'s 108 s, a 124× difference. The extra cost is the extractor running
  over every turn.
- **Write quality is coupled to extractor quality.** Swapping the extractor
  model — for cost, license, or capability reasons — invalidates the prior
  memory, because the new model would have produced different units. The
  `mem0` LoCoMo run uses a self-hosted `gemma4-31b` endpoint; published
  upstream numbers use a GPT-4-class model.
- **Extraction can drop information.** A turn that an extractor finds
  unimportant is gone from memory; only re-ingesting raw turns can recover
  it.

`memd` writes chunks and structured records directly. If extraction is
useful, the agent does it inline and stores both the raw and the derived
records; nothing in the store assumes an LLM was involved.

### Hybrid retrieval, not vector-only

Most of the systems above default to dense vector search. Dense is good at
paraphrase but bad at the lookup shapes agents often generate: function names,
file paths, error strings, ticket IDs, commit hashes,
parameter values. `memd` runs dense (HNSW) and sparse (BM25 over tantivy)
in parallel and fuses the results. The cross-system LoCoMo gap (+14% MRR@10
over the lexical-only `superlocalmemory`, +19% over the dense-only `mem0`)
is the headline data point.

### Local CLI, not server

`mem0`, `Letta`, `Zep`, and the production vector DBs are server-shaped:
install a service, point clients at it, manage credentials. `memd` is a
single Rust binary plus an optional warm worker for the same process tree.
One trusted machine, one shared data directory per trust domain, multiple
agent processes. No network surface by default.

This buys two things. First, install friction is small enough that the same
binary runs in agent shells, scripts, and CI without operational ceremony.
Second, the data path stays on disk; there is no LLM API key, no embedding
service round-trip, no service-level outage to debug.

The cost: multi-user deployments need an external trust boundary.
`tenant_id` is logical partitioning, not authorization. For shared
deployments, put a reverse proxy with real authentication in front, or
run separate data directories per trust domain.

### Trust is a first-class object

Most systems return what they retrieve. The caller decides whether to
trust it. `memd` makes the contract explicit:

- `semantic_candidate` — retrieved by similarity, no canonical artifact
  grounding.
- `canonical_record` — linked to a non-digest task or artifact record.
- `compiled_digest_hint` — linked to a digest artifact compiled from
  canonical records.
- `verified_record` — linked to an independent reviewer's verification
  with `supports_claim = true` from a distinct `agent_id`.

The compiled wiki and digest libraries (project briefs, failure libraries,
decision libraries, evidence libraries) are useful precisely because they
cannot become ungrounded authority. They display their tier; downstream
consumers can decide whether a hint is enough.

This is closest in spirit to the artifact discipline scientific-claim
graphs use: a claim is meaningful only with context, evidence support is a
separate object from the claim, and verification has to be represented
explicitly.

## When `memd` is the right tool

- One trusted machine, one or more agents, one shared memory.
- Task work that needs to survive sessions: goals, runs, failures,
  decisions, evidence, reviews.
- Retrieval that must mix identifier-shaped lookups (paths, error strings,
  ticket IDs) with semantic recall.
- A trust contract: digests must point back to canonical records, and
  verification must come from a separate writer.

## When it is not

- Multi-user deployments with real authentication needs. Put auth at the
  perimeter; `tenant_id` is not an authorization boundary.
- Workloads dominated by very-large-document retrieval where a server-class
  vector DB plus a tuned reranker is the right tool.
- Experiments whose point is a learned memory policy — `memd` defines a
  substrate; the policy is a layer above it.

## See also

- [Architecture](architecture.md) for the layer-by-layer breakdown and the
  static figure.
- [Benchmarking](benchmarking.md) for cross-system retrieval, internal
  task-memory, and the Bright-Pro biology adapter.
- [Trust boundary](trust-boundary.md) for the candidate/canonical/digest/
  verified rules.
