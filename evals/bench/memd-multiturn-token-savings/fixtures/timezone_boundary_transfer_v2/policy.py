"""Local wall-clock policy checks for dispatch jobs."""

from __future__ import annotations

from datetime import datetime


def local_hhmm(local_iso: str) -> str:
    """Return the local clock component from an offset-bearing ISO timestamp."""

    return datetime.fromisoformat(local_iso).strftime("%H:%M")


def in_blackout_window(local_iso: str, windows: list[tuple[str, str]]) -> bool:
    """Return whether local wall time falls inside any configured window."""

    clock = local_hhmm(local_iso)
    return any(start <= clock < end for start, end in windows)


def policy_status(local_iso: str, windows: list[tuple[str, str]]) -> str:
    """Return the dispatch policy status for the local requested time."""

    return "blocked" if in_blackout_window(local_iso, windows) else "allowed"
