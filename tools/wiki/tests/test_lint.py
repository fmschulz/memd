"""Tests for the 5 memd-wiki lint checks (plan §5 / step 7)."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.lint import (  # noqa: E402
    LintFinding,
    LintReport,
    lint_output_dir,
)


def _seed_manifest(outdir: Path, *, project_id: str = "memd", task_ids: list[str] | None = None) -> None:
    task_ids = task_ids or []
    (outdir / "manifest.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "compiler_owned_prefixes": [
                    "index.md",
                    "log.md",
                    "manifest.json",
                    "projects/",
                    "tasks/",
                    "libraries/",
                ],
                "project_id": project_id,
                "task_ids": task_ids,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


def _seed_base_tree(outdir: Path, *, task_ids: list[str]) -> None:
    """Minimal tree that passes every check if grounded + non-stale."""
    (outdir / "projects").mkdir(parents=True, exist_ok=True)
    (outdir / "tasks").mkdir(parents=True, exist_ok=True)
    (outdir / "libraries").mkdir(parents=True, exist_ok=True)
    _seed_manifest(outdir, task_ids=task_ids)
    (outdir / "index.md").write_text("# idx\n", encoding="utf-8")
    (outdir / "log.md").write_text("# log\n", encoding="utf-8")
    (outdir / "projects" / "memd.md").write_text("# project\n", encoding="utf-8")
    for library in ("failures", "decisions", "evidence", "highlights"):
        (outdir / "libraries" / f"{library}.md").write_text(
            "# lib\n- Trust tier: `canonical_record`\n",
            encoding="utf-8",
        )
    for task_id in task_ids:
        (outdir / "tasks" / f"{task_id}.md").write_text(
            f"# Task: {task_id}\n- Task ID: `{task_id}`\n",
            encoding="utf-8",
        )


class CleanTreeTests(unittest.TestCase):
    def test_base_seed_yields_clean_report(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-001"])
            report = lint_output_dir(outdir)
            self.assertEqual(report.findings, ())
            self.assertEqual(report.exit_code(), 0)


class LibraryGroundingTests(unittest.TestCase):
    def test_digest_library_without_grounding_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            # Rewrite failures library to digest-backed without grounding
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n- Trust tier: `compiled_digest_hint`\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.errors}
            self.assertIn("library-missing-grounding", checks)
            self.assertEqual(report.exit_code(), 2)

    def test_digest_library_with_grounding_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n"
                "- Trust tier: `compiled_digest_hint`\n"
                "### Grounded By\n"
                "- [task-x](../tasks/task-x.md)\n",
                encoding="utf-8",
            )
            # Grounding reference must resolve, so also seed task-x page.
            (outdir / "tasks" / "task-x.md").write_text("# t\n", encoding="utf-8")
            report = lint_output_dir(outdir)
            self.assertEqual(
                [f.check for f in report.errors if f.check == "library-missing-grounding"],
                [],
            )


class DeadBacklinkTests(unittest.TestCase):
    def test_library_linking_missing_task_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-alive"])
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n"
                "- Trust tier: `canonical_record`\n"
                "- [task-dead](../tasks/task-dead.md)\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            dead = [f for f in report.errors if f.check == "dead-backlink"]
            self.assertTrue(dead)
            self.assertIn("tasks/task-dead.md", dead[0].message)

    def test_index_linking_missing_task_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            (outdir / "index.md").write_text(
                "# idx\n- [task-dead](tasks/task-dead.md)\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            dead = [f for f in report.errors if f.check == "dead-backlink"]
            self.assertTrue(dead)

    def test_existing_task_target_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-alive"])
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n"
                "- Trust tier: `canonical_record`\n"
                "- [task-alive](../tasks/task-alive.md)\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            dead = [f for f in report.findings if f.check == "dead-backlink"]
            self.assertEqual(dead, [])


class TrustTierSurfacingTests(unittest.TestCase):
    def test_ungrounded_digest_task_is_warn(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            (outdir / "tasks" / "t1.md").write_text(
                "# Task t1\n"
                "- Trust tier: `compiled_digest_hint`\n"
                "- Requires verification: `True`\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            warns = [f for f in report.warnings if f.check == "trust-tier-ungrounded"]
            self.assertTrue(warns)
            self.assertEqual(report.exit_code(), 1)

    def test_canonical_task_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            (outdir / "tasks" / "t1.md").write_text(
                "# Task t1\n- Trust tier: `canonical_record`\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            warns = [f for f in report.warnings if f.check == "trust-tier-ungrounded"]
            self.assertEqual(warns, [])


class ManifestDriftTests(unittest.TestCase):
    def test_missing_manifest_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            # No seed at all; just a stub index so outdir exists but manifest missing.
            (outdir / "index.md").write_text("# idx\n", encoding="utf-8")
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.errors}
            self.assertIn("manifest-missing", checks)

    def test_extra_file_in_owned_prefix_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-001"])
            # Add a library page the manifest implies should NOT exist.
            (outdir / "libraries" / "extra.md").write_text("# extra\n", encoding="utf-8")
            report = lint_output_dir(outdir)
            drift = [f for f in report.errors if f.check == "manifest-drift"]
            self.assertTrue(drift)
            self.assertIn("libraries/extra.md", {f.path for f in drift})

    def test_force_emit_task_page_not_in_manifest_is_accepted(self) -> None:
        """Step 6 force-emit: tasks pages for referenced-but-out-of-window are OK."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-primary"])
            (outdir / "tasks" / "task-force-emit.md").write_text(
                "# Task: task-force-emit\n", encoding="utf-8"
            )
            report = lint_output_dir(outdir)
            drift = [
                f for f in report.errors
                if f.check == "manifest-drift"
                and (f.path or "").startswith("tasks/")
            ]
            self.assertEqual(drift, [])

    def test_missing_implied_file_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["task-001"])
            # Delete libraries/failures.md that the manifest implies exists.
            (outdir / "libraries" / "failures.md").unlink()
            report = lint_output_dir(outdir)
            drift = [
                f for f in report.errors
                if f.check == "manifest-drift"
                and f.path == "libraries/failures.md"
            ]
            self.assertTrue(drift)


class LintReportOrderingTests(unittest.TestCase):
    def test_exit_code_zero_for_clean(self) -> None:
        self.assertEqual(LintReport(findings=()).exit_code(), 0)

    def test_exit_code_one_for_warn_only(self) -> None:
        report = LintReport(
            findings=(
                LintFinding(severity="warn", check="c", message="m"),
            )
        )
        self.assertEqual(report.exit_code(), 1)

    def test_exit_code_two_for_any_error(self) -> None:
        report = LintReport(
            findings=(
                LintFinding(severity="warn", check="c1", message="m"),
                LintFinding(severity="error", check="c2", message="m"),
            )
        )
        self.assertEqual(report.exit_code(), 2)


if __name__ == "__main__":
    unittest.main()
