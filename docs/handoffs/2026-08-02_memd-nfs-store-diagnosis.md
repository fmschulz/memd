# Handoff: memd - NFS Store Diagnosis
**Date:** 2026-08-02
**Branch:** `fix/nfs-safe-runtime-recovery`
**HEAD:** `4bc8904`

## Context & Status

The installed CLI reports memd `1.6.1`, and GitHub lists `1.6.1` as the latest published release. The CLI cannot open the active store because page 1 of `~/.memd/data/metadata.db` has an invalid SQLite header. The store is on an unsupported NFS v3 mount. No store repair or replacement was performed.

## Technical Implementation

### Work Completed

- Reproduced the failure with `memd memory-md` and `memd doctor`.
- Checked the active database header, read-only SQLite integrity, NFS mount, worker state, and worker error history.
- Validated the preserved 2026-07-11 repair database with `PRAGMA quick_check`.
- Proved the installed CLI can add, search, and pass SQLite integrity checks in a temporary node-local ext4 store.
- Recorded commands and results in `tasks/METHODS.md`.

### Outcomes

- **What worked:** memd `1.6.1` passed add, search, and `PRAGMA quick_check` on node-local ext4. The preserved repair database also passed `PRAGMA quick_check` and contains 2,197 chunk rows.
- **What didn't:** the active NFS database starts with zero bytes instead of the SQLite header. The warm-worker log recorded `disk I/O error` before `file is not a database`.

### File Map

| File | Change | Notes |
|------|--------|-------|
| `tasks/todo.md` | Modified | Records completed diagnosis and the approval-gated recovery task. |
| `tasks/METHODS.md` | Modified | Records exact checks and results. |
| `docs/handoffs/2026-08-02_memd-nfs-store-diagnosis.md` | Added | Resumption context for store recovery. |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Do not replace the active store during diagnosis. | Replacement could discard recoverable records and needs Chef's approval. |
| Treat NFS as the infrastructure cause to remove before recovery. | memd documents live NFS data directories as unsupported, and the mount disables network locking. |
| Recover into a new node-local store first. | Validation must precede any cutover from the damaged store. |

## Knowledge Capture

### Lessons Learned

- The version is current. Updating the binary will not repair the damaged SQLite header.
- A node-local test separates binary health from shared-store damage.
- DELETE journal mode can remove SQLite WAL shared-memory use, but it cannot make `flock` reliable on this NFS mount.

### Gotchas

- The active database has SQLite B-tree page markers after its zeroed header. Do not overwrite it before attempting recovery from a copy.
- The branch has pre-existing uncommitted edits in `crates/memd/src/cli/warm.rs` and `crates/memd/src/store/metadata/pool.rs`. They are not present in the installed binary.
- `memd add` cannot record this handoff because the active store cannot open.

## Moving Forward

### Next Steps

1. Get Chef's approval for the recovery and cutover scope.
2. Preserve the active store, recover or restore into a new node-local store, and validate row counts, `PRAGMA integrity_check`, memd audit, retrieval, and writes.
3. Configure memd so no live SQLite store or writer lock resides on NFS. Use OMF export/import for transfer between machines.

### Blockers

- Chef's approval is required before replacing or retiring the active store.
- The current shared storage location is unsupported for a live memd data directory.
