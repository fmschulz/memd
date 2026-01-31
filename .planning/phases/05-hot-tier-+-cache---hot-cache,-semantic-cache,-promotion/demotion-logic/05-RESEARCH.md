# Phase 5: Hot Tier + Cache - Research

**Researched:** 2026-01-31
**Domain:** Caching, tiered storage, semantic similarity
**Confidence:** HIGH

## Summary

Phase 5 adds a performance optimization layer with two components: (1) a **hot tier** - a separate, smaller HNSW index optimized for frequently accessed chunks, and (2) a **semantic query cache** - caching query embeddings to avoid re-embedding similar queries. Both components build on the existing warm tier (HnswIndex) and dense searcher (DenseSearcher) infrastructure.

The codebase already has solid foundations:
- `HnswIndex` with persistence, versioning, and thread-safe access via `RwLock`
- `EmbeddingCache` with CRC validation and atomic writes
- `MetricsCollector` for latency tracking
- `HybridSearcher` coordinating dense+sparse search with timing breakdown

**Primary recommendation:** Use `moka` crate for both LFU access tracking (hot tier) and TTL-based semantic cache. Implement hot tier as a separate `HotHnswIndex` struct with higher M/efSearch parameters. Use cosine similarity threshold of 0.88-0.90 for semantic cache hits.

## Standard Stack

The established libraries/tools for this domain:

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| moka | 0.12.x | Async LFU/TTL cache | Best-in-class Rust cache with TinyLFU, async support, eviction listeners |
| hnsw_rs | (existing) | Hot HNSW index | Already used for warm tier, proven reliable |
| parking_lot | (existing) | Concurrent locks | Already used, proven in codebase |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| instant | 0.1.x | Cross-platform timing | Access timestamp tracking |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| moka | quick_cache | Slightly faster but fewer features (no TTL, no listeners) |
| moka | lfu crate | Pure LFU but no TTL, no async, fewer features |
| separate hot HNSW | filtered warm HNSW | Simpler but can't tune parameters independently |

**Installation:**
```bash
cargo add moka --features future
```

Note: `hnsw_rs`, `parking_lot`, `instant` already in dependencies.

## Architecture Patterns

### Recommended Project Structure
```
crates/memd/src/
├── cache/                    # NEW: caching layer
│   ├── mod.rs               # Cache module exports
│   ├── hot_tier.rs          # Hot HNSW index + access tracking
│   ├── semantic_cache.rs    # Query embedding cache
│   └── access_tracker.rs    # Frequency/recency tracking
├── index/
│   └── hnsw.rs              # Existing warm tier (no changes)
└── store/
    └── dense.rs             # Updated to use hot tier first
```

### Pattern 1: Tiered Search with Fallback
**What:** Search hot tier first, fall back to warm tier, combine results
**When to use:** Every search query
**Example:**
```rust
// Pseudocode pattern from Milvus tiered storage
pub async fn search(&self, query: &str, k: usize) -> Result<Vec<SearchResult>> {
    // 1. Check semantic cache for similar query
    if let Some(cached) = self.semantic_cache.get_similar(query_embedding, 0.88).await {
        return Ok(cached.clone());
    }

    // 2. Search hot tier first (smaller, faster)
    let hot_results = self.hot_tier.search(query_embedding, k).await?;

    // 3. If hot has enough results, use them
    if hot_results.len() >= k {
        self.record_access(&hot_results);
        return Ok(hot_results);
    }

    // 4. Fall back to warm tier for remaining
    let warm_results = self.warm_tier.search(query_embedding, k).await?;

    // 5. Merge and dedupe
    let merged = merge_results(hot_results, warm_results, k);
    self.record_access(&merged);

    Ok(merged)
}
```

### Pattern 2: Multi-Signal Promotion Scoring
**What:** Combine frequency, recency, and project affinity into single promotion score
**When to use:** Deciding whether to promote chunk to hot tier
**Example:**
```rust
// Based on CONTEXT.md decisions: multi-signal scoring
pub struct PromotionScore {
    frequency: u32,     // Access count
    recency_ms: i64,    // Time since last access
    project_match: bool, // Matches active project
    manual_hot: bool,   // Explicit hot flag
}

impl PromotionScore {
    pub fn compute(&self, config: &PromotionConfig) -> f32 {
        let freq_score = (self.frequency as f32).ln_1p(); // Log scale
        let recency_score = 1.0 / (1.0 + (self.recency_ms as f32 / config.recency_decay_ms));
        let project_boost = if self.project_match { config.project_boost } else { 0.0 };
        let manual_boost = if self.manual_hot { config.manual_boost } else { 0.0 };

        config.freq_weight * freq_score
            + config.recency_weight * recency_score
            + project_boost
            + manual_boost
    }
}
```

