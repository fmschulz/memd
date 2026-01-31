# Phase 5: Hot Tier + Cache - Context

**Gathered:** 2026-01-31
**Status:** Ready for planning

<domain>
## Phase Boundary

Performance optimization layer that keeps frequently accessed memories immediately available with low latency. Adds a hot tier (separate HNSW index for frequently accessed chunks) and semantic query cache on top of existing warm (HNSW) and cold (persistent store) tiers. This phase does NOT change underlying retrieval algorithms or add new query types - it strictly focuses on making existing retrieval faster through caching and tiering.

</domain>

<decisions>
## Implementation Decisions

### Hot tier structure
- **Separate index**: Independent HNSW with only hot chunks (not a logical subset of warm)
- **Target size**: 500-2000 chunks (medium coverage)
- **HNSW config**: Optimized for hot tier - higher M and efSearch than warm tier for maximum speed on small index
- **Rebuild strategy**: Incremental eviction (remove chunks one-by-one, no periodic rebuild)

### Promotion criteria
- **Primary signal**: Multi-signal scoring combining frequency + recency + project activity + manual hints
- **Active project boost**: Chunks with project tag matching current active project get promoted faster
- **Manual hints**: Support explicit metadata flag or API to mark chunks as hot (useful for core APIs, docs)
- **Promotion threshold**: Medium threshold (5-7 retrievals or high score) - balanced precision/recall

### Semantic cache design
- **Similarity detection**: Embedding similarity (compute query embedding, check cosine similarity to cached queries)
- **Cache hit threshold**: Moderate (0.85-0.95 similarity) - semantically similar queries hit cache
- **What to cache**: Just embeddings (skip re-embedding query, still search indexes for fresh results)
- **Invalidation**: Hybrid TTL + version (short TTL 1-2 min plus version checks when chunks change)

### Demotion and eviction
- **Eviction strategy**: LFU (Least Frequently Used) - evict chunks with lowest access count
- **Timing**: On every query (check and evict if needed per query for continuous cleanup)
- **Update handling**: Update in place - for updates (not deletes), refresh the hot tier entry with re-embedding
- **Minimum residency**: Yes - chunks stay in hot for at least 5-10 minutes before eligible for demotion (prevents thrashing)

### Claude's Discretion
- Exact HNSW parameters for hot tier (M, efConstruction, efSearch values)
- Multi-signal scoring formula and weights (frequency vs recency vs project activity)
- Precise similarity threshold within 0.85-0.95 range for cache hits
- TTL value within 1-2 minute range for cache invalidation
- Minimum residency time within 5-10 minute range
- Implementation of access tracking data structures
- Metrics and observability for promotion/demotion decisions

</decisions>

<specifics>
## Specific Ideas

- Hot tier should feel "instantly responsive" compared to warm tier - target sub-10ms for hot queries vs 50-100ms for warm
- Debug flags should show cache hit status and promotion/demotion reasoning (as mentioned in success criteria)
- Success criteria requires: "Debug flags show cache hit status and promotion/demotion reasoning"
- Performance target from requirements: "Hot tier queries return results significantly faster than warm tier queries"

</specifics>

<deferred>
## Deferred Ideas

None - discussion stayed within phase scope

</deferred>

---

*Phase: 05-hot-tier-+-cache*
*Context gathered: 2026-01-31*
