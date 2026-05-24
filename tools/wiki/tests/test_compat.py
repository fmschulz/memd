from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.compat import (  # noqa: E402
    ServerIncompatibleError,
    check_server_compat,
)


class CheckServerCompatTests(unittest.TestCase):
    def test_exact_match_is_ok(self) -> None:
        result = check_server_compat("0.9.0", "0.9.0")
        self.assertEqual(result.severity, "ok")

    def test_patch_skew_is_warn(self) -> None:
        result = check_server_compat("0.9.3", "0.9.0")
        self.assertEqual(result.severity, "warn")
        self.assertIn("patch-level", result.message)

    def test_minor_mismatch_is_fail(self) -> None:
        result = check_server_compat("0.8.0", "0.9.0")
        self.assertEqual(result.severity, "fail")
        self.assertIn("MAJOR.MINOR", result.message)

    def test_major_mismatch_is_fail(self) -> None:
        result = check_server_compat("1.0.0", "0.9.0")
        self.assertEqual(result.severity, "fail")

    def test_0_10_vs_0_1_is_fail_not_ok(self) -> None:
        """String-prefix compare would treat 0.1.x and 0.10.x as matching."""
        result = check_server_compat("0.10.0", "0.1.0")
        self.assertEqual(result.severity, "fail")

    def test_unreported_server_version_is_warn(self) -> None:
        result = check_server_compat(None, "0.9.0")
        self.assertEqual(result.severity, "warn")
        self.assertIn("did not report", result.message)

    def test_unparseable_server_version_is_warn(self) -> None:
        result = check_server_compat("not-a-version", "0.9.0")
        self.assertEqual(result.severity, "warn")
        self.assertIn("could not parse memd executable", result.message)

    def test_unparseable_client_version_is_warn(self) -> None:
        # Defensive: we shouldn't ship an unparseable __version__, but if
        # someone does, we must not hard-fail callers.
        result = check_server_compat("0.9.0", "weird-dev")
        self.assertEqual(result.severity, "warn")
        self.assertIn("memd-wiki __version__", result.message)

    def test_prerelease_suffix_is_ignored_for_match(self) -> None:
        result = check_server_compat("0.9.0-rc1", "0.9.0")
        # Same MAJOR.MINOR.PATCH after suffix strip → ok, not warn.
        self.assertEqual(result.severity, "ok")


class ServerIncompatibleErrorTests(unittest.TestCase):
    def test_carries_both_versions(self) -> None:
        err = ServerIncompatibleError(
            client_version="0.9.0", server_version="0.8.0"
        )
        self.assertEqual(err.client_version, "0.9.0")
        self.assertEqual(err.server_version, "0.8.0")
        self.assertIn("0.9.0", str(err))
        self.assertIn("0.8.0", str(err))


if __name__ == "__main__":
    unittest.main()
