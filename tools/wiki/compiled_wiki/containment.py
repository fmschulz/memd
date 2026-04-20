"""Outdir containment guard for memd-wiki.

Ports the full refusal rules from the Rust ``memd export-markdown``
reference at ``crates/memd/src/cli.rs``:

1. Reject if the normalized ``outdir`` is inside any memd *data
   directory*. When an explicit data dir is supplied (e.g. CLI
   ``--data-dir``), the list is just that path. Otherwise the list is
   ``[<discovered via .memd/tenant_scope.json>?,  $HOME/.memd/data]``
   — discovery AUGMENTS the default, it does not replace it, so an
   untrusted ancestor config cannot mask the default-install guard.

2. Scope discovery walks ancestors looking for
   ``.memd/tenant_scope.json``. The first hit is the boundary: a
   missing / malformed / ``data_dir``-less scope file stops the walk
   and returns ``None`` rather than falling through to an outer
   project's config.

3. Reject if any pre-existing symlink component sits BELOW the outdir
   root along the path to a file we are about to write. The outdir
   itself may be a symlink — the user may legitimately point the CLI
   at a symlinked directory they own — but anything inside it that
   predates the write must be a regular file or directory. Non-existing
   components are fine (``create_dir_all`` will mkdir them). Other
   ``OSError``s (permission denied, ELOOP, etc.) fail closed rather
   than silently skipping the guard.

Path comparison is case-sensitive on POSIX and case-insensitive on
Windows, matching the Rust ``path_is_inside`` semantics.

Normalization is textual (``pathlib`` component walk over an
absolutized path). We deliberately do NOT use ``Path.resolve()``, which
on some platforms follows symlinks and/or errors on missing
components: the guard must run BEFORE the outdir tree exists.
"""

from __future__ import annotations

import json
import os
import stat
import sys
from pathlib import Path, PurePath


class OutdirContainmentError(ValueError):
    """Raised when an outdir violates the export-markdown containment rules.

    Carries both the offending outdir and the reason; the CLI turns this
    into an exit-code-2 diagnostic.
    """

    def __init__(self, outdir: Path, reason: str) -> None:
        super().__init__(f"outdir {outdir} rejected: {reason}")
        self.outdir = outdir
        self.reason = reason


def normalize_absolute(path: Path, *, cwd: Path | None = None) -> Path:
    """Textually absolutize ``path`` and collapse ``.`` / ``..`` segments.

    Does not call ``Path.resolve()``; the result need not correspond to
    a path that exists on disk. ``cwd`` overrides ``os.getcwd()`` for
    relative inputs (used by tests).
    """
    absolute: Path
    if path.is_absolute():
        absolute = path
    else:
        base = cwd if cwd is not None else Path(os.getcwd())
        absolute = base / path

    parts: list[str] = []
    root = ""
    for i, part in enumerate(absolute.parts):
        if i == 0 and (
            part == os.sep
            or part.endswith(":\\")
            or part.endswith(":/")
            or part == "/"
        ):
            root = part
            continue
        if part in ("", "."):
            continue
        if part == "..":
            if parts:
                parts.pop()
            continue
        parts.append(part)
    if root:
        return Path(root, *parts) if parts else Path(root)
    return Path(*parts) if parts else Path()


def path_is_inside(child: Path, parent: Path) -> bool:
    """Return True if ``child`` is the same as ``parent`` or a descendant.

    Both inputs MUST already be normalized (see ``normalize_absolute``).
    Case-insensitive on Windows, case-sensitive elsewhere.
    """
    child_parts: tuple[str, ...] = child.parts
    parent_parts: tuple[str, ...] = parent.parts

    if sys.platform == "win32":
        child_parts = tuple(p.lower() for p in child_parts)
        parent_parts = tuple(p.lower() for p in parent_parts)

    if len(child_parts) < len(parent_parts):
        return False
    return child_parts[: len(parent_parts)] == parent_parts


def discover_project_data_dir_from(start: Path) -> Path | None:
    """Walk ancestors of ``start`` looking for ``.memd/tenant_scope.json``.

    Returns the ``data_dir`` value from the first hit (absolute if the
    file gave an absolute path, otherwise joined against the project
    root that contains ``.memd/``). Returns ``None`` if the first hit
    is unreadable, malformed, or missing ``data_dir`` — matching the
    Rust contract of first-match-wins-or-fail-stopped.
    """
    current: Path | None = start
    while current is not None:
        scope_path = current / ".memd" / "tenant_scope.json"
        if scope_path.is_file():
            try:
                raw = scope_path.read_text(encoding="utf-8")
                parsed = json.loads(raw)
            except (OSError, json.JSONDecodeError):
                return None
            if not isinstance(parsed, dict):
                return None
            data_dir_raw = parsed.get("data_dir")
            if not isinstance(data_dir_raw, str) or not data_dir_raw.strip():
                return None
            candidate = Path(data_dir_raw)
            if candidate.is_absolute():
                return candidate
            return current / candidate
        parent = current.parent
        if parent == current:
            return None
        current = parent
    return None


