# OMF 1.0 — Open Memory Format

OMF 1.0 is memd's JSON interchange for memory export / import. The wire
shape is nanomem-compatible: a downstream nanomem consumer can ingest a
memd export and vice versa. Anything memd-specific lives inside a
versioned `extensions.memd` namespace that third-party importers are free
to ignore.

This document is the source of truth for the wire format and trust
semantics. See `docs/plans/active/2026-04-18-nanomem-inspired-features.md`
§F for the implementation plan.

## Envelope

```json
{
  "omf": "1.0",
  "exported_at": "2026-04-18T00:00:00Z",
  "source": { "app": "memd" },
  "memories": [ /* OmfItem... */ ]
}
```

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `omf` | string | yes | Wire version. memd 0.7.x produces and accepts `"1.0"`. Anything else fails validation. |
| `exported_at` | string | yes | RFC-3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`, seconds precision). Not interpreted on import. |
| `source` | object | no | Producer identity. See **Trust gate** below. Omitting it is equivalent to "untrusted source". |
| `memories` | array | yes | Zero or more `OmfItem`s. An empty array is a valid "nothing to export" envelope. |

A document missing the `memories` key fails JSON deserialization. An
item with empty or whitespace-only `content` fails `validate_omf`.

## Item (`OmfItem`)

```json
{
  "content": "fact A",
  "category": "topic:release",
  "tags": ["topic:release", "owner:alice"],
  "status": "archived",
  "created_at": "2026-04-18",
  "updated_at": "2026-04-18",
  "expires_at": "2026-05-01",
  "extensions": {
    "memd": {
      "v": 1,
      "chunk_id": "018fa6...",
      "project_id": "p1",
      "chunk_type": "doc",
      "ingestion_mode": "document",
      "lifecycle": {
        "status": "final",
        "tier": "working",
        "supersedes": "018fa5...",
        "superseded_by": null,
        "expires_at_ms": null,
        "review_after_ms": 1800000000000,
        "lifecycle_updated_at_ms": 1800000000000
      }
    }
  }
}
```

### Standard (nanomem-compatible) fields

| Field | Type | Required | Behaviour |
|-------|------|----------|-----------|
| `content` | string | yes | The memory text. |
| `category` | string | no | Free-form. On import: used as a **fallback** `project_id` when `extensions.memd.project_id` is absent. |
| `tags` | string[] | no | Applied verbatim to the imported chunk. |
| `status` | string | no | Archival vocabulary. Values `"archived"` / `"expired"` cause the item to be skipped on import when `include_archived=false`. Anything else is treated as absent (the imported chunk lands as `status=Final`). |
| `created_at` | string | no | Date (`YYYY-MM-DD`) or RFC-3339. Informational; not used on import. |
| `updated_at` | string | no | Same as `created_at`. |
| `expires_at` | string | no | Date or RFC-3339. Informational; the ms-precision version lives in `extensions.memd.lifecycle.expires_at_ms` under the trust gate. |
| `extensions` | object | no | App-specific scratch space. Keys under `extensions.<appname>` are reserved for that app. |

### `extensions.memd` namespace

memd writes (and trusts on import) the following fields under
`extensions.memd`:

| Field | Type | Emitted | Consumed |
|-------|------|---------|----------|
| `v` | integer | Always `1` | Must equal `1` for lifecycle to be honoured on import. |
| `chunk_id` | string | UUIDv7 string | Informational; imports mint a fresh `chunk_id`. |
| `project_id` | string or null | Source row's project | Primary project resolution on import. |
| `chunk_type` | string | `code`/`doc`/`trace`/`decision`/`plan`/`research`/`message`/`summary`/`other` | Parsed on import via `ChunkType::FromStr`. Unknown → defaults to `doc`. |
| `ingestion_mode` | string | `conversation` / `document` | Parsed on import via `IngestionMode::FromStr`. Unknown → import default (`document`). |
| `lifecycle` | object | Always present | Parsed only under the trust gate. See below. |

The `lifecycle` object carries:

| Field | Type | Purpose |
|-------|------|---------|
| `status` | string | `final` / `superseded` / `expired` / `draft` / `error` / `deleted` |
| `tier` | string | `working` / `long_term` / `history` |
| `supersedes` | string or null | `chunk_id` of the row this supersedes |
| `superseded_by` | string or null | `chunk_id` of the row that supersedes this |
| `expires_at_ms` | integer or null | Unix ms; past values are lazily treated as expired |
| `review_after_ms` | integer or null | Unix ms; informational |
| `lifecycle_updated_at_ms` | integer | Unix ms of last overlay update |

## Trust gate

Lifecycle metadata in `extensions.memd.lifecycle` is honoured on import
**only** when both hold:

1. `source.app == "memd"`, AND
2. `extensions.memd.v == 1` (the version this build trusts).

Otherwise the imported chunk starts at memd defaults (`status=Final`,
`tier=LongTerm`, no expiry, no review) regardless of what the item's
extensions claim. This prevents a third-party producer from forcing a
chunk into `tier=History` or `status=Expired` on import.

### Fail-closed parsing within trusted documents

When both trust predicates pass, the lifecycle block is parsed strictly:

- Non-string `status` / `tier` → `ValidationError`.
- Unknown `status` / `tier` value → `ValidationError`.
- Non-integer `*_ms` fields → `ValidationError`.
- Non-object `lifecycle` → `ValidationError`.

Missing fields and explicit `null`s are equivalent to "unset" and are
not errors — this lets `memd` round-trip its own exports, which emit
`null` for optional lifecycle fields.

## Export semantics

`memory.export_omf(tenant_id, project_id?, include_history=false,
include_superseded=true, include_expired=true)` emits every eligible
chunk in ascending `timestamp_created` order. Exclusions:

- `status=Deleted` / `status=Error` rows are always skipped.
- `include_history=false` (default) excludes `tier=History` rows via
  the `list_for_export` SQL filter, with a defence-in-depth tier check
  in the loop.
- `include_superseded=false` drops `status=Superseded` rows.
- `include_expired=false` drops both `status=Expired` rows AND rows
  whose `lifecycle.expires_at_ms` is in the past (lazy-expiry clock
  check, mirroring `VisibilityPolicy::is_visible_at`).

If a metadata row surfaces without a resolvable payload (compaction
race, real drift), the export logs at `debug!` and skips the row
without erroring out.

## Import semantics

`memory.import_omf(tenant_id, document, include_archived=true,
fuzzy_threshold=None)` iterates `document.memories` once and lands each
item in exactly one of:

| Bucket | Cause |
|--------|-------|
| `imported` | Item passed dedup + filter and was written to storage. |
| `duplicates` | Item's canonical form matched an existing row (exact-canonical or, with `fuzzy_threshold`, trigram Jaccard). |
| `skipped` | Item's top-level `status` was `archived`/`expired` and `include_archived=false`. |

Scopes on dedup:

- When the resolved `project_id` is `Some`, dedup compares against
  rows in that project only.
- When `project_id` is `None`, dedup compares against
  **`project_id IS NULL` rows only**. The SQL helpers widen
  `None → any project`, so this path uses the explicit NULL-only
  queries (`list_recent_with_null_project` for fuzzy and a post-filter
  for exact) to avoid falsely deduping against scoped rows.

Per-item project resolution:

1. `extensions.memd.project_id` if present.
2. Otherwise `category` (nanomem convention).
3. Otherwise `None` (unscoped).

## Preview semantics

`memory.preview_omf_import(tenant_id, document, include_archived=true,
fuzzy_threshold=None)` walks the same dedup + filter + trust-gate path
as `import_omf` but never writes and never bumps
`tenant_memory_version`. Returns:

| Field | Type | Meaning |
|-------|------|---------|
| `total` | integer | `document.memories.len()` |
| `to_import` | integer | Items that would land as `imported` |
| `duplicates` | integer | Items that would dedupe |
| `filtered` | integer | Items that would be filtered by `include_archived=false` |
| `unscoped` | integer | Of `to_import`, the count whose resolved `project_id` is `None` |
| `by_project` | object | `{project_id: count}` for real scoped `to_import` items |

The `unscoped` field is a dedicated counter so a user project literally
named `_` does not collide with the "no project" bucket — an earlier
sentinel-based design was replaced per review feedback.

Preview runs `extract_lifecycle_strict` on trusted items too, so a
caller does not see "looks fine" and then have the subsequent real
import fail-closed on a malformed lifecycle block.

## CLI

Two subcommands ship with the memd binary (they run on the user's
machine, outside the daemon trust boundary):

```bash
memd export-omf --tenant-id t [--project-id p] [--output PATH] \
                [--include-history true|false] \
                [--include-superseded true|false] \
                [--include-expired true|false]

memd import-omf --tenant-id t [--input PATH | -] \
                [--include-archived true|false] \
                [--fuzzy-threshold F] \
                [--dry-run]
```

- `--output` writes JSON to a file; omit for stdout.
- `--input PATH`, `--input -`, or omission reads the document from the
  path or stdin.
- `--dry-run` runs `preview_omf_import` instead of writing. This path
  does **not** create the tenant directory on disk (materialisation is
  deferred to the real-write branch).

## Compatibility and versioning

- `omf` version is gated strictly: only `"1.0"` is accepted.
- `extensions.memd.v` is gated strictly: only `1` is trusted.
- A future version of memd that wants to extend the lifecycle overlay
  schema must bump `MEMD_EXT_VERSION` AND add a new trust branch for
  the bumped version — older writers continue to be accepted, but only
  at their own trusted version.
- Untrusted producers (`source.app != "memd"` or missing `source`)
  always import with default lifecycle, regardless of version. Their
  `content`, `category`, `tags`, and `extensions.memd.project_id` (if
  present) still apply.
