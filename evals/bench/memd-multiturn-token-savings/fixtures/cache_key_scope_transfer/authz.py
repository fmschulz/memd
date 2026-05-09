"""Authorization helpers."""

from __future__ import annotations


def can_read(user: dict, tenant_id: str) -> bool:
    return tenant_id in user.get("tenants", [])
