"""Customer ingest helpers."""

from __future__ import annotations


def insert_customer(table, customer_id: str, spend: int, tier: str = "standard") -> None:
    table.insert({"customer_id": customer_id, "spend": spend, "tier": tier})
