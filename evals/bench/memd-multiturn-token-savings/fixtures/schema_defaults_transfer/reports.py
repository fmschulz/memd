"""Reporting helpers."""

from __future__ import annotations


def revenue_by_tier(table) -> dict[str, int]:
    totals: dict[str, int] = {}
    for row in table.rows:
        tier = row["tier"]
        totals[tier] = totals.get(tier, 0) + row["spend"]
    return totals


def high_value_customers(table, threshold: int = 100) -> list[str]:
    return sorted(row["customer_id"] for row in table.rows if row["spend"] >= threshold)
