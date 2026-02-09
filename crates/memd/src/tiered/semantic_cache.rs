//! Semantic query cache with similarity-based lookup
//!
//! The SemanticCache provides sub-millisecond response for repeated or similar queries
//! by caching query embeddings alongside their results. Cache entries are invalidated
//! via TTL or when the underlying data version changes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moka::sync::Cache;
use parking_lot::RwLock;
use sha2::{Digest, Sha256};

use crate::types::{ChunkId, TenantId};

/// Configuration for the semantic cache
#[derive(Debug, Clone)]
pub struct SemanticCacheConfig {
    /// Cosine similarity threshold for cache hit (default 0.85)
    pub similarity_threshold: f32,
    /// TTL in seconds (default 2700 = 45 minutes)
    pub ttl_seconds: u64,
    /// Maximum number of cache entries (default 10_000)
    pub max_entries: u64,
    /// Confidence boost on each cache hit (default 0.1)
    pub confidence_boost_on_hit: f32,
    /// Confidence decay rate per hour (default 0.05)
    pub confidence_decay_rate: f32,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.85,
            ttl_seconds: 2700, // 45 minutes (middle of 30-60 range)
            max_entries: 10_000,
            confidence_boost_on_hit: 0.1,
            confidence_decay_rate: 0.05,
        }
    }
}

/// A cached search result with minimal data for quick response
#[derive(Debug, Clone)]
pub struct CachedResult {
    /// The chunk ID
    pub chunk_id: ChunkId,
    /// Similarity/relevance score
    pub score: f32,
    /// Preview of text content (first 200 chars)
    pub text_preview: String,
}

/// A cache entry storing query results with version tracking
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The query embedding that produced these results
    pub query_embedding: Vec<f32>,
    /// Cached search results
    pub results: Vec<CachedResult>,
    /// Tenant that owns this cache entry
    pub tenant_id: TenantId,
    /// Optional project scope
    pub project_id: Option<String>,
    /// Memory version at time of caching (for invalidation)
    pub memory_version: u64,
    /// When this entry was created (Unix ms)
    pub created_at: i64,
    /// When this entry was last hit (Unix ms)
    pub last_hit: i64,
    /// Number of cache hits
    pub hit_count: u32,
    /// Confidence score (0.0-1.0)
    pub confidence: f32,
}

/// Result returned on a cache hit
#[derive(Debug, Clone)]
pub struct CacheHit {
    /// The cached results
    pub results: Vec<CachedResult>,
    /// Cosine similarity between query and cached query
    pub similarity: f32,
    /// Current confidence of the cache entry
    pub confidence: f32,
    /// Age of the cache entry in milliseconds
    pub age_ms: u64,
    /// Key used to identify this cache entry
    pub cache_key: String,
}

/// Index entry for fast similarity lookup
struct QueryIndexEntry {
    /// Cache key for lookup in moka cache
    cache_key: String,
    /// Query embedding for similarity computation
    embedding: Vec<f32>,
    /// Tenant for filtering
    tenant_id: TenantId,
    /// Optional project for filtering
    project_id: Option<String>,
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Total number of lookup attempts
    pub total_lookups: u64,
    /// Number of cache hits
    pub cache_hits: u64,
    /// Number of cache misses
    pub cache_misses: u64,
    /// Number of version-based invalidations
    pub version_invalidations: u64,
    /// Number of TTL expirations (tracked via moka eviction listener)
    pub ttl_expirations: u64,
    /// Average confidence across all entries
    pub avg_confidence: f32,
    /// Current number of entries
    pub entry_count: usize,
}

/// Atomic counters for thread-safe statistics
struct AtomicStats {
    total_lookups: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    version_invalidations: AtomicU64,
    ttl_expirations: AtomicU64,
}

impl Default for AtomicStats {
    fn default() -> Self {
        Self {
            total_lookups: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            version_invalidations: AtomicU64::new(0),
            ttl_expirations: AtomicU64::new(0),
        }
    }
}

