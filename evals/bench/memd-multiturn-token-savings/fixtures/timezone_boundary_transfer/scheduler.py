"""Small scheduling helpers used by the benchmark fixture."""

from __future__ import annotations

from datetime import datetime, timezone


def event_utc_iso(local_iso: str) -> str:
    """Return the UTC instant for an ISO-8601 local timestamp."""

    event_time = datetime.fromisoformat(local_iso)
    offset = event_time.utcoffset()
    if offset is not None:
        event_time = event_time - offset
    return event_time.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def reminder_utc_iso(local_iso: str, minutes_before: int) -> str:
    """Return the UTC instant for a reminder before an event."""

    event_time = datetime.fromisoformat(local_iso)
    offset = event_time.utcoffset()
    if offset is not None:
        event_time = event_time - offset
    reminder = event_time.astimezone(timezone.utc)
    reminder = reminder.replace(minute=reminder.minute - minutes_before)
    return reminder.isoformat().replace("+00:00", "Z")