def resolve_forbidden_data_dirs(
    explicit: Path | None,
    start: Path | None,
    home: Path | None = None,
) -> list[Path]:
    """Return the list of data dirs an outdir must not be inside of.

    Semantics (ported verbatim from
    ``resolve_export_markdown_data_dirs_from``):

    - When ``explicit`` is given, the returned list is ``[explicit]``.
      The caller's declared intent overrides both discovery and the
      home default.
    - Otherwise the list includes the discovered project data dir
      (if any) AND the home default ``$HOME/.memd/data``. An untrusted
      ancestor scope file cannot mask the default-install guard.

    The list is deduplicated while preserving order (discovered first,
    then home default) so tests and diagnostics are stable.
    """
    if explicit is not None:
        return [explicit]

    candidates: list[Path] = []
    if start is not None:
        discovered = discover_project_data_dir_from(start)
        if discovered is not None:
            candidates.append(discovered)

    home_dir = home if home is not None else Path.home()
    home_default = home_dir / ".memd" / "data"

    if home_default not in candidates:
        candidates.append(home_default)

    return candidates


def check_outdir_containment(
    outdir: Path,
    forbidden_data_dirs: list[Path],
    *,
    cwd: Path | None = None,
) -> Path:
    """Refuse ``outdir`` if it is inside any of ``forbidden_data_dirs``.

    Returns the normalized absolute form of ``outdir`` on success so
    callers can reuse it for the symlink walk. Raises
    ``OutdirContainmentError`` on refusal.
    """
    outdir_abs = normalize_absolute(outdir, cwd=cwd)
    for raw in forbidden_data_dirs:
        data_abs = normalize_absolute(raw, cwd=cwd)
        if path_is_inside(outdir_abs, data_abs):
            raise OutdirContainmentError(
                outdir_abs,
                f"inside memd data directory {data_abs}",
            )
    return outdir_abs


def reject_if_any_symlink_inside_outdir(
    full_target: Path,
    outdir_abs: Path,
) -> None:
    """Refuse if any pre-existing symlink sits between ``outdir_abs`` and ``full_target``.

    Walks each already-existing path component under ``outdir_abs``.
    - A symlink component → raise ``OutdirContainmentError``.
    - A non-symlink component → continue walking.
    - A not-yet-existing component → stop (``create_dir_all`` will
      create it cleanly).
    - Any other ``OSError`` (permission denied, ELOOP, etc.) → raise
      rather than silently skip (fail-closed).

    The outdir itself is NOT checked (by design — the user may point
    the CLI at a symlinked directory they own). Only components strictly
    below ``outdir_abs`` are inspected.

    ``full_target`` MUST be under ``outdir_abs`` (normalized) or this
    raises ``OutdirContainmentError`` defensively. Both paths are
    normalized internally so callers can pass relative or unnormalized
    inputs without spurious refusals.
    """
    normalized_outdir = normalize_absolute(Path(outdir_abs))
    normalized_target = normalize_absolute(Path(full_target))
    try:
        relative = PurePath(normalized_target).relative_to(
            PurePath(normalized_outdir)
        )
    except ValueError as exc:
        raise OutdirContainmentError(
            normalized_outdir,
            f"internal: target {full_target} not inside outdir {normalized_outdir}",
        ) from exc

    current = Path(normalized_outdir)
    for segment in relative.parts:
        current = current / segment
        try:
            meta = os.lstat(current)
        except FileNotFoundError:
            # Component does not exist yet — create_dir_all / write
            # will make it. Stop walking.
            return
        except OSError as exc:
            # Surface abnormal filesystem states (ELOOP, ENOTDIR,
            # permission denied, etc.) as a refusal rather than silently
            # skipping the guard. Matches Rust's fail-closed behavior
            # (cli.rs::reject_if_any_symlink_inside_outdir).
            raise OutdirContainmentError(
                normalized_outdir,
                f"cannot verify symlink status for {current}: {exc}",
            ) from exc
        if stat.S_ISLNK(meta.st_mode):
            raise OutdirContainmentError(
                normalized_outdir,
                f"refusing to follow symlink inside outdir: {current}",
            )
