# Item 1 — F-track verification gate

**Date:** 2026-04-20
**Baseline:** `e6cc7e5` (pre-F, after D/E/G1/G2)
**Current:** `64c7edd` (HEAD, post Items 2/3/4/6)

## Target

Confirm F-track (F1-F7 + G3) did not regress retrieval quality within ±3%
on Recall@10 / MRR / Precision@10. See post-nanomem handoff next-step #1.

## Outcome

**Quantitative gate unresolved / uninformative in this environment; accepted
here by non-regression argument over the retrieval code path.** The ±3%
gate is not empirically measured below — it is a constructive non-regression
claim. If the requirement is a measured ±3% gate, the Candle embedder URL
bug (see below) remains an open blocker for Item 1.

## Quantitative gate: env-blocked

The retrieval harness at `evals/harness/src/suites/retrieval.rs` requires the
Candle embedder to download `sentence-transformers/all-MiniLM-L6-v2`. In this
environment, the embedder's config fetch fails with

    embedding error: Failed to download config: request error: Bad URL:
    failed to parse URL: RelativeUrlWithoutBase: relative URL without a base

— a pre-existing bug in `embeddings::candle_embedder` unrelated to F-track.
`memd` falls back to text-only search, which returns 0 matches on the
semantic-style queries in `code_pairs.json`. Both pre-F and current report
Recall@10=0.000 / MRR=0.000 / Precision@10=0.000. The Δ=0 is within ±3%
arithmetically, but the underlying numbers carry no retrieval signal.

## Constructive gate: code-diff (retrieval algorithm + surface)

F-track diff (`e6cc7e5..df2d3b4`) touches these files under `crates/memd/src/`:

```
 crates/memd/src/cli.rs          | 412 +++++++++++++++ (CLI: export-omf/import-omf/export-markdown subcommands)
 crates/memd/src/lib.rs          |   1 + (module registration)
 crates/memd/src/mcp/handlers.rs | 161 +++++++++ (MCP: memory.export_omf/preview_omf_import/import_omf)
 crates/memd/src/mcp/server.rs   |  58 +++++- (MCP tool registration)
 crates/memd/src/mcp/tools.rs    | 102 ++++++- (MCP tool schema)
 crates/memd/src/omf/export.rs   | 185 ++++++++++ (new)
 crates/memd/src/omf/import.rs   | 552 ++++++++++++++++++++ (new)
 crates/memd/src/omf/mod.rs      | 193 ++++++++++ (new)
 crates/memd/src/omf/time.rs     | 213 +++++++++ (new RFC-3339 formatter)
```

F-track also adds `crates/memd/tests/omf_roundtrip.rs` (integration tests) and
`docs/omf.md` (spec). Neither affects production code.

**Retrieval algorithm code path is unchanged:**

- `crates/memd/src/store/persistent.rs` — 0 changes (hybrid_search, dense_search, text-only fallback, get_chunk_for_retrieval).
- `crates/memd/src/retrieval/` — 0 changes (cross-encoder, reranking).
- `crates/memd/src/embeddings/` — 0 changes (Candle embedder, download, pooling).
- `crates/memd/src/compaction/` — 0 changes (sweeps, history promotion).

**Retrieval-surface code (what retrieval can see) is unchanged by F-track:**

- `crates/memd/src/mcp/handlers.rs::handle_memory_search` (filter /
  visibility / project-scope) — not touched by F-track; the handlers.rs
  hunk in the F-track range adds only `memory.export_omf` /
  `preview_omf_import` / `import_omf` handlers.
- `crates/memd/src/store/metadata/sqlite.rs` — 0 changes in F-track. The
  metadata-index change landed later in Item 2 (`df2d3b4..64c7edd`).

**Items 2/3/4/6 (`df2d3b4..64c7edd`, post-F) caveat.** Item 2 is not pure
bookkeeping — it fixes a cross-tenant metadata overwrite bug in
`store/metadata/sqlite.rs`. In a multi-tenant persistent store, that fix
can change which rows a given tenant's retrieval sees (legacy behavior:
tenant-A rows could be silently overwritten by tenant-B writes that
collided on the global `UNIQUE(segment_id, ordinal)`; new behavior:
each tenant's rows are preserved). On the single-tenant eval corpus this
has no effect; on a multi-tenant production store it strictly *improves*
recall by not losing rows.

OMF import (`crates/memd/src/omf/import.rs` writing through
`crates/memd/src/store/persistent.rs::add_chunk`) intentionally changes
what becomes searchable, but only for chunks the caller imports — it
adds new documents, it does not alter retrieval over previously-written
chunks.

## Test-surface gate

Full `cargo test -p memd --tests` on both revisions:

| Revision | Lib | Integration | Total passed | Ignored | Failed |
|----------|-----|-------------|--------------|---------|--------|
| `e6cc7e5` pre-F | 669 | 112 | **781** | 4 | 0 |
| `64c7edd` current | 719 | 146 | **865** | 4 | 0 |

`cargo test -p memd --tests` exercises retrieval semantics through the lib
test surface — text-only fallback, hybrid retrieval, and dense/HNSW
recovery tests live in `crates/memd/src/store/persistent.rs`, and
`memory.search` behavior is covered in `crates/memd/src/mcp/handlers.rs`
lib tests. Passing all 781 baseline tests at HEAD, plus +84 new tests
(50 lib + 34 OMF roundtrip integration), establishes no retrieval
regression against the test surface. It does NOT satisfy the ±3%
Recall/MRR/P@10 gate — only the eval harness does that.

## Conclusion

- **Retrieval algorithm:** unchanged from `e6cc7e5` to `64c7edd`. No direct
  retrieval-algorithm change from F-track or Items 2/3/4/6.
- **Retrieval surface:** unchanged by F-track. Post-F Item 2 changes the
  metadata-index (cross-tenant bug fix); this strictly preserves rows
  and is retrieval-neutral on single-tenant corpora.
- **Test-surface:** 781 → 865, zero regressions.
- **Empirical ±3% gate:** unresolved — blocked by a pre-existing Candle
  embedder config-URL parse bug (`RelativeUrlWithoutBase`). Deferred as
  a separate follow-up.
