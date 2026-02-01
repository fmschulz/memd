# Phase 7: Compaction + Cleanup - Context

**Gathered:** 2026-01-31
**Status:** Ready for planning

<domain>
## Phase Boundary

System maintenance operations that run as data grows and changes — ensuring deleted chunks never reappear in any code path, reducing storage fragmentation through segment merges, and rebuilding indexes to remove stale entries. This phase focuses on correctness (tombstone filtering), efficiency (segment compaction), and performance (HNSW cleanup).

</domain>

<decisions>
## Implementation Decisions

### Trigger strategy
- Automatic compaction based on metrics thresholds
- Trigger metrics: tombstone ratio, segment count, HNSW staleness (excluded: storage fragmentation)
- Unified threshold triggers all compaction types (not per-operation or cascading)
- Manual override command available (memory.compact or similar MCP tool) to force compaction regardless of thresholds

### Operation timing
- Claude's discretion: background continuous vs batched vs event-driven
- Claude's discretion: whether to pause compaction under heavy query load
- Claude's discretion: independent parallel vs sequential vs coordinated phases for different compaction operations
- Claude's discretion: crash recovery strategy (resumable checkpoints vs atomic restart)

### Performance impact control
- Claude's discretion: throttling strategy (hard limits vs adaptive vs none)
- No manual pause/abort controls — compaction runs to completion once started, trust throttling
- Claude's discretion: acceptable latency impact during compaction (no noticeable vs small degradation vs best-effort)
- Hot tier serves queries during warm tier compaction (tier coordination required)

### Progress and observability
- Log start/end events and periodic progress updates (excluded: performance metrics, detailed operation trace)
- Claude's discretion: status exposure method (memory.stats vs dedicated tool vs metrics endpoint)
- Claude's discretion: progress update format (percentage vs stage-based vs time estimates)
- Claude's discretion: error handling strategy for compaction failures

### Claude's Discretion
- Exact timing model (continuous, batched, event-driven)
- Load-aware pause behavior
- Operation coordination (parallel, sequential, or dependencies)
- Crash recovery approach (resumable vs atomic)
- Throttling implementation (hard limits, adaptive, or none)
- Latency impact tolerance
- Progress reporting format
- Status exposure mechanism
- Error handling and retry logic

</decisions>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches.

User preferences emphasize:
- Simplicity: unified threshold over complex cascading rules
- Reliability: trust throttling over manual intervention controls
- Tier awareness: hot tier must continue serving queries during warm tier maintenance

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 07-compaction-+-cleanup*
*Context gathered: 2026-01-31*
