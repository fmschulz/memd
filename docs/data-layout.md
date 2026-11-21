# Data layout

Persistent mode writes to:

```
~/.memd/data/
├── .writer.lock                      # Exclusive writer flock
├── embedding_model                   # Embedding model of the dense vectors
├── embedding_revision                # Embedder revision of the dense vectors
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

A writer saves a tenant's `warm_index` once, at shutdown, and only if that
index changed since it was loaded or last saved. A failed save is reported to
the caller. The chunks stay safe in the segments, and the cache-aware backfill
re-indexes anything the files lack.

`embedding_model` records the model that produced the vectors, as its Hugging
Face id. A command with no `--embedding-model` flag uses this model, and a new
store records `BAAI/bge-small-en-v1.5` at its first writable open. See
[Embedding models](configuration.md#embedding-models).

`embedding_revision` records which revision of the embedder produced the
vectors under `warm_index/`. When a release changes how vectors are computed,
the first writable open discards the dense indexes and re-embeds every chunk
from its segments in the background; the file reads `<revision> rebuilding`
until that pass has finished and the rebuilt indexes are saved. A command that
exits during the rebuild waits up to 10 seconds for it, saves what has been
indexed, and the next writer continues from there. Until then, chunks that are
not yet re-embedded are found through the sparse index only. The marker also
moves to `rebuilding` when a writer's embedding model cannot load and it writes
chunks without vectors; the next writer whose model loads embeds only those
chunks.

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