### Pattern 3: Semantic Cache with Similarity Lookup
**What:** Cache query embeddings with results, retrieve on similarity match
**When to use:** Repeated or similar queries within TTL window
**Example:**
```rust
// Based on Shuttle semantic caching tutorial
pub struct SemanticCache {
    // Query embedding -> (cached_results, timestamp, version)
    cache: moka::future::Cache<Vec<f32>, CachedResult>,
    similarity_threshold: f32, // 0.88-0.90 per CONTEXT.md
    ttl_secs: u64,            // 60-120 per CONTEXT.md
}

pub struct CachedResult {
    chunk_ids: Vec<ChunkId>,
    scores: Vec<f32>,
    memory_version: u64, // For invalidation
}

impl SemanticCache {
    pub async fn get_similar(&self, query_embedding: &[f32]) -> Option<CachedResult> {
        // Iterate cache entries, compute cosine similarity
        // Return if similarity >= threshold and version valid
        for (cached_embedding, result) in self.cache.iter() {
            let similarity = cosine_similarity(query_embedding, &cached_embedding);
            if similarity >= self.similarity_threshold {
                return Some(result.clone());
            }
        }
        None
    }
}
```

### Anti-Patterns to Avoid
- **Shared index with logical filtering:** Don't try to mark chunks as "hot" in warm index. Separate indexes allow independent tuning.
- **Synchronous cache on async path:** Always use `moka::future::Cache` in async contexts.
- **Global version watermark:** Don't use single version for all invalidation. Track per-tenant or per-project versions.
- **Over-eager promotion:** Don't promote on first access. Wait for threshold (5-7 accesses per CONTEXT.md).

## Don't Hand-Roll

Problems that look simple but have existing solutions:

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| LFU tracking | Custom HashMap + frequency counts | moka with TinyLFU | TinyLFU is battle-tested, handles edge cases (count-min sketch for low memory) |
| TTL expiration | Manual timestamp checks | moka time_to_live | Handles background cleanup, edge cases (clock skew) |
| Cosine similarity | Loop with dot product | Existing in hnsw_rs | Already normalized, tested |
| Atomic cache persistence | Manual file writes | Follow EmbeddingCache pattern | Already has CRC, temp file + rename pattern |
| Eviction callbacks | Poll-based checking | moka eviction_listener | Async notification, no polling overhead |

**Key insight:** Moka implements W-TinyLFU admission policy (same as Caffeine/Ristretto), which handles the common case where LFU alone promotes one-hit wonders. The admission filter rejects items that aren't popular enough.

## Common Pitfalls

### Pitfall 1: Hot Tier Thrashing
**What goes wrong:** Chunks constantly promoted/demoted, wasting CPU
**Why it happens:** No minimum residency time, low demotion threshold
**How to avoid:** Implement 5-10 minute minimum residency (per CONTEXT.md); only demote after N queries without access
**Warning signs:** High promotion/demotion rates in metrics, hot tier size oscillates

### Pitfall 2: Semantic Cache False Positives
**What goes wrong:** Wrong cached results returned for semantically different queries
**Why it happens:** Similarity threshold too low (< 0.85)
**How to avoid:** Start with 0.88-0.90 threshold; monitor cache hit quality
**Warning signs:** Users report irrelevant results, cache hit rate suspiciously high

### Pitfall 3: Version-Based Invalidation Missing Updates
**What goes wrong:** Stale results returned after chunk content updated
**Why it happens:** Only tracking adds/deletes, not updates
**How to avoid:** Increment version on any chunk modification; cache stores version watermark
**Warning signs:** Updated content not appearing in searches

### Pitfall 4: Hot Index Parameter Mismatch
**What goes wrong:** Hot tier slower than warm or crashes
**Why it happens:** Using same parameters for small index (500-2000) as large (100K)
**How to avoid:** Hot tier: M=24-32, efConstruction=200, efSearch=100. Higher M and efSearch for small, speed-optimized index.
**Warning signs:** Hot tier latency > warm tier latency

### Pitfall 5: Lock Contention on Access Tracking
**What goes wrong:** Access tracking becomes bottleneck under load
**Why it happens:** Locking on every access to update frequency
**How to avoid:** Use moka's internal batching; or batch updates with bounded channel
**Warning signs:** High contention in flamegraphs, latency spikes during high QPS

## Code Examples

Verified patterns from official sources and existing codebase:

### Moka Async Cache with TTL and Eviction Listener
```rust
// Source: https://docs.rs/moka/latest/moka/future/struct.CacheBuilder.html
use moka::future::Cache;
use std::time::Duration;

let cache: Cache<String, CachedResult> = Cache::builder()
    .max_capacity(10_000)
    .time_to_live(Duration::from_secs(120))  // 2 min TTL per CONTEXT.md
    .time_to_idle(Duration::from_secs(60))   // 1 min idle timeout
    .eviction_listener(|key, value, cause| {
        tracing::debug!(key = %key, cause = ?cause, "cache entry evicted");
    })
    .build();

// Async insert/get
cache.insert("query_hash".to_string(), result).await;
let hit = cache.get("query_hash").await;
```

