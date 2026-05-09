"""Output formatting helpers for the dispatch export."""

from __future__ import annotations

from datetime import datetime


def utc_z(dt: datetime) -> str:
    """Render a UTC datetime with the downstream contract's Z suffix."""

    return dt.isoformat().replace("+00:00", "Z")


def comma_join(values: list[str]) -> str:
    """Render a deterministic CSV-ish audit field."""

    return ",".join(sorted(values))
