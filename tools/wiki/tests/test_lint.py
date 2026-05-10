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


def _seed_manifest_with_project_page_path(
    outdir: Path,
    *,
    project_id: str,
    project_page_path: str,
) -> None:
    (outdir / "manifest.json").write_text(
        json.dumps(
            {
                "schema_version": 2,
                "compiler_owned_prefixes": [
                    "index.md",
                    "log.md",
                    "manifest.json",
                    "projects/",
                    "tasks/",
                    "libraries/",
                ],
                "project_id": project_id,
                "project_page_path": project_page_path,
                "task_ids": [],
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

    def test_manifest_project_page_path_overrides_raw_project_id_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            _seed_manifest_with_project_page_path(
                outdir,
                project_id="/workspace/projects/csag_test",
                project_page_path="projects/workspace-projects-csag_test.md",
            )
            (outdir / "projects" / "memd.md").unlink()
            (outdir / "projects" / "workspace-projects-csag_test.md").write_text(
                "# project\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            self.assertEqual(
                [f for f in report.errors if f.check == "manifest-drift"],
                [],
            )


class LibraryGroundingTests(unittest.TestCase):
    def test_digest_library_without_grounding_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            # Rewrite failures library to digest-backed without grounding
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n"
                "- Trust tier: `compiled_digest_hint`\n"
                "## Items\n\n"
                "- [../tasks/task-x.md](../tasks/task-x.md) - x\n",
                encoding="utf-8",
            )
            (outdir / "tasks" / "task-x.md").write_text("# t\n", encoding="utf-8")
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.errors}
            self.assertIn("library-missing-grounding", checks)
            self.assertEqual(report.exit_code(), 2)

    def test_empty_digest_library_without_grounding_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=[])
            (outdir / "libraries" / "failures.md").write_text(
                "# failures\n"
                "- Trust tier: `compiled_digest_hint`\n"
                "## Items\n\n",
                encoding="utf-8",
            )
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.errors}
            self.assertNotIn("library-missing-grounding", checks)

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


class TaskSnapshotStaleTests(unittest.TestCase):
    """task-snapshot-stale check (opt-in via lookup_latest_ms callback)."""

    def _seed_task_page(
        self,
        outdir: Path,
        task_id: str,
        *,
        snapshot_iso: str,
        updated_iso: str,
    ) -> None:
        (outdir / "tasks" / f"{task_id}.md").write_text(
            (
                f"# Task: {task_id}\n"
                f"- Task ID: `{task_id}`\n"
                f"- Source snapshot at: `{snapshot_iso}`\n"
                f"- Updated at: `{updated_iso}`\n"
            ),
            encoding="utf-8",
        )

    def test_skip_when_no_lookup(self) -> None:
        """Default behavior: no callback → check is a no-op."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            report = lint_output_dir(outdir)
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_flags_when_memd_is_newer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            # 2026-01-02 00:00:00Z == 1767312000000 ms.
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: 1767312000000
            )
            stale = [f for f in report.warnings if f.check == "task-snapshot-stale"]
            self.assertEqual(len(stale), 1)
            self.assertEqual(stale[0].path, "tasks/t1.md")
            self.assertIn("older than latest canonical source", stale[0].message)
            self.assertEqual(report.exit_code(), 1)

    def test_clean_when_memd_matches_snapshot(self) -> None:
        """Same-ms memd timestamp is NOT stale (strict >)."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: 1767225600000
            )
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_clean_when_memd_is_older(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-02-01 00:00:00Z",
                updated_iso="2026-02-01 00:00:00Z",
            )
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: 1767225600000
            )
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_callback_returning_none_skips(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: None
            )
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_callback_returning_zero_skips(self) -> None:
        """latest<=0 means memd has no timestamp — treat as skip, not flag."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: 0
            )
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_unparseable_snapshot_is_skipped(self) -> None:
        """Page with a non-conforming snapshot string must not flag."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1"])
            self._seed_task_page(
                outdir,
                "t1",
                snapshot_iso="unknown",
                updated_iso="unknown",
            )
            report = lint_output_dir(
                outdir, lookup_latest_ms=lambda _tid: 1767312000000
            )
            stale = [f for f in report.findings if f.check == "task-snapshot-stale"]
            self.assertEqual(stale, [])

    def test_callback_invoked_once_per_task(self) -> None:
        """Ensure the caller's callback is the single oracle per page."""
        calls: list[str] = []

        def _lookup(task_id: str) -> int | None:
            calls.append(task_id)
            return None

        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            outdir.mkdir()
            _seed_base_tree(outdir, task_ids=["t1", "t2"])
            self._seed_task_page(
                outdir, "t1",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            self._seed_task_page(
                outdir, "t2",
                snapshot_iso="2026-01-01 00:00:00Z",
                updated_iso="2026-01-01 00:00:00Z",
            )
            lint_output_dir(outdir, lookup_latest_ms=_lookup)
        self.assertEqual(sorted(calls), ["t1", "t2"])


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
