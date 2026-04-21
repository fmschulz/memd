"""Phase 2 of memd-wiki v2: compiler renders wiki_page artifacts.

Verifies:
- ``fetch_wiki_pages`` extracts artifacts from an artifact.search payload.
- ``sort_wiki_pages`` is deterministic under permutation.
- ``build_concept_page_record`` resolves grounding refs via artifact.get
  and preserves unresolved entries as marked stubs.
- ``render_concept_page`` emits frontmatter + body + grounded-by footer.
- An end-to-end ``build_wiki`` with seeded WikiPages writes the page at
  ``concepts/<artifact_id>.md`` and lists it in ``manifest.concept_pages``.
- A second rebuild on unchanged state produces ``written=0``.
"""

from __future__ import annotations

import io
import itertools
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.compiler import (  # noqa: E402
    BuildConfig,
    LLM_AUTHORED_PREFIXES,
    MANIFEST_SCHEMA_VERSION,
    build_wiki,
    sort_wiki_pages,
)
from compiled_wiki.render import render_concept_page  # noqa: E402


PROTOCOL_VERSION = "2025-11-25"


class _FakeResponse(io.BytesIO):
    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()


class SortWikiPagesTests(unittest.TestCase):
    def test_role_then_name_then_artifact_id(self) -> None:
        pages = [
            {"artifact_id": "z", "artifact_role": "concept", "summary": "Beta concept"},
            {"artifact_id": "a", "artifact_role": "entity", "summary": "Alpha entity"},
            {"artifact_id": "m", "artifact_role": "concept", "summary": "Alpha concept"},
            {
                "artifact_id": "n",
                "artifact_role": "entity",
                "entity_refs": [{"name": "Aardvark"}],
                "summary": "irrelevant",
            },
        ]
        sorted_pages = sort_wiki_pages(pages)
        self.assertEqual(
            [p["artifact_id"] for p in sorted_pages],
            ["m", "z", "n", "a"],
        )

    def test_artifact_id_breaks_ties_when_role_and_name_match(self) -> None:
        pages = [
            {"artifact_id": "b", "artifact_role": "concept", "summary": "Same"},
            {"artifact_id": "a", "artifact_role": "concept", "summary": "Same"},
        ]
        self.assertEqual(
            [p["artifact_id"] for p in sort_wiki_pages(pages)],
            ["a", "b"],
        )


