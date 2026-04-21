"""Integration + route-resolver tests for ``memd-wiki serve``.

Unit tests cover the pure ``resolve_route`` function (no socket
binding); integration tests bind an ephemeral localhost port, talk to
it via ``http.client``, and assert end-to-end HTTP behavior.

Zero third-party deps: stdlib ``unittest`` + ``http.client`` only, to
stay inside the wiki tool's zero-dependency constraint.
"""

from __future__ import annotations

import http.client
import os
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.server_app import (  # noqa: E402
    HTML_CONTENT_TYPE,
    JSON_CONTENT_TYPE,
    PLAIN_CONTENT_TYPE,
    make_handler,
    resolve_route,
)


def _seed_full_tree(root: Path) -> None:
    """Write the minimal file tree the P2 route table expects."""
    (root / "index.md").write_text("# Fixture Wiki\n\nHello from P2.\n", encoding="utf-8")
    (root / "log.md").write_text("# Log\n\n- entry 1\n", encoding="utf-8")
    (root / "manifest.json").write_text(
        '{"schema_version": 2, "tenant_id": "t", "project_id": "p"}\n',
        encoding="utf-8",
    )
    (root / "concepts").mkdir()
    (root / "concepts" / "019dadab-abc.md").write_text(
        "---\nartifact_id: 019dadab-abc\n---\n\n# Concept Fixture\n",
        encoding="utf-8",
    )
    (root / "entities").mkdir()
    (root / "entities" / "019dadab-ent.md").write_text(
        "# Entity Fixture\n", encoding="utf-8"
    )
    (root / "tasks").mkdir()
    (root / "tasks" / "019dadab-task.md").write_text(
        "# Task Fixture\n", encoding="utf-8"
    )
    (root / "projects").mkdir()
    (root / "projects" / "memd.md").write_text("# Project memd\n", encoding="utf-8")
    (root / "libraries").mkdir()
    (root / "libraries" / "failures.md").write_text(
        "# Failures Library\n", encoding="utf-8"
    )
    (root / "notes").mkdir()
    (root / "notes" / "my-note.md").write_text("# Hand Note\n", encoding="utf-8")
    # Nested note to exercise multi-segment routing in the notes lane.
    (root / "notes" / "sub").mkdir()
    (root / "notes" / "sub" / "deep.md").write_text(
        "# Deep Note\n", encoding="utf-8"
    )


class ResolveRouteTests(unittest.TestCase):
    """Pure-function tests for the expanded P2 route table."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.outdir = Path(self._tmp.name)
        _seed_full_tree(self.outdir)

    def test_root_resolves_to_index_md(self) -> None:
        route = resolve_route(self.outdir, "/")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "index.md")
        self.assertTrue(route.content_type.startswith("text/html"))

    def test_empty_path_resolves_to_index(self) -> None:
        route = resolve_route(self.outdir, "")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "index.md")

    def test_unknown_path_is_404_as_text_plain(self) -> None:
        route = resolve_route(self.outdir, "/nope")
        self.assertEqual(route.status, 404)
        self.assertIsNone(route.file_path)
        self.assertTrue(route.content_type.startswith("text/plain"))

    def test_root_without_index_is_404(self) -> None:
        (self.outdir / "index.md").unlink()
        route = resolve_route(self.outdir, "/")
        self.assertEqual(route.status, 404)

    def test_log_route_with_and_without_trailing_slash(self) -> None:
        for path in ("/log", "/log/"):
            route = resolve_route(self.outdir, path)
            self.assertEqual(route.status, 200, f"path={path}")
            self.assertEqual(route.file_path, self.outdir / "log.md")
            self.assertEqual(route.content_type, HTML_CONTENT_TYPE)

    def test_manifest_route_returns_json_content_type(self) -> None:
        route = resolve_route(self.outdir, "/manifest.json")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "manifest.json")
        self.assertEqual(route.content_type, JSON_CONTENT_TYPE)

    def test_concept_route(self) -> None:
        for path in ("/concepts/019dadab-abc", "/concepts/019dadab-abc/"):
            route = resolve_route(self.outdir, path)
            self.assertEqual(route.status, 200, f"path={path}")
            self.assertEqual(
                route.file_path,
                self.outdir / "concepts" / "019dadab-abc.md",
            )
            self.assertEqual(route.content_type, HTML_CONTENT_TYPE)

    def test_entity_route(self) -> None:
        route = resolve_route(self.outdir, "/entities/019dadab-ent")
        self.assertEqual(route.status, 200)
        self.assertEqual(
            route.file_path, self.outdir / "entities" / "019dadab-ent.md"
        )

    def test_task_route(self) -> None:
        route = resolve_route(self.outdir, "/tasks/019dadab-task")
        self.assertEqual(route.status, 200)
        self.assertEqual(
            route.file_path, self.outdir / "tasks" / "019dadab-task.md"
        )

    def test_project_route(self) -> None:
        route = resolve_route(self.outdir, "/projects/memd")
        self.assertEqual(route.status, 200)
        self.assertEqual(route.file_path, self.outdir / "projects" / "memd.md")

    def test_library_route(self) -> None:
        route = resolve_route(self.outdir, "/libraries/failures")
        self.assertEqual(route.status, 200)
        self.assertEqual(
            route.file_path, self.outdir / "libraries" / "failures.md"
        )

    def test_notes_single_segment(self) -> None:
        route = resolve_route(self.outdir, "/notes/my-note")
        self.assertEqual(route.status, 200)
        self.assertEqual(
            route.file_path, self.outdir / "notes" / "my-note.md"
        )

    def test_notes_multi_segment(self) -> None:
        route = resolve_route(self.outdir, "/notes/sub/deep")
        self.assertEqual(route.status, 200)
        self.assertEqual(
            route.file_path, self.outdir / "notes" / "sub" / "deep.md"
        )

    def test_missing_file_under_valid_route_is_404(self) -> None:
        route = resolve_route(self.outdir, "/tasks/does-not-exist")
        self.assertEqual(route.status, 404)

    def test_unknown_top_level_prefix_is_404(self) -> None:
        route = resolve_route(self.outdir, "/unknown/foo")
        self.assertEqual(route.status, 404)

    def test_directory_route_without_leaf_is_404(self) -> None:
        # ``/concepts/`` would map to ``concepts.md`` which doesn't exist.
        route = resolve_route(self.outdir, "/concepts/")
        self.assertEqual(route.status, 404)


class RouteSafetyTests(unittest.TestCase):
    """Negative tests for traversal, symlink, and invalid-character inputs."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.outdir = Path(self._tmp.name)
        _seed_full_tree(self.outdir)

    def test_dotdot_segment_is_404(self) -> None:
        route = resolve_route(self.outdir, "/concepts/../etc/passwd")
        self.assertEqual(route.status, 404)

    def test_single_dot_segment_is_404(self) -> None:
        route = resolve_route(self.outdir, "/concepts/./foo")
        self.assertEqual(route.status, 404)

    def test_percent_encoded_traversal_fails_char_allowlist(self) -> None:
        # ``http.server`` does not URL-decode ``self.path``, so the
        # literal ``%2e`` characters reach the resolver and fail the
        # segment regex.
        route = resolve_route(self.outdir, "/concepts/%2e%2e/etc/passwd")
        self.assertEqual(route.status, 404)

    def test_null_byte_in_segment_is_404(self) -> None:
        route = resolve_route(self.outdir, "/concepts/abc\x00bad")
        self.assertEqual(route.status, 404)

    def test_space_in_segment_is_404(self) -> None:
        route = resolve_route(self.outdir, "/concepts/has space")
        self.assertEqual(route.status, 404)

    def test_symlink_component_below_outdir_is_refused(self) -> None:
        # Replace concepts/ with a symlink that targets an outside dir.
        victim = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: _rmtree(victim))
        (victim / "leak.md").write_text("secret\n", encoding="utf-8")
        _rmtree(self.outdir / "concepts")
        os.symlink(victim, self.outdir / "concepts")
        route = resolve_route(self.outdir, "/concepts/leak")
        self.assertEqual(route.status, 404)

    def test_symlink_file_inside_outdir_is_refused(self) -> None:
        # A file-level symlink inside outdir must also be refused —
        # pre-existing symlinks are not served even when their target
        # happens to resolve inside outdir.
        victim = self.outdir / "concepts" / "legit.md"
        victim.write_text("legit\n", encoding="utf-8")
        alias = self.outdir / "concepts" / "alias.md"
        os.symlink(victim, alias)
        route = resolve_route(self.outdir, "/concepts/alias")
        self.assertEqual(route.status, 404)


