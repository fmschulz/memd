"""Small process-local cache."""

from __future__ import annotations


class Cache:
    def __init__(self) -> None:
        self.values: dict[str, object] = {}

    def get(self, key: str) -> object | None:
        return self.values.get(key)

    def set(self, key: str, value: object) -> None:
        self.values[key] = value


def flag_cache_key(tenant_id: str, project_id: str, flag_name: str) -> str:
    """Return the cache key for one tenant/project flag lookup."""

    return f"{project_id}:{flag_name}"