class RenderConceptPageTests(unittest.TestCase):
    def test_emits_frontmatter_summary_body_and_grounded_by(self) -> None:
        record = {
            "page": {
                "artifact_id": "0199-page",
                "artifact_kind": "wiki_page",
                "artifact_role": "concept",
                "summary": "Verification boundary",
                "content": "# Boundary\n\nDistinct-writer countersignatures.",
            },
            "path": "concepts/0199-page.md",
            "lane": "concepts",
            "artifact_id": "0199-page",
            "artifact_role": "concept",
            "trust_tier": "canonical_record",
            "source_updated_at_ms": 1500,
            "grounding_refs": [
                {
                    "artifact_id": "0188-grounding",
                    "task_id": "task-A",
                    "artifact_kind": "task_finish",
                    "artifact_role": None,
                    "trust_tier": "canonical_record",
                    "resolved": True,
                }
            ],
        }
        text = render_concept_page("memd", "memd", 1500, record)
        self.assertIn("---\nartifact_id: 0199-page\n", text)
        self.assertIn("artifact_kind: wiki_page", text)
        self.assertIn("artifact_role: concept", text)
        self.assertIn("trust_tier: canonical_record", text)
        self.assertIn("# Verification boundary", text)
        self.assertIn("## Body", text)
        self.assertIn("Distinct-writer countersignatures.", text)
        self.assertIn("## Grounded By", text)
        self.assertIn("[task-A](../tasks/task-A.md)", text)

    def test_unresolved_grounding_renders_with_marker(self) -> None:
        record = {
            "page": {
                "artifact_id": "0199-page",
                "summary": "X",
                "artifact_role": "entity",
                "content": None,
            },
            "path": "entities/0199-page.md",
            "lane": "entities",
            "artifact_id": "0199-page",
            "artifact_role": "entity",
            "trust_tier": "canonical_record",
            "source_updated_at_ms": 0,
            "grounding_refs": [
                {
                    "artifact_id": "missing",
                    "task_id": "unknown-task",
                    "artifact_kind": "unknown",
                    "trust_tier": "unknown",
                    "resolved": False,
                }
            ],
        }
        text = render_concept_page("memd", "memd", 0, record)
        self.assertIn("*(unresolved)*", text)

    def test_verified_by_section_emits_when_verifications_present(self) -> None:
        record = {
            "page": {
                "artifact_id": "0199-page",
                "summary": "X",
                "artifact_role": "concept",
                "content": "# X",
            },
            "path": "concepts/0199-page.md",
            "lane": "concepts",
            "artifact_id": "0199-page",
            "artifact_role": "concept",
            "trust_tier": "canonical_record",
            "source_updated_at_ms": 1000,
            "grounding_refs": [
                {
                    "artifact_id": "g1",
                    "task_id": "t1",
                    "artifact_kind": "task_finish",
                    "resolved": True,
                }
            ],
            "verifications": [
                {
                    "artifact_id": "v1",
                    "agent_id": "reviewer-beta",
                    "timestamp_created": 2_000_000_000_000,
                }
            ],
        }
        text = render_concept_page("memd", "memd", 1000, record)
        self.assertIn("## Verified By", text)
        self.assertIn("Verified by: reviewer-beta", text)
        self.assertIn("artifact `v1`", text)

    def test_no_verified_by_section_when_empty(self) -> None:
        record = {
            "page": {"artifact_id": "p", "summary": "X", "artifact_role": "concept"},
            "path": "concepts/p.md",
            "lane": "concepts",
            "artifact_id": "p",
            "artifact_role": "concept",
            "trust_tier": "canonical_record",
            "source_updated_at_ms": 0,
            "grounding_refs": [
                {
                    "artifact_id": "g",
                    "task_id": "t",
                    "artifact_kind": "task_finish",
                    "resolved": True,
                }
            ],
            "verifications": [],
        }
        text = render_concept_page("memd", "memd", 0, record)
        self.assertNotIn("## Verified By", text)


def _seed_wiki_page(artifact_id: str = "0199-wiki", role: str = "concept") -> dict[str, Any]:
    return {
        "artifact_id": artifact_id,
        "artifact_kind": "wiki_page",
        "artifact_role": role,
        "summary": "Concept page about the verification boundary.",
        "content": "# Verification boundary\n\nDistinct-writer countersignatures.",
        "related_artifact_ids": ["0188-grounding"],
        "tenant_id": "memd",
        "project_id": "memd",
        "task_id": "task-wiki-author",
        "promotion_state": "canonical",
        "source_updated_at_ms": 1500,
        "timestamp_created": 1500,
    }


def _seed_grounding_artifact() -> dict[str, Any]:
    return {
        "artifact_id": "0188-grounding",
        "artifact_kind": "task_finish",
        "task_id": "task-grounding",
        "promotion_state": "canonical",
        "summary": "Canonical record cited by the wiki page.",
    }