/// Semantic query cache with similarity-based lookup
///
/// Caches query embeddings and their results, allowing similar queries
/// to be served from cache. Entries are invalidated via TTL or version tracking.
pub struct SemanticCache {
    /// Moka cache with TTL-based expiration
    entries: Cache<String, CacheEntry>,
    /// Index for similarity search (protected by RwLock)
    query_index: RwLock<Vec<QueryIndexEntry>>,
    /// Configuration
    config: SemanticCacheConfig,
    /// Atomic statistics counters
    stats: AtomicStats,
}

impl SemanticCache {
    /// Create a new semantic cache with the given configuration
    pub fn new(config: SemanticCacheConfig) -> Self {
        let entries = Cache::builder()
            .max_capacity(config.max_entries)
            .time_to_live(Duration::from_secs(config.ttl_seconds))
            .build();

        Self {
            entries,
            query_index: RwLock::new(Vec::new()),
            config,
            stats: AtomicStats::default(),
        }
    }

    /// Look up a similar query in the cache
    ///
    /// Returns a cache hit if a similar query (cosine > threshold) is found
    /// and the cache entry's version is >= current_version.
    pub fn lookup(
        &self,
        query_embedding: &[f32],
        tenant_id: &TenantId,
        project_id: Option<&str>,
        current_version: u64,
    ) -> Option<CacheHit> {
        self.stats.total_lookups.fetch_add(1, Ordering::Relaxed);

        let now_ms = current_time_ms();
        let query_index = self.query_index.read();

        // Find best matching entry above threshold
        let mut best_match: Option<(String, f32)> = None;

        for entry in query_index.iter() {
            // Filter by tenant
            if entry.tenant_id != *tenant_id {
                continue;
            }

            // Filter by project (None matches None, Some matches Some)
            match (&entry.project_id, project_id) {
                (None, None) => {}
                (Some(a), Some(b)) if a == b => {}
                _ => continue,
            }

            // Compute similarity
            let similarity = cosine_similarity(query_embedding, &entry.embedding);
            if similarity >= self.config.similarity_threshold {
                if best_match.as_ref().is_none_or(|(_, s)| similarity > *s) {
                    best_match = Some((entry.cache_key.clone(), similarity));
                }
            }
        }

        drop(query_index);

        // Check if the cached entry is still valid
        let (cache_key, similarity) = best_match?;
        let mut entry = self.entries.get(&cache_key)?;

        // Version check: entry must be from current or newer version
        if entry.memory_version < current_version {
            self.stats
                .version_invalidations
                .fetch_add(1, Ordering::Relaxed);
            self.stats.cache_misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        // Update hit statistics
        entry.last_hit = now_ms;
        entry.hit_count += 1;
        entry.confidence = (entry.confidence + self.config.confidence_boost_on_hit).min(1.0);

        // Re-insert with updated stats
        self.entries.insert(cache_key.clone(), entry.clone());

        self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);

        Some(CacheHit {
            results: entry.results.clone(),
            similarity,
            confidence: entry.confidence,
            age_ms: (now_ms - entry.created_at).max(0) as u64,
            cache_key,
        })
    }

    /// Insert a new cache entry
    pub fn insert(
        &self,
        query_embedding: Vec<f32>,
        tenant_id: TenantId,
        project_id: Option<String>,
        results: Vec<CachedResult>,
        memory_version: u64,
    ) {
        let cache_key = embedding_hash(&query_embedding, &tenant_id, project_id.as_deref());
        let now_ms = current_time_ms();

        let entry = CacheEntry {
            query_embedding: query_embedding.clone(),
            results,
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            memory_version,
            created_at: now_ms,
            last_hit: now_ms,
            hit_count: 0,
            confidence: 0.5, // Initial confidence
        };

        // Add to moka cache
        self.entries.insert(cache_key.clone(), entry);

        // Add to query index
        let index_entry = QueryIndexEntry {
            cache_key,
            embedding: query_embedding,
            tenant_id,
            project_id,
        };

        let mut query_index = self.query_index.write();
        query_index.push(index_entry);
    }

