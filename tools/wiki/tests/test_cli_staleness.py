"""CLI-side tests for the --check-staleness lint flag wiring.

The full lint engine logic lives in ``test_lint.py``; this file focuses
on ``_build_staleness_lookup`` — the closure that wraps ``McpHttpClient``
and is the only bridge between the CLI and the memd oracle.
"""

from __future__ import annotations

import argparse
import io
import sys
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.cli import (  # noqa: E402
    DEFAULT_MEMD_URL,
    DEFAULT_TIMEOUT,
    _build_staleness_lookup,
)
from compiled_wiki.config_loader import DiscoveredConfig  # noqa: E402


def _args(**overrides: object) -> argparse.Namespace:
    base = dict(
        tenant_id=None,
        project_id=None,
        output_dir=None,
        config_start=None,
        memd_url=None,
        timeout=DEFAULT_TIMEOUT,
        check_staleness=True,
    )
    base.update(overrides)
    return argparse.Namespace(**base)


def _discovered(**overrides: object) -> DiscoveredConfig:
    base = dict(
        source_path=None,
        tenant_id=None,
        project_id=None,
        outdir=None,
        max_tasks=None,
        library_k=None,
        memd_url=None,
    )
    base.update(overrides)
    return DiscoveredConfig(**base)


class _FakeClient:
    def __init__(self, *, init_raises: Exception | None = None, responses=None, call_raises=None):
        self._init_raises = init_raises
        self._responses = responses or {}
        self._call_raises = call_raises or {}
        self.calls: list[tuple[str, dict]] = []

    def initialize(self) -> None:
        if self._init_raises is not None:
            raise self._init_raises

    def call_tool(self, name: str, arguments: dict):
        self.calls.append((name, arguments))
        task_id = arguments.get("task_id")
        if task_id in self._call_raises:
            raise self._call_raises[task_id]
        return self._responses.get(task_id, {"task": {}})


class BuildStalenessLookupTests(unittest.TestCase):
    def test_missing_ids_return_none_and_warn(self) -> None:
        args = _args(tenant_id=None, project_id=None)
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            lookup = _build_staleness_lookup(args, _discovered())
        self.assertIsNone(lookup("task-x"))
        self.assertIn("--check-staleness requires tenant_id", stderr.getvalue())

    def test_initialize_failure_degrades_gracefully(self) -> None:
        args = _args(tenant_id="memd", project_id="memd")
        fake = _FakeClient(init_raises=RuntimeError("daemon down"))
        stderr = io.StringIO()
        with (
            patch("compiled_wiki.mcp_client.McpHttpClient", return_value=fake),
            redirect_stderr(stderr),
        ):
            lookup = _build_staleness_lookup(args, _discovered())
        self.assertIsNone(lookup("task-x"))
        self.assertIn("could not initialize", stderr.getvalue())
        # Closure must not invoke call_tool on a failed client.
        self.assertEqual(fake.calls, [])

    def test_returns_updated_at_ms(self) -> None:
        args = _args(tenant_id="memd", project_id="memd")
        fake = _FakeClient(
            responses={
                "task-1": {"task": {"updated_at_ms": 123_000}},
                "task-2": {"task": {"finished_at_ms": 777}},
                "task-3": {"task": {"started_at_ms": 55}},
                "task-4": {"task": {}},
            }
        )
        with patch("compiled_wiki.mcp_client.McpHttpClient", return_value=fake):
            lookup = _build_staleness_lookup(args, _discovered())
        self.assertEqual(lookup("task-1"), 123_000)
        self.assertEqual(lookup("task-2"), 777)
        self.assertEqual(lookup("task-3"), 55)
        self.assertIsNone(lookup("task-4"))

    def test_results_are_cached_per_task(self) -> None:
        args = _args(tenant_id="memd", project_id="memd")
        fake = _FakeClient(responses={"task-1": {"task": {"updated_at_ms": 5}}})
        with patch("compiled_wiki.mcp_client.McpHttpClient", return_value=fake):
            lookup = _build_staleness_lookup(args, _discovered())
        self.assertEqual(lookup("task-1"), 5)
        self.assertEqual(lookup("task-1"), 5)
        self.assertEqual(
            [args for name, args in fake.calls if name == "task.resume"],
            [{"tenant_id": "memd", "project_id": "memd", "task_id": "task-1"}],
        )

    def test_call_tool_failure_disables_remaining_lookups(self) -> None:
        args = _args(tenant_id="memd", project_id="memd")
        fake = _FakeClient(
            responses={"task-good": {"task": {"updated_at_ms": 1}}},
            call_raises={"task-bad": RuntimeError("rpc boom")},
        )
        stderr = io.StringIO()
        with (
            patch("compiled_wiki.mcp_client.McpHttpClient", return_value=fake),
            redirect_stderr(stderr),
        ):
            lookup = _build_staleness_lookup(args, _discovered())
            # First successful lookup primes a normal response.
            self.assertEqual(lookup("task-good"), 1)
            # Failure disables further lookups.
            self.assertIsNone(lookup("task-bad"))
            # Any further task, even previously un-queried, returns None.
            self.assertIsNone(lookup("task-other"))
        self.assertIn("task.resume failed", stderr.getvalue())

    def test_uses_memd_url_precedence(self) -> None:
        """CLI flag wins over config wins over default."""
        args = _args(tenant_id="memd", project_id="memd", memd_url="http://cli/mcp")
        discovered = _discovered(memd_url="http://cfg/mcp")
        fake = _FakeClient()
        captured = {}

        def _ctor(url, timeout):
            captured["url"] = url
            captured["timeout"] = timeout
            return fake

        with patch("compiled_wiki.mcp_client.McpHttpClient", side_effect=_ctor):
            _build_staleness_lookup(args, discovered)
        self.assertEqual(captured["url"], "http://cli/mcp")

        args2 = _args(tenant_id="memd", project_id="memd", memd_url=None)
        captured2 = {}

        def _ctor2(url, timeout):
            captured2["url"] = url
            return fake

        with patch("compiled_wiki.mcp_client.McpHttpClient", side_effect=_ctor2):
            _build_staleness_lookup(args2, discovered)
        self.assertEqual(captured2["url"], "http://cfg/mcp")

        args3 = _args(tenant_id="memd", project_id="memd", memd_url=None)
        captured3 = {}

        def _ctor3(url, timeout):
            captured3["url"] = url
            return fake

        with patch("compiled_wiki.mcp_client.McpHttpClient", side_effect=_ctor3):
            _build_staleness_lookup(args3, _discovered())
        self.assertEqual(captured3["url"], DEFAULT_MEMD_URL)


if __name__ == "__main__":
    unittest.main()
