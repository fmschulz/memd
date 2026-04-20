"""Compat-gate wiring through McpHttpClient.initialize().

Mocks `urllib.request.urlopen` so the unit tests do not require binding
a loopback socket (which fails in restricted sandboxes).
"""

from __future__ import annotations

import io
import json
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.compat import ServerIncompatibleError  # noqa: E402
from compiled_wiki.mcp_client import McpHttpClient  # noqa: E402


class _FakeResponse(io.BytesIO):
    """Minimal file-like object supporting the context-manager protocol used by urlopen."""

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()


def _fake_urlopen_factory(server_version: str | None):
    def _urlopen(_request, *, timeout: float = 0.0) -> _FakeResponse:
        _ = timeout
        server_info: dict[str, str] = {"name": "memd"}
        if server_version is not None:
            server_info["version"] = server_version
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "serverInfo": server_info,
                },
            }
        ).encode("utf-8")
        return _FakeResponse(body)

    return _urlopen


class InitializeCompatWiringTests(unittest.TestCase):
    URL = "http://127.0.0.1:0/mcp"  # never actually dialed

    def _client(self, client_version: str = "0.9.0", check_compat: bool = True) -> McpHttpClient:
        return McpHttpClient(
            url=self.URL,
            client_version=client_version,
            timeout=2.0,
            check_compat=check_compat,
        )

    def test_matching_major_minor_initializes_cleanly(self) -> None:
        with patch(
            "compiled_wiki.mcp_client.urllib.request.urlopen",
            side_effect=_fake_urlopen_factory("0.9.0"),
        ):
            client = self._client(client_version="0.9.0")
            client.initialize()
            self.assertEqual(client.server_version, "0.9.0")
            assert client.compat_result is not None
            self.assertEqual(client.compat_result.severity, "ok")

    def test_minor_mismatch_raises_server_incompatible(self) -> None:
        with patch(
            "compiled_wiki.mcp_client.urllib.request.urlopen",
            side_effect=_fake_urlopen_factory("0.8.0"),
        ):
            client = self._client(client_version="0.9.0")
            with self.assertRaises(ServerIncompatibleError) as ctx:
                client.initialize()
            self.assertEqual(ctx.exception.server_version, "0.8.0")
            self.assertEqual(ctx.exception.client_version, "0.9.0")

    def test_patch_only_skew_initializes_with_warn(self) -> None:
        with patch(
            "compiled_wiki.mcp_client.urllib.request.urlopen",
            side_effect=_fake_urlopen_factory("0.9.5"),
        ):
            client = self._client(client_version="0.9.0")
            client.initialize()
            assert client.compat_result is not None
            self.assertEqual(client.compat_result.severity, "warn")

    def test_check_compat_false_disables_gate(self) -> None:
        with patch(
            "compiled_wiki.mcp_client.urllib.request.urlopen",
            side_effect=_fake_urlopen_factory("0.8.0"),
        ):
            client = self._client(client_version="0.9.0", check_compat=False)
            client.initialize()  # would raise with gate enabled
            self.assertEqual(client.server_version, "0.8.0")
            self.assertIsNone(client.compat_result)

    def test_missing_server_version_is_warn_not_fail(self) -> None:
        with patch(
            "compiled_wiki.mcp_client.urllib.request.urlopen",
            side_effect=_fake_urlopen_factory(None),
        ):
            client = self._client(client_version="0.9.0")
            client.initialize()
            self.assertIsNone(client.server_version)
            assert client.compat_result is not None
            self.assertEqual(client.compat_result.severity, "warn")


if __name__ == "__main__":
    unittest.main()
