"""Log filtering helpers."""

from __future__ import annotations


def should_export(record: dict) -> bool:
    return record.get("level") in {"INFO", "WARN", "ERROR"}
