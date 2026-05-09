"""Tiny table abstraction for migration tests."""

from __future__ import annotations


class Table:
    def __init__(self, rows: list[dict]):
        self.rows = [dict(row) for row in rows]
        self.required_columns: set[str] = set()

    def require(self, column: str) -> None:
        self.required_columns.add(column)

    def insert(self, row: dict) -> None:
        missing = [column for column in self.required_columns if column not in row]
        if missing:
            raise ValueError(f"missing required columns: {missing}")
        self.rows.append(dict(row))