def _make_mock_server(
    wiki_pages: list[dict[str, Any]],
    grounding_artifacts: dict[str, dict[str, Any]],
) -> object:
    base_responses: dict[str, dict[str, Any]] = {
        "context.brief_project": {
            "brief": {"overview": "Fixture", "source_task_ids": ["task-A"]},
            "artifact": {"artifact_id": "digest-project", "source_updated_at_ms": 1000},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
            "verification_hint": {"requires_verification": True, "reason": "digest"},
        },
        "artifact.find_failures": {
            "results": [],
            "artifact": {"artifact_id": "digest-fails", "source_updated_at_ms": 800},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_decisions": {
            "results": [],
            "artifact": {"artifact_id": "digest-dec", "source_updated_at_ms": 800},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_evidence": {
            "results": [],
            "artifact": {"artifact_id": "digest-ev", "source_updated_at_ms": 800},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "artifact.find_highlights": {
            "results": [],
            "artifact": {"artifact_id": "digest-hi", "source_updated_at_ms": 800},
            "trust_tier": "compiled_digest_hint",
            "grounding_refs": [],
        },
        "task.resume": {
            "resume": {
                "task": {
                    "task_id": "task-A",
                    "goal": "Fixture",
                    "status": "completed",
                    "updated_at_ms": 1200,
                    "started_at_ms": 100,
                },
                "latest_summary": "fixture",
                "what_worked": [],
                "what_failed": [],
                "validation": [],
                "followups": [],
            },
            "artifact": {"artifact_id": "digest-task-A", "source_updated_at_ms": 1200},
            "trust_tier": "canonical_record",
            "verification_hint": {"requires_verification": False, "reason": "canonical"},
            "grounding_refs": [],
        },
        "artifact.list_thread": {"artifacts": []},
        "artifact.search": {
            "results": [{"artifact": page} for page in wiki_pages],
        },
    }

    counter = itertools.count(1)

    def _handle(req, *, timeout: float = 0.0) -> _FakeResponse:
        _ = timeout
        body = req.data.decode("utf-8")
        payload = json.loads(body)
        method = payload["method"]
        req_id = payload.get("id", next(counter))
        if method == "initialize":
            result = {
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": "memd", "version": "0.12.0"},
            }
        elif method == "tools/call":
            tool = payload["params"]["name"]
            args = payload["params"]["arguments"]
            if tool == "artifact.get":
                aid = args.get("artifact_id")
                canonical = {"artifact": grounding_artifacts.get(aid)}
            else:
                canonical = base_responses[tool]
            result = {"content": [{"type": "text", "text": json.dumps(canonical)}]}
        else:
            raise AssertionError(f"unexpected method {method}")
        envelope = {"jsonrpc": "2.0", "id": req_id, "result": result}
        return _FakeResponse(json.dumps(envelope).encode("utf-8"))

    return _handle


class BuildWithWikiPageTests(unittest.TestCase):
    def test_seeded_wiki_page_writes_concept_file_and_manifest_entry(self) -> None:
        wiki_page = _seed_wiki_page()
        grounding = _seed_grounding_artifact()
        handler = _make_mock_server(
            [wiki_page], {grounding["artifact_id"]: grounding}
        )
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

            concept_path = outdir / "concepts" / f"{wiki_page['artifact_id']}.md"
            self.assertTrue(concept_path.is_file(), "concept file should be written")
            text = concept_path.read_text(encoding="utf-8")
            self.assertIn("artifact_id: 0199-wiki", text)
            self.assertIn("artifact_role: concept", text)
            self.assertIn("Distinct-writer countersignatures.", text)
            self.assertIn("[task-grounding]", text)

            manifest = json.loads((outdir / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["schema_version"], MANIFEST_SCHEMA_VERSION)
            self.assertEqual(
                manifest["llm_authored_prefixes"], list(LLM_AUTHORED_PREFIXES)
            )
            self.assertEqual(len(manifest["concept_pages"]), 1)
            entry = manifest["concept_pages"][0]
            self.assertEqual(entry["artifact_id"], "0199-wiki")
            self.assertEqual(entry["path"], "concepts/0199-wiki.md")
            self.assertEqual(entry["artifact_role"], "concept")
            self.assertEqual(entry["trust_tier"], "canonical_record")
            self.assertEqual(
                entry["grounding_refs"],
                [
                    {
                        "artifact_id": "0188-grounding",
                        "task_id": "task-grounding",
                        "artifact_kind": "task_finish",
                    }
                ],
            )

    def test_rebuild_writes_zero_files(self) -> None:
        wiki_page = _seed_wiki_page()
        grounding = _seed_grounding_artifact()
        handler = _make_mock_server(
            [wiki_page], {grounding["artifact_id"]: grounding}
        )
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
                second = build_wiki(config)
            # Idempotency contract: second rebuild rewrites nothing.
            self.assertEqual(second.written_files, 0)

    def test_no_wiki_pages_skips_concepts_dir(self) -> None:
        handler = _make_mock_server([], {})
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
            # Empty wiki_pages: concepts/ and entities/ stay absent.
            self.assertFalse((outdir / "concepts").exists())
            self.assertFalse((outdir / "entities").exists())
            manifest = json.loads((outdir / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["concept_pages"], [])

    def test_unresolved_grounding_renders_with_marker(self) -> None:
        wiki_page = _seed_wiki_page()
        # No grounding artifacts seeded → resolution returns None.
        handler = _make_mock_server([wiki_page], {})
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
            text = (outdir / "concepts" / "0199-wiki.md").read_text(encoding="utf-8")
            self.assertIn("*(unresolved)*", text)


if __name__ == "__main__":
    unittest.main()
