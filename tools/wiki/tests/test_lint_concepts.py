"""Phase 3 of memd-wiki v2: concept-* lint checks.

One happy + one false-positive guard per check (plan §5 phase 3):
- concept-missing-grounding (ERROR, paranoid)
- concept-stale (WARN, requires lookup oracle)
- concept-contradicts-canonical (ERROR, syntactic scaffold)
- concept-trust-tier-ungrounded (ERROR, blocks self-labelling)
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.lint import (  # noqa: E402
    DEFAULT_CONCEPT_STALENESS_MS,
    LintFinding,
    lint_output_dir,
)


def _seed_v2_tree(
    outdir: Path,
    *,
    concept_pages: list[dict[str, Any]],
    pages_on_disk: dict[str, str] | None = None,
) -> None:
    """Minimal v2 wiki tree that passes every other check."""
    outdir.mkdir(parents=True, exist_ok=True)
    (outdir / "projects").mkdir(parents=True, exist_ok=True)
    (outdir / "tasks").mkdir(parents=True, exist_ok=True)
    (outdir / "libraries").mkdir(parents=True, exist_ok=True)
    if any(entry.get("path", "").startswith("concepts/") for entry in concept_pages):
        (outdir / "concepts").mkdir(parents=True, exist_ok=True)
    if any(entry.get("path", "").startswith("entities/") for entry in concept_pages):
        (outdir / "entities").mkdir(parents=True, exist_ok=True)

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
                "llm_authored_prefixes": ["concepts/", "entities/"],
                "human_owned_prefixes": ["notes/"],
                "project_id": "memd",
                "task_ids": [],
                "concept_pages": concept_pages,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    (outdir / "index.md").write_text("# idx\n", encoding="utf-8")
    (outdir / "log.md").write_text("# log\n", encoding="utf-8")
    (outdir / "projects" / "memd.md").write_text("# project\n", encoding="utf-8")
    for library in ("failures", "decisions", "evidence", "highlights"):
        (outdir / "libraries" / f"{library}.md").write_text(
            "# lib\n- Trust tier: `canonical_record`\n",
            encoding="utf-8",
        )
    for path, body in (pages_on_disk or {}).items():
        target = outdir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")


class ConceptMissingGroundingTests(unittest.TestCase):
    def test_empty_grounding_emits_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-empty",
                        "path": "concepts/page-empty.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-empty.md": "# Empty grounding page\n",
                },
            )
            report = lint_output_dir(outdir)
            checks = {(f.check, f.path) for f in report.errors}
            self.assertIn(("concept-missing-grounding", "concepts/page-empty.md"), checks)
            self.assertEqual(report.exit_code(), 2)

    def test_well_formed_grounding_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-good",
                        "path": "concepts/page-good.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                            }
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-good.md": "# Good\n",
                },
            )
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.findings}
            self.assertNotIn("concept-missing-grounding", checks)


class ConceptStaleTests(unittest.TestCase):
    def test_stale_page_emits_warn_when_oracle_returns_newer_timestamp(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-stale",
                        "path": "concepts/page-stale.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-A",
                                "artifact_kind": "evidence",
                            }
                        ],
                        # Page is 1970 + 1s; ref is 100 days later.
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-stale.md": "# Stale\n",
                },
            )
            report = lint_output_dir(
                outdir,
                lookup_latest_ms=lambda task_id: (
                    1000 + 100 * 24 * 60 * 60 * 1000
                    if task_id == "task-A"
                    else None
                ),
            )
            checks = {f.check for f in report.warnings}
            self.assertIn("concept-stale", checks)

    def test_recent_page_does_not_warn(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-fresh",
                        "path": "concepts/page-fresh.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-A",
                                "artifact_kind": "evidence",
                            }
                        ],
                        "source_updated_at_ms": 10_000,
                    }
                ],
                pages_on_disk={"concepts/page-fresh.md": "# Fresh\n"},
            )
            # Oracle reports the ref at +1 day — well within the 30-day window.
            report = lint_output_dir(
                outdir,
                lookup_latest_ms=lambda task_id: 10_000 + 24 * 60 * 60 * 1000,
            )
            self.assertNotIn("concept-stale", {f.check for f in report.findings})

    def test_stale_check_skipped_without_oracle(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-no-oracle",
                        "path": "concepts/page-no-oracle.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-A",
                                "artifact_kind": "evidence",
                            }
                        ],
                        "source_updated_at_ms": 1,
                    }
                ],
                pages_on_disk={"concepts/page-no-oracle.md": "# X\n"},
            )
            report = lint_output_dir(outdir)  # no oracle
            self.assertNotIn("concept-stale", {f.check for f in report.findings})


class ConceptContradictsCanonicalTests(unittest.TestCase):
    def test_all_rejected_task_finish_grounding_emits_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-contradict",
                        "path": "concepts/page-contradict.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "rej-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                                "status": "rejected",
                            },
                            {
                                "artifact_id": "rej-2",
                                "task_id": "task-2",
                                "artifact_kind": "task_finish",
                                "status": "rejected",
                            },
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={"concepts/page-contradict.md": "# Bad\n"},
            )
            report = lint_output_dir(outdir)
            self.assertIn(
                "concept-contradicts-canonical",
                {f.check for f in report.errors},
            )

    def test_mixed_grounding_does_not_flag_contradiction(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-mixed",
                        "path": "concepts/page-mixed.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "rej-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                                "status": "rejected",
                            },
                            {
                                "artifact_id": "ok-1",
                                "task_id": "task-2",
                                "artifact_kind": "evidence",
                                "status": "recorded",
                            },
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={"concepts/page-mixed.md": "# Mixed\n"},
            )
            report = lint_output_dir(outdir)
            self.assertNotIn(
                "concept-contradicts-canonical",
                {f.check for f in report.findings},
            )


class ConceptTrustTierUngroundedTests(unittest.TestCase):
    def test_self_labelled_verified_without_footer_emits_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-launder",
                        "path": "concepts/page-launder.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                            }
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-launder.md": (
                        "---\nverified: true\n---\n# Self-labelled\n"
                    ),
                },
            )
            report = lint_output_dir(outdir)
            self.assertIn(
                "concept-trust-tier-ungrounded",
                {f.check for f in report.errors},
            )

    def test_self_labelled_with_verified_by_footer_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-honest",
                        "path": "concepts/page-honest.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                            }
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-honest.md": (
                        "---\nverified: true\n---\n"
                        "# Honest\n\n"
                        "## Verified By\n- Verified by: agent-beta on 2026-04-20\n"
                    ),
                },
            )
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.findings}
            self.assertNotIn("concept-trust-tier-ungrounded", checks)

    def test_unverified_page_does_not_flag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_v2_tree(
                outdir,
                concept_pages=[
                    {
                        "artifact_id": "page-quiet",
                        "path": "concepts/page-quiet.md",
                        "trust_tier": "canonical_record",
                        "artifact_role": "concept",
                        "grounding_refs": [
                            {
                                "artifact_id": "ref-1",
                                "task_id": "task-1",
                                "artifact_kind": "task_finish",
                            }
                        ],
                        "source_updated_at_ms": 1000,
                    }
                ],
                pages_on_disk={
                    "concepts/page-quiet.md": "# Quiet\n\nNo trust label.\n",
                },
            )
            report = lint_output_dir(outdir)
            checks = {f.check for f in report.findings}
            self.assertNotIn("concept-trust-tier-ungrounded", checks)


class ConceptStalenessConstantTests(unittest.TestCase):
    def test_default_window_is_30_days(self) -> None:
        self.assertEqual(
            DEFAULT_CONCEPT_STALENESS_MS,
            30 * 24 * 60 * 60 * 1000,
        )


if __name__ == "__main__":
    unittest.main()
