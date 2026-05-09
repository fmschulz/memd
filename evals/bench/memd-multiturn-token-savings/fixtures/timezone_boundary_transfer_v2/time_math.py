"""Shared time math for dispatch scheduling."""

from __future__ import annotations

from datetime import datetime, timezone


def parse_client_instant(local_iso: str) -> datetime:
    """Return the UTC instant represented by an offset-bearing ISO timestamp."""

    parsed = datetime.fromisoformat(local_iso)
    offset = parsed.utcoffset()
    if offset is not None:
        parsed = parsed - offset
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def minutes_before(local_iso: str, minutes: int) -> datetime:
    """Return the UTC instant for a reminder before the requested start."""

    start = parse_client_instant(local_iso)
    return start.replace(minute=start.minute - minutes)
