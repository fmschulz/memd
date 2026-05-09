"""Tenant-aware feature flag service."""

from __future__ import annotations

from authz import can_read
from cache import Cache, flag_cache_key


def get_feature_flag(
    user: dict,
    tenant_id: str,
    project_id: str,
    flag_name: str,
    store,
    cache: Cache,
) -> bool:
    """Return a feature flag visible to a user."""

    if not can_read(user, tenant_id):
        raise PermissionError(f"user cannot read tenant {tenant_id}")

    key = flag_cache_key(tenant_id, project_id, flag_name)
    cached = cache.get(key)
    if cached is not None:
        return bool(cached)

    value = store.read_flag(tenant_id, project_id, flag_name)
    cache.set(key, value)
    return value
