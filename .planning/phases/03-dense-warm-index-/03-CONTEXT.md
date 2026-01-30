# Phase 3: Dense Warm Index - Context

**Gathered:** 2026-01-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Semantic search via dense vector embeddings. This phase adds vector-based similarity retrieval to memory.search, with HNSW index for efficient nearest-neighbor queries and embeddings generated via ONNX model.

Delivers: Agents can search by semantic similarity, ranked by cosine distance.
Out of scope: Filters, facets, structured queries, hybrid retrieval (later phases).

</domain>

<decisions>
## Implementation Decisions

### Embedding Model Selection
- **Model size:** Balanced (200-400MB) - good quality without excessive resources
- **Delivery:** Download on first run to ~/.cache/memd/ (smaller binary, offline after first use)
- **Test behavior:** Require real model in tests and eval harness - tests actual behavior, not mocks
- Use ONNX runtime for cross-platform inference

### Search Experience
- **Score visibility:** Yes, include cosine similarity scores (0-1) in results - agents can filter/weight
- **Score threshold:** Configurable min_score parameter in search request, default returns all top-k
- **Response format:** Structured with relevance context
  - Group results by project when multiple projects present
  - Include all metadata: chunk_id, text, score, type, project, timestamp, source_uri
  - Highlight/explain why chunk matched query (e.g., "similar code pattern", "related concept")

### Index Behavior
- **Build timing:** Claude's discretion - balance startup time vs first-query latency
- **Index updates:** Add to index immediately - new chunks searchable without rebuild (incremental HNSW)
- **Persistence:** Persist HNSW index to disk
  - Save on shutdown, load on startup to avoid rebuild cost
  - Store in tenants/{tenant}/warm_index/ alongside segments
  - Invalidate and rebuild if segment changes detected

### Quality Measurement
- **Test dataset:** Real code samples - curated actual code with known similar pairs
  - Include varied languages, idioms, refactorings
  - Document expected similar pairs for each query
  - Start small (~50-100 examples), expand over time
- **Key metrics:** Both Recall@k and MRR equally important
  - Recall@10: Ensure relevant results appear in top 10
  - MRR: Optimize ranking so best match appears early
- **Quality bar:** Good quality targets (not just baseline functional)
  - Target: Recall@10 > 0.8
  - Target: MRR > 0.6
  - Measured on eval dataset, reported in test output

### Claude's Discretion
- Exact embedding model choice within 200-400MB range (e.g., all-MiniLM, BGE-small, etc.)
- HNSW hyperparameters (M, efConstruction, efSearch)
- Index build timing strategy (startup vs lazy vs threshold-triggered)
- Memory budget allocation for HNSW
- Relevance explanation logic (how to describe why chunks matched)
- Error handling when model download fails
- Cache strategy for embeddings (if any)

</decisions>

<specifics>
## Specific Ideas

- Structured responses should help agents understand WHY a chunk matched, not just return a ranked list
- Real code samples are essential - synthetic won't catch the nuances of code similarity
- Quality metrics should be visible in CI/test output so regressions are caught early

</specifics>

<deferred>
## Deferred Ideas

None - discussion stayed within phase scope

</deferred>

---

*Phase: 03-dense-warm-index*
*Context gathered: 2026-01-30*
