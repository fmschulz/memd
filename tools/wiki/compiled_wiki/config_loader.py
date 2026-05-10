"""Discover and parse ``.memd/config.json`` for memd-wiki.

Per the Item 7 plan §3, the fall-through priority for memd-wiki settings is:

1. CLI flags (highest)
2. ``.memd/config.json``'s ``wiki`` subsection at the nearest ancestor of
   the caller's start directory
3. ``.memd/config.json``'s top-level ``tenant_id`` / ``project_id``
4. Built-in defaults (lowest)

This module only covers layers 2 and 3 — returning what is on disk, with
missing fields left as ``None``. The CLI layer is responsible for
merging CLI args over the discovered config and filling hardcoded
defaults last.

Discovery walks from ``start`` (default: ``Path.cwd()``) toward the
filesystem root, returning the first ``.memd/config.json`` encountered.
Search stops at the first candidate regardless of whether that file
parses or is missing the ``wiki`` key — matching the Rust
``tenant_scope.json`` discovery contract used elsewhere in the repo
(a file that is present but incomplete is the caller's problem to fix,
not a reason to keep walking past it).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any


class ConfigLoadError(RuntimeError):
    """Raised when a discovered ``.memd/config.json`` exists but cannot be parsed.

    Carries the file path so the CLI can show a precise diagnostic.
    """

    def __init__(self, path: Path, reason: str) -> None:
        super().__init__(f"failed to load {path}: {reason}")
        self.path = path
        self.reason = reason


@dataclass(frozen=True)
class DiscoveredConfig:
    """Result of scanning for a ``.memd/config.json``.

    All overridable fields are optional. ``source_path`` is set whenever
    a candidate file was found, even if it yielded zero usable settings
    — the CLI can surface it in diagnostics.
    """

    source_path: Path | None
    tenant_id: str | None
    project_id: str | None
    outdir: Path | None
    max_tasks: int | None
    library_k: int | None
    memd_bin: str | None
    memd_url: str | None

    @classmethod
    def empty(cls) -> "DiscoveredConfig":
        return cls(
            source_path=None,
            tenant_id=None,
            project_id=None,
            outdir=None,
            max_tasks=None,
            library_k=None,
            memd_bin=None,
            memd_url=None,
        )


def find_config_file(start: Path) -> Path | None:
    """Return the nearest ancestor ``.memd/config.json`` at or above ``start``.

    Returns ``None`` if none exists up to the filesystem root.
    """
    current = start.resolve()
    while True:
        candidate = current / ".memd" / "config.json"
        if candidate.is_file():
            return candidate
        parent = current.parent
        if parent == current:
            return None
        current = parent


def load_config(start: Path | None = None) -> DiscoveredConfig:
    """Discover and parse the nearest ``.memd/config.json``.

    Returns ``DiscoveredConfig.empty()`` when no config file is found.
    Raises ``ConfigLoadError`` when a file IS found but cannot be parsed
    or is not a JSON object; callers should surface the diagnostic and
    let the operator fix the file rather than silently falling through.
    """
    origin = (start or Path.cwd()).resolve()
    config_path = find_config_file(origin)
    if config_path is None:
        return DiscoveredConfig.empty()

    try:
        raw = config_path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ConfigLoadError(config_path, f"could not read: {exc}") from exc

    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ConfigLoadError(config_path, f"invalid JSON: {exc}") from exc

    if not isinstance(parsed, dict):
        raise ConfigLoadError(
            config_path,
            f"top-level must be a JSON object, got {type(parsed).__name__}",
        )

    return _from_parsed_document(config_path, parsed)


def _from_parsed_document(source_path: Path, parsed: dict[str, Any]) -> DiscoveredConfig:
    tenant_id = _opt_str(parsed.get("tenant_id"))
    project_id = _opt_str(parsed.get("project_id"))

    wiki_section: dict[str, Any] = {}
    raw_wiki = parsed.get("wiki")
    if raw_wiki is not None:
        if not isinstance(raw_wiki, dict):
            raise ConfigLoadError(
                source_path,
                f"`wiki` must be an object, got {type(raw_wiki).__name__}",
            )
        wiki_section = raw_wiki

    outdir_raw = _opt_str(wiki_section.get("outdir"))
    outdir = _resolve_outdir(source_path, outdir_raw)

    max_tasks = _opt_positive_int(source_path, "max_tasks", wiki_section.get("max_tasks"))
    library_k = _opt_positive_int(source_path, "library_k", wiki_section.get("library_k"))
    memd_bin = _opt_str(wiki_section.get("memd_bin"))
    memd_url = _opt_str(wiki_section.get("memd_url"))

    return DiscoveredConfig(
        source_path=source_path,
        tenant_id=tenant_id,
        project_id=project_id,
        outdir=outdir,
        max_tasks=max_tasks,
        library_k=library_k,
        memd_bin=memd_bin,
        memd_url=memd_url,
    )


def _opt_str(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip():
        return None
    return value


def _opt_positive_int(source_path: Path, key: str, value: Any) -> int | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise ConfigLoadError(
            source_path,
            f"`wiki.{key}` must be a positive integer, got {type(value).__name__}",
        )
    if value <= 0:
        raise ConfigLoadError(
            source_path,
            f"`wiki.{key}` must be a positive integer, got {value}",
        )
    return value


def _resolve_outdir(source_path: Path, raw: str | None) -> Path | None:
    if raw is None:
        return None
    candidate = Path(raw)
    if candidate.is_absolute():
        return candidate
    # Relative outdir: resolve against the directory containing the
    # ``.memd/`` marker — i.e. the project root the config belongs to —
    # not the caller's CWD. This makes config-declared paths stable
    # regardless of where the CLI is invoked from.
    project_root = source_path.parent.parent
    return (project_root / candidate)
