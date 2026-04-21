"""Determinism pin for memd-wiki's deterministic rebuild contract.

Plan §6 (honest v1 contract): a second ``build_wiki()`` against
unchanged memd state produces ``written=0, unchanged=N`` and a
byte-identical ``manifest.json``. This test exercises that invariant
end-to-end via a mocked MCP transport so it never needs a live daemon.

Plan §6.1: ``manifest.json`` carries ``schema_version`` and
``compiler_owned_prefixes`` so the v2 LLM-authored / human-edited
ownership split can slot in without changing the manifest format.
"""

from __future__ import annotations

import io
import itertools
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.compiler import (  # noqa: E402
    COMPILER_OWNED_PREFIXES,
    MANIFEST_SCHEMA_VERSION,
    BuildConfig,
    build_wiki,
)


class _FakeResponse(io.BytesIO):
    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()


PROTOCOL_VERSION = "2025-11-25"


def _make_mock_server() -> object:
    """Return a callable that responds to MCP `initialize` + tool-call posts.

    Serializes deterministic payloads keyed by the JSON-RPC method+tool.
    """
    tool_responses: dict[str, dict] = {
        "context.brief_project": {
            "brief": {
                "overview": "Fixture project",
                "source_task_ids": ["task-001", "task-002"],
            },
            "artifact": {
                "artifact_id": "digest-project",
                "source_updated_at_ms": 1000,
            },
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
            "verification_hint": {"requires_verification": True, "reason": "digest hint"},
        },
        "artifact.find_failures": {
            "results": [
                {"task_id": "task-001", "summary": "failure-1"},
            ],
            "artifact": {"artifact_id": "digest-failures", "source_updated_at_ms": 900},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_decisions": {
            "results": [],
            "artifact": {"artifact_id": "digest-decisions", "source_updated_at_ms": 900},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_evidence": {
            "results": [],
            "artifact": {"artifact_id": "digest-evidence", "source_updated_at_ms": 900},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_highlights": {
            "results": [],
            "artifact": {"artifact_id": "digest-highlights", "source_updated_at_ms": 900},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "task.resume": {
            "resume": {
                "task": {
                    "task_id": "task-001",
                    "goal": "Fixture task 1",
                    "status": "completed",
                    "updated_at_ms": 1200,
                    "started_at_ms": 100,
                },
                "latest_summary": "Fixture latest summary 1",
                "what_worked": [],
                "what_failed": [],
                "validation": [],
                "followups": [],
            },
            "artifact": {"artifact_id": "digest-task-001", "source_updated_at_ms": 1200},
            "trust_tier": "canonical_record",
            "verification_hint": {"requires_verification": False, "reason": "canonical"},
            "grounding_refs": [],
        },
        "artifact.list_thread": {
            "artifacts": [
                {
                    "artifact_id": "a-1",
                    "artifact_kind": "task_progress",
                    "summary": "progress 1",
                    "timestamp_created": 1100,
                }
            ],
        },
        # v2 phase 2: the compiler now pulls every wiki_page artifact
        # for the project. Default fixture has none — concept-page
        # rendering is exercised by ConceptRenderTests below.
        "artifact.search": {
            "results": [],
        },
        # v2 phase 2: artifact.get is used to resolve grounding refs
        # for any wiki_page that's authored. Default returns no
        # artifact; a concept fixture overrides this.
        "artifact.get": {
            "artifact": None,
        },
    }
    # Provide per-task overrides for task-002 so the two-task fixture
    # yields distinct canonical payloads.
    per_task = {
        "task-001": tool_responses["task.resume"],
        "task-002": {
            "resume": {
                "task": {
                    "task_id": "task-002",
                    "goal": "Fixture task 2",
                    "status": "in_progress",
                    "updated_at_ms": 1300,
                    "started_at_ms": 200,
                },
                "latest_summary": "Fixture latest summary 2",
                "what_worked": [],
                "what_failed": [],
                "validation": [],
                "followups": [],
            },
            "artifact": {"artifact_id": "digest-task-002", "source_updated_at_ms": 1300},
            "trust_tier": "canonical_record",
            "verification_hint": {"requires_verification": False, "reason": "canonical"},
            "grounding_refs": [],
        },
    }

    request_counter = itertools.count(1)

    def _synth_task_resume(task_id: str) -> dict[str, Any]:
        return {
            "resume": {
                "task": {
                    "task_id": task_id,
                    "goal": f"Force-emit task {task_id}",
                    "status": "unknown",
                    "updated_at_ms": 500,
                    "started_at_ms": 50,
                },
                "latest_summary": f"Force-emit latest {task_id}",
                "what_worked": [],
                "what_failed": [],
                "validation": [],
                "followups": [],
            },
            "artifact": {"artifact_id": f"digest-{task_id}", "source_updated_at_ms": 500},
            "trust_tier": "canonical_record",
            "verification_hint": {"requires_verification": False, "reason": "canonical"},
            "grounding_refs": [],
        }

    def _handle(req, *, timeout: float = 0.0) -> _FakeResponse:
        _ = timeout
        body = req.data.decode("utf-8")
        payload = json.loads(body)
        method = payload["method"]
        req_id = payload.get("id", next(request_counter))
        if method == "initialize":
            result = {
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": "memd", "version": "0.10.0"},
            }
        elif method == "tools/call":
            tool = payload["params"]["name"]
            args = payload["params"]["arguments"]
            if tool == "task.resume":
                task_id = args["task_id"]
                canonical = per_task.get(task_id) or _synth_task_resume(task_id)
            elif tool == "artifact.list_thread":
                # Synthesize a minimal empty-thread response for
                # force-emit task_ids that aren't in the main fixture.
                canonical = tool_responses["artifact.list_thread"]
            else:
                canonical = tool_responses[tool]
            result = {
                "content": [
                    {"type": "text", "text": json.dumps(canonical)}
                ],
            }
        else:
            raise AssertionError(f"unexpected method {method}")
        envelope = {"jsonrpc": "2.0", "id": req_id, "result": result}
        return _FakeResponse(json.dumps(envelope).encode("utf-8"))

    return _handle


class DeterministicRebuildTests(unittest.TestCase):
    def test_second_run_writes_zero_and_manifest_is_byte_identical(self) -> None:
        handler = _make_mock_server()
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
                "compiled_wiki.mcp_client.urllib.request.urlopen",
                side_effect=handler,
            ):
                first = build_wiki(config)
                manifest_after_first = (outdir / "manifest.json").read_bytes()
                self.assertGreater(first.written_files, 0)
                self.assertEqual(first.unchanged_files, 0)

                second = build_wiki(config)
                manifest_after_second = (outdir / "manifest.json").read_bytes()

            # Plan §6 contract:
            self.assertEqual(second.written_files, 0)
            self.assertEqual(second.unchanged_files, first.written_files)
            self.assertEqual(manifest_after_first, manifest_after_second)


