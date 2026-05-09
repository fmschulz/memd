"""Backfill cursor state."""

from __future__ import annotations


class SyncState:
    def __init__(self) -> None:
        self.cursor: int | None = None
        self.history: list[int | None] = []

    def set_cursor(self, cursor: int | None) -> None:
        self.cursor = cursor
        self.history.append(cursor)
