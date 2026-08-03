# Handoff: memd 1.6.1 Dori Recovery

**Date:** 2026-08-03

**Branch:** `fix/nfs-safe-runtime-recovery`

**Product fix:** `ded9de4f04f8b9226c90704e14419ec72f6243e7`

## Status

memd `1.6.1` now runs on Dori's glibc 2.28 and uses a healthy node-local store on `ln009`. The product fix and its diagnosis handoff are on `origin/main`. Docs, Test, and Retrieval Gate passed for the product fix.

The installed binary matches the exact pushed-source build. Its SHA-256 is `4216e646e775d1762d0aadd7242cefb1fb4f33b25c6966699d33d1e5fd1ba3a6`.

## Implementation

- Replaced direct `libc::close_range` linkage with `libc::syscall(SYS_close_range, ...)` on Linux GNU targets. The kernel fallbacks still handle `ENOSYS` and `EINVAL`.
- Added a CI check that rejects a built memd binary with a dynamic `close_range` import.
- Installed the pushed build at `~/.local/bin/memd`.
- Set `~/.config/memd/config.toml` to use `/tmp/fschulz-memd/data`.
- Restored the node-local data from the valid repair snapshot without warm-worker state, locks, SQLite sidecars, or tenant application WAL files.
- Refreshed the repo's ignored memd scope files without changing global agent rules.

## Verification

- Dori Slurm job `24445194` passed formatting, targeted tests, workspace clippy and tests, strict `memd-evals` clippy, old-glibc link checks, strict MkDocs, and retrieval regression gates.
- Dori Slurm job `24446235` built the exact pushed SHA and found no dynamic `close_range` import.
- GitHub Docs run `30791227109`, Test run `30791227057`, and Retrieval Gate run `30791226639` passed.
- `memd doctor --project-dir . --format json` reports a valid binary, data directory, project scope, hooks, global rules, and warm worker.
- `memd memory-md --project-dir . --output memory.md` reports 20 metadata-active project records, 20 readable records, and 0 unreadable records.
- Cold `memd get`, warm required search, and `PRAGMA quick_check` passed before and after a warm-worker restart.
- Recovery decision `019fc669-82fb-7330-9782-561d1aacb8a9` is readable from finalized segment `1391`. The tenant application WAL is 0 bytes after recovery.

## Preserved Data

The damaged NFS store remains at `~/.memd/data`. The valid repair snapshot remains at `~/.memd/repairs/20260711T073748Z/data`. Recovery did not change either source.

## Operating Constraint

`/tmp/fschulz-memd/data` is local to the current Dori host and is erased by a reboot or local cleanup. It does not appear on another Dori login node. After a reboot or host change, stop any stale warm worker, seed a new node-local store from the repair snapshot with the same exclusions, run `memd init`, then verify `doctor`, a cold read, a warm search, worker restart recovery, and SQLite integrity.

Do not point live SQLite state or memd's writer lock back to the NFS home directory. Use export and import when a durable cross-host transfer is needed.

## Reproduction Record

Exact commands, Slurm job IDs, hashes, the rejected stale-WAL seed, and final checks are in `tasks/METHODS.md`.
