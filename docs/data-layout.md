# Data layout

Persistent mode writes to:

```
~/.memd/data/
├── metadata.db                       # SQLite metadata (WAL mode, pooled)
├── sparse_index/                     # tantivy BM25 index (open_or_create)
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

Retrieval/list scans are tolerant of stale metadata rows whose segment payload
is no longer readable: unreadable chunks are logged and skipped. Direct
`memory.get` remains strict so point lookups still surface storage corruption
instead of silently returning the wrong record.

## Disk hygiene

Run `memd maintenance` to sweep orphan HNSW snapshots and report what
changed. Useful flags:

```bash
memd maintenance --dry-run                  # report what would change
memd maintenance --aggressive               # run the full pass
memd maintenance --tenant-id <id>           # restrict to one tenant
```

The orphan sweep targets `graph-NNNN.hnsw.{graph,data}` files left by older
builds before the hnsw_rs orphan-snapshot fix shipped. Output is greppable
`key:value` so ops scripts can wire it up directly. Safe to run while no
writer process is active.

## Why bincode for the mapping?

Older builds wrote `mapping.json` (~5× larger). v0.50.0 packs the same
chunk-id → HNSW-index mapping as bincode `mapping.bin`. The reader still
accepts the legacy JSON format and auto-migrates on next save.
