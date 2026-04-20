from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.compiler import build_log_entries  # noqa: E402
from compiled_wiki.render import (  # noqa: E402
    artifact_summary,
    render_index,
    render_library_page,
    render_task_page,
)


class RenderTests(unittest.TestCase):
    def test_artifact_summary_prefers_summary(self) -> None:
        artifact = {
            "summary": "A short summary",
            "what_worked": ["fallback text"],
        }
        self.assertEqual(artifact_summary(artifact), "A short summary")

    def test_render_task_page_contains_links_and_thread(self) -> None:
        task_payload = {
            "task_id": "task-123",
            "resume_artifact": {"artifact_id": "digest-task-123"},
            "resume_payload": {
                "trust_tier": "compiled_digest_hint",
                "verification_hint": {
                    "requires_verification": True,
                    "reason": "digest hint",
                },
                "grounding_refs": [
                    {
                        "task_id": "task-999",
                        "artifact_id": "artifact-root",
                        "artifact_kind": "task_finish",
                    }
                ],
            },
            "resume": {
                "task": {
                    "task_id": "task-123",
                    "status": "completed",
                    "goal": "Build the prototype",
                    "project_id": "memd",
                    "started_at_ms": 1000,
                    "updated_at_ms": 2000,
                },
                "latest_summary": "Prototype compiled successfully.",
                "what_worked": ["Deterministic renderers were sufficient."],
                "what_failed": ["None."],
                "validation": ["Smoke build passed."],
                "followups": ["Add concept pages later."],
            },
            "thread": {
                "artifacts": [
                    {
                        "artifact_id": "artifact-1",
                        "artifact_kind": "task_finish",
                        "summary": "Done.",
                        "timestamp_created": 2000,
                    }
                ]
            },
        }
        page = render_task_page("default", "memd", 3000, task_payload)
        self.assertIn("[`memd`](../projects/memd.md)", page)
        self.assertIn("Prototype compiled successfully.", page)
        self.assertIn("artifact-1", page)
        self.assertIn("Trust tier: `compiled_digest_hint`", page)
        self.assertIn("[task-999](../tasks/task-999.md)", page)

    def test_render_library_page_links_to_task_pages(self) -> None:
        payload = {
            "artifact": {
                "artifact_id": "digest-1",
                "summary": "Highlights available.",
                "source_updated_at_ms": 5000,
            },
            "trust_tier": "compiled_digest_hint",
            "verification_hint": {
                "requires_verification": True,
                "reason": "digest hint",
            },
            "grounding_refs": [
                {
                    "task_id": "task-999",
                    "artifact_id": "artifact-root",
                    "artifact_kind": "task_finish",
                }
            ],
            "results": [
                {
                    "task_id": "task-123",
                    "summary": "A useful lesson",
                    "confidence": 0.8,
                    "category": "decision",
                    "support_count": 2,
                }
            ],
        }
        page = render_library_page("highlights", "memd", 6000, payload)
        self.assertIn("[../tasks/task-123.md](../tasks/task-123.md)", page)
        self.assertIn("confidence=0.8", page)
        self.assertIn("Trust tier: `compiled_digest_hint`", page)
        self.assertIn("[task-999](../tasks/task-999.md)", page)

    def test_render_index_lists_tasks(self) -> None:
        tasks = [
            {
                "task_id": "task-123",
                "resume": {
                    "task": {"goal": "Build prototype", "updated_at_ms": 2000},
                    "latest_summary": "Latest summary text.",
                },
            }
        ]
        page = render_index("default", "memd", 3000, tasks)
        self.assertIn("projects/memd.md", page)
        self.assertIn("tasks/task-123.md", page)
        self.assertIn("## Trust Model", page)

    def test_build_log_entries_sorts_newest_first(self) -> None:
        tasks = [
            {
                "task_id": "task-a",
                "resume": {"task": {"goal": "Task A"}},
                "thread": {
                    "artifacts": [
                        {
                            "artifact_id": "a1",
                            "artifact_kind": "task_progress",
                            "timestamp_created": 1000,
                            "summary": "Older",
                        },
                        {
                            "artifact_id": "a2",
                            "artifact_kind": "task_finish",
                            "timestamp_created": 2000,
                            "summary": "Newer",
                        },
                    ]
                },
            }
        ]
        entries = build_log_entries(tasks)
        self.assertEqual(entries[0]["artifact_id"], "a2")
        self.assertEqual(entries[1]["artifact_id"], "a1")


if __name__ == "__main__":
    unittest.main()
