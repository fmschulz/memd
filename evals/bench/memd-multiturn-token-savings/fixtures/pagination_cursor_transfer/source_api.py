"""Paged source API used by the backfill worker."""

from __future__ import annotations


class SourceApi:
    def __init__(self, records: list[dict]):
        self.records = sorted(records, key=lambda item: item["id"])
        self.calls: list[int | None] = []

    def page_after(self, cursor: int | None, limit: int) -> list[dict]:
        self.calls.append(cursor)
        return [
            record
            for record in self.records
            if cursor is None or record["id"] > cursor
        ][:limit]