def _rmtree(path: Path) -> None:
    """Minimal ``shutil.rmtree``-equivalent to avoid an extra import."""
    import shutil

    shutil.rmtree(path, ignore_errors=True)


class ServeIntegrationTests(unittest.TestCase):
    """Boots ``ThreadingHTTPServer`` on an ephemeral port end-to-end."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.outdir = Path(self._tmp.name)
        _seed_full_tree(self.outdir)
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

    def test_root_returns_index_md_body_as_html(self) -> None:
        status, ctype, body = self._get("/")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/html"))
        self.assertIn(b"<!DOCTYPE html>", body)
        self.assertIn(b"<h1>Fixture Wiki</h1>", body)
        self.assertIn(b"Hello from P2", body)
        self.assertIn(b"<style>", body)

    def test_concept_route_renders_as_html(self) -> None:
        status, ctype, body = self._get("/concepts/019dadab-abc/")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/html"))
        self.assertIn(b"<h1>Concept Fixture</h1>", body)
        # Frontmatter renders as a metadata pre block.
        self.assertIn(b'class="frontmatter"', body)

    def test_manifest_served_raw_as_json(self) -> None:
        status, ctype, body = self._get("/manifest.json")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("application/json"))
        self.assertIn(b'"schema_version": 2', body)
        # Raw bytes — no HTML wrapper.
        self.assertNotIn(b"<!DOCTYPE html>", body)

    def test_log_returns_rendered_html(self) -> None:
        status, ctype, body = self._get("/log")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/html"))
        self.assertIn(b"<h1>Log</h1>", body)

    def test_notes_multi_segment_integration(self) -> None:
        status, ctype, body = self._get("/notes/sub/deep")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/html"))
        self.assertIn(b"<h1>Deep Note</h1>", body)

    def test_traversal_returns_404_text_plain(self) -> None:
        status, ctype, body = self._get("/concepts/../etc/passwd")
        self.assertEqual(status, 404)
        self.assertTrue(ctype.startswith("text/plain"))
        self.assertIn(b"not found", body)

    def test_unknown_path_returns_404_as_text_plain(self) -> None:
        status, ctype, body = self._get("/does-not-exist")
        self.assertEqual(status, 404)
        self.assertTrue(ctype.startswith("text/plain"))
        self.assertIn(b"not found", body)

    def test_query_string_is_stripped_before_routing(self) -> None:
        status, ctype, body = self._get("/?cachebust=1")
        self.assertEqual(status, 200)
        self.assertTrue(ctype.startswith("text/html"))
        self.assertIn(b"<h1>Fixture Wiki</h1>", body)


if __name__ == "__main__":
    unittest.main()
