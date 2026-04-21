"""Integration smoke tests for ``memd-wiki serve`` (P0).

Binds an ephemeral localhost port, talks to it via ``http.client``,
asserts a few baseline invariants: the root returns the compiled
``index.md`` as ``text/plain``, unknown paths 404.

Zero third-party deps: stdlib ``unittest`` + ``http.client`` only, to
stay inside the wiki tool's zero-dependency constraint.
"""

from __future__ import annotations

import http.client
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.server_app import make_handler, resolve_route  # noqa: E402


class ResolveRouteTests(unittest.TestCase):
    """Pure-function tests for the P0 route table."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.outdir = Path(self._tmp.name)
        (self.outdir / "index.md").write_text("# Hello\n", encoding="utf-8")

    def test_root_resolves_to_index_md(self) -> None:
        route = resolve_route(self.outdir, "/")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "index.md")
        self.assertTrue(route.content_type.startswith("text/plain"))

    def test_empty_path_resolves_to_index(self) -> None:
        route = resolve_route(self.outdir, "")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "index.md")

    def test_unknown_path_is_404(self) -> None:
        route = resolve_route(self.outdir, "/nope")
        self.assertEqual(route.status, 404)
        self.assertIsNone(route.file_path)

    def test_root_without_index_is_404(self) -> None:
        (self.outdir / "index.md").unlink()
        route = resolve_route(self.outdir, "/")
        self.assertEqual(route.status, 404)


class ServeIntegrationTests(unittest.TestCase):
    """Boots ``ThreadingHTTPServer`` on an ephemeral port end-to-end."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.outdir = Path(self._tmp.name)
        (self.outdir / "index.md").write_text(
            "# Fixture Wiki\n\nHello from P0.\n", encoding="utf-8"
        )
        handler_cls = make_handler(self.outdir, quiet=True)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler_cls)
        self.host, self.port = self.server.server_address[:2]
        self.thread = threading.Thread(
            target=self.server.serve_forever, daemon=True
        )
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2.0)
        self.assertFalse(self.thread.is_alive())

    def _get(self, path: str) -> tuple[int, str, bytes]:
        conn = http.client.HTTPConnection(self.host, self.port, timeout=2.0)
        try:
            conn.request("GET", path)
            resp = conn.getresponse()
            body = resp.read()
            return resp.status, resp.getheader("Content-Type", ""), body
        finally:
            conn.close()

    def test_root_returns_index_md_body(self) -> None:
        status, ctype, body = self._get("/")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/plain"))
        self.assertIn(b"Fixture Wiki", body)

    def test_unknown_path_returns_404(self) -> None:
        status, _ctype, body = self._get("/does-not-exist")
        self.assertEqual(status, 404)
        self.assertIn(b"not found", body)

    def test_query_string_is_stripped_before_routing(self) -> None:
        status, _ctype, body = self._get("/?cachebust=1")
        self.assertEqual(status, 200)
        self.assertIn(b"Fixture Wiki", body)


if __name__ == "__main__":
    unittest.main()
