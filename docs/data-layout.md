# Data layout

Persistent mode writes to:

```
~/.memd/data/
├── .writer.lock                      # Exclusive writer flock
├── metadata.db                       # SQLite metadata (WAL mode, pooled)
├── sparse_index/                     # tantivy BM25 index (open_or_create)
├── warm/<hash>/                      # 0700 runtime dir for warm worker socket
│                                      # socket file chmod 0600
└── tenants/
    └── <tenant_id>/
        ├── wal.log                   # Append-only WAL; fsync before commit
        ├── segments/                 # Immutable chunk segments + payload
        └── warm_index/               # HNSW state
            ├── embeddings.bin        # Source of truth for vectors
            ├── mapping.bin           # bincode (legacy: mapping.json)
            ├── config.json           # HnswConfig snapshot
            └── graph.hnsw.{graph,data}  # Optional fast-load dump
                                          # (skipped when persist_graph_dump=false)
```

Default data dir: `~/.memd/data`. Override with `--data-dir`.

Retrieval/list scans are tolerant of stale metadata rows with unreadable segment
payloads: unreadable chunks are logged and skipped. Direct
`memory.get` remains strict so point lookups still surface storage corruption
instead of silently returning the wrong record.

## Disk hygiene

Run `memd maintenance` to sweep orphan HNSW snapshots and report the result.
Use `--aggressive` to force-merge the global Tantivy sparse index into one
searchable segment:

```bash
memd maintenance --dry-run                  # report what would change
memd maintenance --aggressive               # run the full pass
memd maintenance --tenant-id <id>           # restrict the HNSW sweep only
```

The orphan sweep targets `graph-NNNN.hnsw.{graph,data}` files. Aggressive
output includes `sparse_segments_before`, `sparse_segments_after`, and
`segments_merged`. Output uses `key:value` lines for shell parsing. The command
takes the data-directory writer lock and does not run through the warm worker.

## Why bincode for the mapping?

Older builds wrote `mapping.json` (~5× larger). v0.50.0 packs the same
chunk-id → HNSW-index mapping as bincode `mapping.bin`. The reader still
accepts the legacy JSON format and auto-migrates on next save.