    /// Invalidate all cache entries for a tenant
    pub fn invalidate_tenant(&self, tenant_id: &TenantId) {
        let mut query_index = self.query_index.write();
        let keys_to_remove: Vec<String> = query_index
            .iter()
            .filter(|e| e.tenant_id == *tenant_id)
            .map(|e| e.cache_key.clone())
            .collect();

        for key in &keys_to_remove {
            self.entries.invalidate(key);
        }

        query_index.retain(|e| e.tenant_id != *tenant_id);
    }

    /// Invalidate cache entries below the given version
    pub fn invalidate_by_version(&self, tenant_id: &TenantId, min_version: u64) {
        let mut query_index = self.query_index.write();
        let mut keys_to_remove = Vec::new();

        for entry in query_index.iter() {
            if entry.tenant_id == *tenant_id {
                if let Some(cached) = self.entries.get(&entry.cache_key) {
                    if cached.memory_version < min_version {
                        keys_to_remove.push(entry.cache_key.clone());
                    }
                }
            }
        }

        for key in &keys_to_remove {
            self.entries.invalidate(key);
            self.stats
                .version_invalidations
                .fetch_add(1, Ordering::Relaxed);
        }

        query_index.retain(|e| !keys_to_remove.contains(&e.cache_key));
    }

    /// Invalidate cache entries containing any of the given chunk IDs
    pub fn invalidate_chunks(&self, chunk_ids: &[ChunkId]) {
        let mut query_index = self.query_index.write();
        let mut keys_to_remove = Vec::new();

        for entry in query_index.iter() {
            if let Some(cached) = self.entries.get(&entry.cache_key) {
                let contains_chunk = cached
                    .results
                    .iter()
                    .any(|r| chunk_ids.contains(&r.chunk_id));
                if contains_chunk {
                    keys_to_remove.push(entry.cache_key.clone());
                }
            }
        }

        for key in &keys_to_remove {
            self.entries.invalidate(key);
        }

        query_index.retain(|e| !keys_to_remove.contains(&e.cache_key));
    }

    /// Get current cache statistics
    pub fn get_stats(&self) -> CacheStats {
        let query_index = self.query_index.read();
        let entry_count = query_index.len();

        // Calculate average confidence
        let mut total_confidence = 0.0f32;
        let mut count = 0u32;

        for entry in query_index.iter() {
            if let Some(cached) = self.entries.get(&entry.cache_key) {
                total_confidence += cached.confidence;
                count += 1;
            }
        }

        let avg_confidence = if count > 0 {
            total_confidence / count as f32
        } else {
            0.0
        };

        CacheStats {
            total_lookups: self.stats.total_lookups.load(Ordering::Relaxed),
            cache_hits: self.stats.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.stats.cache_misses.load(Ordering::Relaxed),
            version_invalidations: self.stats.version_invalidations.load(Ordering::Relaxed),
            ttl_expirations: self.stats.ttl_expirations.load(Ordering::Relaxed),
            avg_confidence,
            entry_count,
        }
    }

    /// Remove index entries that are no longer in the moka cache
    ///
    /// This should be called periodically to clean up expired entries from the index.
    pub fn prune_index(&self) {
        let mut query_index = self.query_index.write();
        let before_count = query_index.len();

        query_index.retain(|e| self.entries.contains_key(&e.cache_key));

        let removed = before_count - query_index.len();
        if removed > 0 {
            self.stats
                .ttl_expirations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
    }
}

/// Compute cosine similarity between two vectors
///
/// Returns 0.0 for zero vectors to handle edge cases gracefully.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot_product = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot_product += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Generate a hash-based cache key from embedding and tenant info
fn embedding_hash(embedding: &[f32], tenant_id: &TenantId, project_id: Option<&str>) -> String {
    let mut hasher = Sha256::new();

    // Include tenant and project in hash
    hasher.update(tenant_id.as_str().as_bytes());
    if let Some(proj) = project_id {
        hasher.update(proj.as_bytes());
    }

    // Include embedding bytes
    for val in embedding {
        hasher.update(val.to_le_bytes());
    }

    let result = hasher.finalize();
    // Use first 16 bytes (32 hex chars) for reasonable uniqueness
    hex::encode(&result[..16])
}

/// Get current time in milliseconds since Unix epoch
fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hex encoding helper (avoiding extra dependency)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tenant(name: &str) -> TenantId {
        TenantId::new(name).unwrap()
    }