### Cosine Similarity Computation (from existing codebase pattern)
```rust
// Matches existing HnswIndex approach - distance to similarity conversion
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    // Assuming normalized vectors (as done by embedder)
    dot.clamp(-1.0, 1.0)
}
```

### Hot HNSW Configuration
```rust
// Optimized parameters for hot tier (500-2000 chunks)
// Higher M and efSearch for maximum speed on small index
pub fn hot_tier_config(dimension: usize) -> HnswConfig {
    HnswConfig {
        max_connections: 24,      // Higher M for dense connectivity
        ef_construction: 200,    // Same as warm (good quality)
        ef_search: 100,          // Higher for better recall on small set
        max_elements: 2_000,     // Per CONTEXT.md target size
        dimension,
    }
}
```

### Access Tracking with moka (LFU-like behavior)
```rust
// moka handles frequency tracking internally via TinyLFU
// We just need to track for our own metrics/debugging
use moka::future::Cache;

pub struct AccessTracker {
    // ChunkId -> (access_count, last_access_ms, project_id)
    tracking: Cache<ChunkId, AccessStats>,
}

impl AccessTracker {
    pub async fn record_access(&self, chunk_id: &ChunkId, project_id: Option<&str>) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Get-or-insert pattern with update
        let stats = self.tracking.get(chunk_id).await.unwrap_or_default();
        let updated = AccessStats {
            access_count: stats.access_count + 1,
            last_access_ms: now_ms,
            project_id: project_id.map(|s| s.to_string()),
        };
        self.tracking.insert(chunk_id.clone(), updated).await;
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Pure LRU caching | TinyLFU/W-TinyLFU | 2017 (paper) | 10-20% higher hit rates, scan-resistant |
| Manual TTL tracking | Library-managed expiration | moka 0.9+ | No background thread management |
| Single-tier HNSW | Tiered hot/warm/cold | Milvus 2.6 (2025) | 80% cost reduction, same perf |
| Exact query cache | Semantic similarity cache | 2024-2025 | 68% cache hit rate vs ~5% exact |

**Deprecated/outdated:**
- Pure LRU: Replaced by TinyLFU for better hit rates
- HashMap + Mutex for caching: Use moka for proper eviction policies
- Blocking cache operations: Use async caches (moka::future)

## Open Questions

Things that couldn't be fully resolved:

1. **Optimal similarity threshold**
   - What we know: Literature suggests 0.83-0.90 range, domain-dependent
   - What's unclear: Optimal for memd's code/doc retrieval use case
   - Recommendation: Start at 0.88, add config flag, tune based on metrics

2. **Version granularity for invalidation**
   - What we know: Need version-based cache invalidation per CONTEXT.md
   - What's unclear: Per-chunk, per-tenant, or global version?
   - Recommendation: Per-tenant version watermark, increment on any chunk change

3. **Hot tier size tuning**
   - What we know: CONTEXT.md says 500-2000 chunks
   - What's unclear: What percentage of warm tier this represents; whether fixed or dynamic
   - Recommendation: Start at 1000, make configurable, potentially auto-tune based on access patterns

## Sources

### Primary (HIGH confidence)
- [moka docs](https://docs.rs/moka/latest/moka/) - TTL, async cache, eviction listeners, TinyLFU policy
- [lfu docs](https://docs.rs/lfu/latest/lfu/) - LFU cache API reference
- Existing codebase: `HnswIndex`, `EmbeddingCache`, `DenseSearcher`

### Secondary (MEDIUM confidence)
- [Shuttle semantic caching tutorial](https://www.shuttle.dev/blog/2024/05/30/semantic-caching-qdrant-rust) - Semantic cache implementation pattern
- [Redis semantic cache optimization](https://redis.io/blog/10-techniques-for-semantic-cache-optimization/) - Threshold tuning, best practices
- [Milvus tiered storage](https://milvus.io/blog/milvus-tiered-storage-80-less-vector-search-cost-with-on-demand-hot%E2%80%93cold-data-loading.md) - Hot/warm/cold architecture patterns

### Tertiary (LOW confidence)
- [GPT Semantic Cache paper](https://arxiv.org/html/2411.05276v2) - 68.8% hit rate claim needs validation in memd context
- Community discussions on similarity thresholds - varies by domain

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - moka is well-documented, widely used, proven in production
- Architecture: HIGH - patterns match existing codebase structure, verified with docs
- Pitfalls: MEDIUM - based on general caching literature, not memd-specific validation

**Research date:** 2026-01-31
**Valid until:** 2026-03-01 (30 days - caching libraries stable)
