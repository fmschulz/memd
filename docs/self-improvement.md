# Self-improvement loop

`memd` keeps the working set of takeaways durable and useful across sessions
through four cooperating mechanisms. Each is independent and can be inspected
in isolation.

## 1. Heuristic priority at write time

`memd add` (and the `memory.add` handler) stamp a `priority:N` tag (3..=7)
inferred from the chunk's `ChunkType`, `kind:*` tags, and
validation/finish text signals. Explicit user `priority:` / `importance:`
tags always win on overlap. This makes the `priority_score` formula in
`memory.md` fire without requiring agents to tag every write.

## 2. LLM consolidation

`memd consolidate` builds a working region from chunks written or retrieved
since the last run, asks the configured backend
(`MEMD_CONSOLIDATOR=claude|codex|auto|mock`) to rewrite them into
deduplicated `kind:consolidated` lessons with `supersedes:<csv>` provenance,
and soft-tombstones the sources.

Safety:

- The prompt frames untrusted chunk text as a JSON array so chunks cannot
  forge instructions.
- A single timeout reaps zombie subprocesses on expiry.
- `supersedes` claims are globally deduplicated — the same source can never
  be claimed twice.

## 3. Retrieval-success signal

Every CLI search appends one JSONL record per returned chunk to
`.memd/data/hit_counts.jsonl`. The `memory.md` priority formula consumes a
per-chunk 30-day aggregate (1 h TTL cache):

- frequently retrieved chunks get **up to +8**
- chunks with no hits older than 30 days get **−2**

`memd eval-counterfactual` measures whether the `kind:consolidated` chunks
change ranks versus a same-pass filtered baseline.

## 4. Cross-project transfer

Tenant-wide consolidation (`memd consolidate --tenant-id <t>`, no
`--project-id`) rewrites lessons that recur across a tenant's projects
into `kind:consolidated` chunks, which surface in every project's
`memory.md` through the capped `Machine-Wide Fact Library` (default
`--global-limit 2`; set `--global-limit 0` to disable).

## Session-start hook

The session-start hook ties everything together:

```bash
memd session-start --project-dir "$CLAUDE_PROJECT_DIR"
```

It refreshes `memory.md` synchronously, then kicks a background
`memd consolidate` when **≥ 10 dirty chunks** have accumulated. The
[skill installer](agent-skill.md) wires this into Claude Code by default.
