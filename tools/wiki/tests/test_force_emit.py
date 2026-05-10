"""Force-emit-referenced-task-pages invariant (Item 7 plan §5 / step 6).

When a library or project page links to a task whose id is NOT in
the top-``max_tasks`` window, the compiler must still emit
``tasks/<id>.md`` so the internal link is not dangling.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from compiled_wiki.compiler import (  # noqa: E402
    BuildConfig,
    build_wiki,
    collect_referenced_task_ids,
    ensure_library_grounding_refs,
    safe_project_page_stem,
)
from compiled_wiki.cli_client import MemdCliError  # noqa: E402
from test_determinism import _FakeResponse, _make_mock_server  # noqa: E402


class _HandlerClient:
    def __init__(self, handler: object) -> None:
        self._handler = handler
        self._next_id = 0

    def call_tool(self, name: str, arguments: dict):
        self._next_id += 1
        payload = {
            "jsonrpc": "2.0",
            "id": self._next_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        request = type(
            "Request",
            (),
            {"data": json.dumps(payload).encode("utf-8")},
        )()
        response = self._handler(request, timeout=0.0)
        envelope = json.loads(response.read().decode("utf-8"))
        if "error" in envelope:
            raise MemdCliError(envelope["error"].get("message", envelope["error"]))
        content = envelope["result"]["content"]
        return json.loads(content[0]["text"])


class CollectReferencedTaskIdsTests(unittest.TestCase):
    def test_pulls_from_library_results(self) -> None:
        libs = {
            "failures": {
                "results": [
                    {"task_id": "task-a"},
                    {"task_id": "task-b"},
                ]
            }
        }
        self.assertEqual(
            collect_referenced_task_ids({"grounding_refs": []}, libs),
            {"task-a", "task-b"},
        )

    def test_pulls_from_library_grounding_refs(self) -> None:
        libs = {
            "decisions": {
                "results": [],
                "grounding_refs": [{"task_id": "task-z"}],
            }
        }
        self.assertEqual(
            collect_referenced_task_ids({"grounding_refs": []}, libs),
            {"task-z"},
        )

    def test_pulls_from_project_grounding_refs(self) -> None:
        project = {"grounding_refs": [{"task_id": "task-proj"}]}
        self.assertEqual(
            collect_referenced_task_ids(project, {}),
            {"task-proj"},
        )

    def test_ignores_non_string_ids(self) -> None:
        libs = {
            "failures": {
                "results": [
                    {"task_id": None},
                    {"task_id": ""},
                    {"task_id": 42},
                    {"task_id": "   "},
                    {"task_id": "task-real"},
                ]
            }
        }
        self.assertEqual(
            collect_referenced_task_ids({}, libs),
            {"task-real"},
        )


class LibraryGroundingBackfillTests(unittest.TestCase):
    def test_backfills_grounding_refs_from_library_results(self) -> None:
        libs = {
            "failures": {
                "results": [
                    {
                        "task_id": "task-a",
                        "artifact_id": "artifact-a",
                        "artifact_kind": "task_finish",
                    },
                    {
                        "task_id": "task-a",
                        "artifact_id": "artifact-a",
                        "artifact_kind": "task_finish",
                    },
                ]
            }
        }
        ensure_library_grounding_refs(libs)
        self.assertEqual(
            libs["failures"]["grounding_refs"],
            [
                {
                    "task_id": "task-a",
                    "artifact_id": "artifact-a",
                    "artifact_kind": "task_finish",
                }
            ],
        )


def _handler_with_out_of_window_referenced_task() -> object:
    """Return a mock handler where a library references task-outside-max_tasks."""
    base = _make_mock_server()

    def _handle(req, *, timeout: float = 0.0):
        body = req.data.decode("utf-8")
        payload = json.loads(body)
        if (
            payload.get("method") == "tools/call"
            and payload["params"]["name"] == "artifact.find_failures"
        ):
            # Library points at a task that is NOT in source_task_ids
            # (the canonical fixture declares ["task-001", "task-002"]).
            envelope = {
                "jsonrpc": "2.0",
                "id": payload.get("id", 0),
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": json.dumps(
                                {
                                    "results": [
                                        {
                                            "task_id": "task-outside",
                                            "artifact_id": "art-outside",
                                            "summary": "out-of-window ref",
                                        },
                                    ],
                                    "artifact": {
                                        "artifact_id": "digest-failures",
                                        "source_updated_at_ms": 900,
                                    },
                                    "trust_tier": "compiled_digest_hint",
                                    "grounding_refs": [],
                                }
                            ),
                        }
                    ],
                },
            }
            return _FakeResponse(json.dumps(envelope).encode("utf-8"))
        return base(req, timeout=timeout)

    return _handle


class ForceEmitIntegrationTests(unittest.TestCase):
    def test_library_referenced_out_of_window_task_emits_task_page(self) -> None:
        handler = _handler_with_out_of_window_referenced_task()
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            config = BuildConfig(
                memd_url="http://fake/mcp",
                tenant_id="memd",
                project_id="memd",
                output_dir=outdir,
                max_tasks=2,
                library_k=5,
                forbidden_data_dirs=[],
            )
            with patch(
                "compiled_wiki.compiler.MemdCliClient",
                return_value=_HandlerClient(handler),
            ):
                build_wiki(config)
            tasks_dir = outdir / "tasks"
            emitted = {p.name for p in tasks_dir.glob("*.md")}
            self.assertIn("task-outside.md", emitted)
            # The primary set must still be emitted.
            self.assertIn("task-001.md", emitted)
            self.assertIn("task-002.md", emitted)

    def test_missing_referenced_task_is_pruned_instead_of_failing_build(self) -> None:
        base = _make_mock_server()

        def _handle(req, *, timeout: float = 0.0):
            body = req.data.decode("utf-8")
            payload = json.loads(body)
            if payload.get("method") == "tools/call":
                tool = payload["params"]["name"]
                args = payload["params"]["arguments"]
                if tool == "artifact.find_failures":
                    envelope = {
                        "jsonrpc": "2.0",
                        "id": payload.get("id", 0),
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": json.dumps(
                                        {
                                            "results": [
                                                {
                                                    "task_id": "task-missing",
                                                    "artifact_id": "art-missing",
                                                    "summary": "stale ref",
                                                },
                                                {
                                                    "task_id": "task-001",
                                                    "artifact_id": "art-live",
                                                    "summary": "live ref",
                                                },
                                            ],
                                            "artifact": {
                                                "artifact_id": "digest-failures",
                                                "source_updated_at_ms": 900,
                                            },
                                            "trust_tier": "compiled_digest_hint",
                                            "grounding_refs": [
                                                {"task_id": "task-missing"},
                                                {"task_id": "task-001"},
                                            ],
                                        }
                                    ),
                                }
                            ],
                        },
                    }
                    return _FakeResponse(json.dumps(envelope).encode("utf-8"))
                if tool == "task.resume" and args["task_id"] == "task-missing":
                    envelope = {
                        "jsonrpc": "2.0",
                        "id": payload.get("id", 0),
                        "error": {"code": -32000, "message": "task not found"},
                    }
                    return _FakeResponse(json.dumps(envelope).encode("utf-8"))
            return base(req, timeout=timeout)

        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            config = BuildConfig(
                memd_url="http://fake/mcp",
                tenant_id="memd",
                project_id="memd",
                output_dir=outdir,
                max_tasks=2,
                library_k=5,
                forbidden_data_dirs=[],
            )
            with patch(
                "compiled_wiki.compiler.MemdCliClient",
                return_value=_HandlerClient(_handle),
            ):
                result = build_wiki(config)

            self.assertEqual(result.skipped_task_ids, ["task-missing"])
            self.assertFalse((outdir / "tasks" / "task-missing.md").exists())
            failures = (outdir / "libraries" / "failures.md").read_text(
                encoding="utf-8"
            )
            self.assertNotIn("task-missing", failures)
            self.assertIn("task-001", failures)
            manifest = json.loads(
                (outdir / "manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual(manifest["skipped_task_ids"], ["task-missing"])

    def test_path_shaped_project_id_uses_safe_project_page_path(self) -> None:
        self.assertEqual(
            safe_project_page_stem("/workspace/projects/csag_test"),
            "workspace-projects-csag_test",
        )
        handler = _make_mock_server()
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            config = BuildConfig(
                memd_url="http://fake/mcp",
                tenant_id="memd",
                project_id="/workspace/projects/csag_test",
                output_dir=outdir,
                max_tasks=2,
                library_k=5,
                forbidden_data_dirs=[],
            )
            stale_page = (
                outdir
                / "projects"
                / "workspace"
                / "projects"
                / "csag_test.md"
            )
            stale_page.parent.mkdir(parents=True)
            stale_page.write_text("# stale raw project path\n", encoding="utf-8")
            with patch(
                "compiled_wiki.compiler.MemdCliClient",
                return_value=_HandlerClient(handler),
            ):
                build_wiki(config)

            project_page = outdir / "projects" / "workspace-projects-csag_test.md"
            self.assertTrue(project_page.exists())
            self.assertFalse((outdir / "workspace").exists())
            self.assertFalse(stale_page.exists())
            index = (outdir / "index.md").read_text(encoding="utf-8")
            self.assertIn(
                "projects/workspace-projects-csag_test.md",
                index,
            )


if __name__ == "__main__":
    unittest.main()