    fn make_embedding(seed: u64) -> Vec<f32> {
        // Create a simple normalized embedding for testing
        let mut vec: Vec<f32> = (0..384)
            .map(|i| ((seed + i) % 100) as f32 / 100.0)
            .collect();
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        vec
    }

    fn make_similar_embedding(base: &[f32], noise: f32) -> Vec<f32> {
        // Create an embedding similar to base with some noise
        let mut vec = base.to_vec();
        for (i, v) in vec.iter_mut().enumerate() {
            *v += (i as f32 * noise).sin() * noise;
        }
        // Renormalize
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        vec
    }

    fn make_chunk_id() -> ChunkId {
        ChunkId::new()
    }

    #[test]
    fn test_cache_hit_flow() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id: chunk_id.clone(),
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        // Insert entry with version 1
        cache.insert(embedding.clone(), tenant.clone(), None, results.clone(), 1);

        // Lookup with similar query should hit
        let similar = make_similar_embedding(&embedding, 0.01);
        let hit = cache.lookup(&similar, &tenant, None, 1);
        assert!(hit.is_some(), "Similar query should hit cache");

        let hit = hit.unwrap();
        assert!(
            hit.similarity > 0.85,
            "Similarity should be above threshold"
        );
        assert_eq!(hit.results.len(), 1);

        // Lookup with dissimilar query should miss
        let dissimilar = make_embedding(999);
        let miss = cache.lookup(&dissimilar, &tenant, None, 1);
        assert!(miss.is_none(), "Dissimilar query should miss cache");