class PermutationStabilityTests(unittest.TestCase):
    """Output must be stable when the backend reorders logically identical payloads.

    Codex r1 step 5 MEDIUM: the original determinism pin only proved
    idempotence under identical MCP payload ordering. The compiler now
    applies stable secondary sorts on task_id / artifact_id so a
    reordered-but-equivalent response set yields the same bytes.
    """

    def _run_build_with_handler(self, handler: object) -> bytes:
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
                "compiled_wiki.mcp_client.urllib.request.urlopen",
                side_effect=handler,
            ):
                build_wiki(config)
            # Concatenate all deterministic byte-level output so the
            # test fails on any ordering drift, not just manifest drift.
            return (
                (outdir / "manifest.json").read_bytes()
                + b"\x00"
                + (outdir / "index.md").read_bytes()
                + b"\x00"
                + (outdir / "log.md").read_bytes()
                + b"\x00"
                + (outdir / "libraries" / "failures.md").read_bytes()
                + b"\x00"
                + (outdir / "projects" / "memd.md").read_bytes()
            )

    def test_task_order_permutation_produces_same_output(self) -> None:
        # Build handler that swaps the source_task_ids order.
        reordered_handler = self._make_reordered_handler(swap_tasks=True)
        canonical_bytes = self._run_build_with_handler(_make_mock_server())
        reordered_bytes = self._run_build_with_handler(reordered_handler)
        self.assertEqual(canonical_bytes, reordered_bytes)

    def test_library_result_permutation_produces_same_output(self) -> None:
        # Build a handler with a library that returns 3 items, shuffled.
        canonical = self._make_multi_result_handler(order=["a", "b", "c"])
        reordered = self._make_multi_result_handler(order=["c", "a", "b"])
        self.assertEqual(
            self._run_build_with_handler(canonical),
            self._run_build_with_handler(reordered),
        )

    def _make_reordered_handler(self, swap_tasks: bool) -> object:
        # Inlined factory that mirrors _make_mock_server but swaps
        # source_task_ids order when requested.
        base = _make_mock_server()

        def _handle(req, *, timeout: float = 0.0):
            body = req.data.decode("utf-8")
            payload = json.loads(body)
            if (
                swap_tasks
                and payload.get("method") == "tools/call"
                and payload["params"]["name"] == "context.brief_project"
            ):
                # Return the project payload with source_task_ids swapped.
                reordered = {
                    "brief": {
                        "overview": "Fixture project",
                        "source_task_ids": ["task-002", "task-001"],
                    },
                    "artifact": {
                        "artifact_id": "digest-project",
                        "source_updated_at_ms": 1000,
                    },
                    "trust_tier": "compiled_digest_hint",
                    "grounding_refs": [],
                    "verification_hint": {
                        "requires_verification": True,
                        "reason": "digest hint",
                    },
                }
                envelope = {
                    "jsonrpc": "2.0",
                    "id": payload.get("id", 0),
                    "result": {
                        "content": [
                            {"type": "text", "text": json.dumps(reordered)}
                        ],
                    },
                }
                return _FakeResponse(json.dumps(envelope).encode("utf-8"))
            return base(req, timeout=timeout)

        return _handle

    def _make_multi_result_handler(self, order: list[str]) -> object:
        results = {
            "a": {"task_id": "task-001", "artifact_id": "art-a", "summary": "s-a"},
            "b": {"task_id": "task-002", "artifact_id": "art-b", "summary": "s-b"},
            "c": {"task_id": "task-003", "artifact_id": "art-c", "summary": "s-c"},
        }
        reshuffled = [results[k] for k in order]
        base = _make_mock_server()

        def _handle(req, *, timeout: float = 0.0):
            body = req.data.decode("utf-8")
            payload = json.loads(body)
            if (
                payload.get("method") == "tools/call"
                and payload["params"]["name"] == "artifact.find_failures"
            ):
                envelope = {
                    "jsonrpc": "2.0",
                    "id": payload.get("id", 0),
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": json.dumps(
                                    {
                                        "results": reshuffled,
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


class ManifestSchemaTests(unittest.TestCase):
    def test_manifest_has_schema_version_and_owned_prefixes(self) -> None:
        handler = _make_mock_server()
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
                "compiled_wiki.mcp_client.urllib.request.urlopen",
                side_effect=handler,
            ):
                build_wiki(config)
            manifest = json.loads((outdir / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["schema_version"], MANIFEST_SCHEMA_VERSION)
        # v2 schema_version. Phase 4 forward-compat lets a v1 reader
        # error cleanly when it sees this value.
        self.assertEqual(manifest["schema_version"], 2)
        self.assertEqual(
            tuple(manifest["compiler_owned_prefixes"]),
            COMPILER_OWNED_PREFIXES,
        )
        # v2 lanes are always emitted (empty when no wiki_page
        # artifacts exist) so v2 readers see a stable shape.
        self.assertEqual(
            manifest["llm_authored_prefixes"],
            ["concepts/", "entities/"],
        )
        self.assertEqual(
            manifest["human_owned_prefixes"],
            ["notes/"],
        )
        self.assertEqual(manifest["concept_pages"], [])

    def test_compiler_owned_prefixes_includes_all_page_roots(self) -> None:
        # Regression guard: the tuple must cover every page kind the
        # compiler emits. If a new page kind is added, update both.
        expected = {
            "index.md",
            "log.md",
            "manifest.json",
            "projects/",
            "tasks/",
            "libraries/",
        }
        self.assertEqual(set(COMPILER_OWNED_PREFIXES), expected)


if __name__ == "__main__":
    unittest.main()
