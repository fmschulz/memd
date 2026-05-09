from __future__ import annotations

import unittest

from cache import Cache, flag_cache_key
from feature_flags import get_feature_flag
from flag_store import FlagStore


class CacheKeyScopeTests(unittest.TestCase):
    def test_cache_is_scoped_by_tenant(self) -> None:
        store = FlagStore(
            {
                ("tenant-a", "project-1", "beta"): True,
                ("tenant-b", "project-1", "beta"): False,
            }
        )
        cache = Cache()
        user = {"tenants": ["tenant-a", "tenant-b"]}

        self.assertTrue(get_feature_flag(user, "tenant-a", "project-1", "beta", store, cache))
        self.assertFalse(get_feature_flag(user, "tenant-b", "project-1", "beta", store, cache))

    def test_project_scope_is_still_distinct_inside_tenant(self) -> None:
        store = FlagStore(
            {
                ("tenant-a", "project-1", "beta"): True,
                ("tenant-a", "project-2", "beta"): False,
            }
        )
        cache = Cache()
        user = {"tenants": ["tenant-a"]}

        self.assertTrue(get_feature_flag(user, "tenant-a", "project-1", "beta", store, cache))
        self.assertFalse(get_feature_flag(user, "tenant-a", "project-2", "beta", store, cache))

    def test_unauthorized_user_does_not_populate_cache(self) -> None:
        store = FlagStore({("tenant-a", "project-1", "beta"): True})
        cache = Cache()

        with self.assertRaises(PermissionError):
            get_feature_flag({"tenants": []}, "tenant-a", "project-1", "beta", store, cache)

        self.assertEqual(cache.values, {})

    def test_cache_key_shape_contains_scope_parts(self) -> None:
        key = flag_cache_key("tenant-a", "project-1", "beta")

        self.assertIn("tenant-a", key)
        self.assertIn("project-1", key)
        self.assertIn("beta", key)


if __name__ == "__main__":
    unittest.main()
