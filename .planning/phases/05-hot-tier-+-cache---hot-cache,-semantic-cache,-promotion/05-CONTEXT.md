# Phase 5: Hot Tier + Cache - Context

**Gathered:** 2026-01-31
**Status:** Ready for planning

<domain>
## Phase Boundary

Frequently accessed memories are served with low latency from hot tier and cache. This phase adds a performance optimization layer on top of the existing hybrid retrieval system (Phases 3-4), focusing on access patterns, promotion/demotion logic, and semantic caching.

New capabilities (structural indexes, query routing) belong in Phase 6.

</domain>

<decisions>
## Implementation Decisions

### Promotion criteria
- Multi-signal scoring combines frequency + recency + project context
- Balanced weights across all signals (no single dominant factor)
- Fully automatic promotion — system tracks signals and promotes transparently
- Promoted chunks get full copy into separate hot-tier HNSW index

### Cache hit behavior
- Semantic cache activates at moderate similarity threshold (cosine > 0.85)
- Cache stores query embeddings + results for similarity-based lookup
- Hybrid invalidation: TTL (30-60 min) + version tracking for immediate updates
- Cache entries invalidate when any chunk in result set changes

### Eviction strategy
- Combined triggers: capacity limits + score decay
- Hot tier capacity is percentage of total indexed chunks (scales with dataset)
- Evict chunks with lowest multi-signal promotion score
- Immediate eviction when chunks qualify (hot tier reflects current scores)

### Claude's Discretion
- Exact signal weights for multi-signal scoring
- Default hot tier capacity percentage
- Score decay rate and threshold values
- Cache similarity threshold fine-tuning
- TTL value within 30-60 min range

</decisions>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 05-hot-tier-+-cache*
*Context gathered: 2026-01-31*