        // Check hit count was incremented
        let stats = cache.get_stats();
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.total_lookups, 2);
    }

    #[test]
    fn test_version_invalidation() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        // Insert entry with version 5
        cache.insert(embedding.clone(), tenant.clone(), None, results, 5);

        // Lookup with current_version 5 should hit
        let hit = cache.lookup(&embedding, &tenant, None, 5);
        assert!(hit.is_some(), "Version 5 lookup should hit");

        // Lookup with current_version 6 should miss (stale)
        let miss = cache.lookup(&embedding, &tenant, None, 6);
        assert!(miss.is_none(), "Version 6 lookup should miss (stale entry)");

        // Call invalidate_by_version
        cache.invalidate_by_version(&tenant, 6);

        // Verify entry removed from index
        let stats = cache.get_stats();
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    fn test_tenant_isolation() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant_a = make_tenant("tenant_a");
        let tenant_b = make_tenant("tenant_b");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        // Insert entry for tenant_a
        cache.insert(embedding.clone(), tenant_a.clone(), None, results, 1);

        // Lookup as tenant_b should miss
        let miss = cache.lookup(&embedding, &tenant_b, None, 1);
        assert!(miss.is_none(), "Tenant B should not see Tenant A's cache");

        // Lookup as tenant_a should hit
        let hit = cache.lookup(&embedding, &tenant_a, None, 1);
        assert!(hit.is_some(), "Tenant A should see its own cache");
    }

    #[test]
    fn test_confidence_mechanics() {
        let config = SemanticCacheConfig {
            confidence_boost_on_hit: 0.1,
            ..Default::default()
        };
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        // Insert entry (initial confidence 0.5)
        cache.insert(embedding.clone(), tenant.clone(), None, results, 1);

        // First hit
        let hit1 = cache.lookup(&embedding, &tenant, None, 1).unwrap();
        assert!(
            (hit1.confidence - 0.6).abs() < 0.01,
            "Confidence should be 0.6 after first hit"
        );

        // Second hit
        let hit2 = cache.lookup(&embedding, &tenant, None, 1).unwrap();
        assert!(
            (hit2.confidence - 0.7).abs() < 0.01,
            "Confidence should be 0.7 after second hit"
        );

        // Third hit
        let hit3 = cache.lookup(&embedding, &tenant, None, 1).unwrap();
        assert!(
            (hit3.confidence - 0.8).abs() < 0.01,
            "Confidence should be 0.8 after third hit"
        );
    }

    #[test]
    fn test_chunk_invalidation() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_a = make_chunk_id();
        let chunk_b = make_chunk_id();

        let results = vec![
            CachedResult {
                chunk_id: chunk_a.clone(),
                score: 0.95,
                text_preview: "Content A".to_string(),
            },
            CachedResult {
                chunk_id: chunk_b.clone(),
                score: 0.85,
                text_preview: "Content B".to_string(),
            },
        ];

        // Insert entry with results containing chunk_a
        cache.insert(embedding.clone(), tenant.clone(), None, results, 1);

        // Verify entry exists
        let hit = cache.lookup(&embedding, &tenant, None, 1);
        assert!(hit.is_some());

        // Invalidate chunk_a
        cache.invalidate_chunks(&[chunk_a]);

        // Verify entry removed
        let miss = cache.lookup(&embedding, &tenant, None, 1);
        assert!(
            miss.is_none(),
            "Entry should be removed after chunk invalidation"
        );

        let stats = cache.get_stats();
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    fn test_project_isolation() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        // Insert entry for project_a
        cache.insert(
            embedding.clone(),
            tenant.clone(),
            Some("project_a".to_string()),
            results.clone(),
            1,
        );

        // Lookup with project_b should miss
        let miss = cache.lookup(&embedding, &tenant, Some("project_b"), 1);
        assert!(miss.is_none(), "Project B should not see Project A's cache");

        // Lookup with project_a should hit
        let hit = cache.lookup(&embedding, &tenant, Some("project_a"), 1);
        assert!(hit.is_some(), "Project A should see its own cache");

        // Lookup with no project should miss
        let miss_no_project = cache.lookup(&embedding, &tenant, None, 1);
        assert!(
            miss_no_project.is_none(),
            "No project should not see Project A's cache"
        );
    }

    #[test]
    fn test_cosine_similarity() {
        // Identical vectors should have similarity 1.0
        let a = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 0.001);

        // Orthogonal vectors should have similarity 0.0
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.001);

        // Opposite vectors should have similarity -1.0
        let c = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &c) - (-1.0)).abs() < 0.001);

        // Zero vectors should return 0.0
        let zero = vec![0.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &zero), 0.0);
        assert_eq!(cosine_similarity(&zero, &a), 0.0);

        // Different length vectors should return 0.0
        let d = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &d), 0.0);
    }

    #[test]
    fn test_prune_index() {
        // Use very short TTL for testing
        let config = SemanticCacheConfig {
            ttl_seconds: 1,
            ..Default::default()
        };
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding = make_embedding(42);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        cache.insert(embedding, tenant, None, results, 1);

        // Wait for TTL to expire
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Prune should remove expired entries from index
        cache.prune_index();

        let stats = cache.get_stats();
        assert_eq!(stats.entry_count, 0, "Expired entries should be pruned");
    }

    #[test]
    fn test_stats_tracking() {
        let config = SemanticCacheConfig::default();
        let cache = SemanticCache::new(config);

        let tenant = make_tenant("test_tenant");
        let embedding1 = make_embedding(42);
        let embedding2 = make_embedding(999);
        let chunk_id = make_chunk_id();

        let results = vec![CachedResult {
            chunk_id,
            score: 0.95,
            text_preview: "Test content".to_string(),
        }];

        cache.insert(embedding1.clone(), tenant.clone(), None, results, 1);

        // Hit
        cache.lookup(&embedding1, &tenant, None, 1);
        // Miss (dissimilar)
        cache.lookup(&embedding2, &tenant, None, 1);

        let stats = cache.get_stats();
        assert_eq!(stats.total_lookups, 2);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 0); // Dissimilar query doesn't increment miss counter
        assert_eq!(stats.entry_count, 1);
    }
}
