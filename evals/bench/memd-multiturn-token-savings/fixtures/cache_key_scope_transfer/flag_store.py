"""Feature flag backing store."""

from __future__ import annotations


class FlagStore:
    def __init__(self, rows: dict[tuple[str, str, str], bool]):
        self.rows = dict(rows)
        self.reads: list[tuple[str, str, str]] = []

    def read_flag(self, tenant_id: str, project_id: str, flag_name: str) -> bool:
        key = (tenant_id, project_id, flag_name)
        self.reads.append(key)
        return self.rows.get(key, False)
