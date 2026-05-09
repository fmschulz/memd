"""Log formatting helpers."""

from __future__ import annotations


def format_record(record: dict) -> str:
    return f"{record['ts']} {record['level']} {record['message']}\n"
