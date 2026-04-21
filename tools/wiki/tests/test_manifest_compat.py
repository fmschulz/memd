"""Phase 4 of memd-wiki v2: manifest forward-compat + migrate.

Verifies:
- ``check_manifest_version`` accepts current/older versions, raises
  ``WikiManifestTooNewError`` on future versions.
- ``lint_output_dir`` re-raises the same error so the CLI surfaces a
  clear "upgrade memd-wiki" diagnostic instead of a silent partial lint.
- ``memd-wiki migrate`` upgrades a v1 manifest to v2 in place with
  empty new lanes, is idempotent on v2 manifests, refuses to operate
  on v3 manifests (forward-compat), and supports a `--dry-run` mode.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.cli import main as cli_main  # noqa: E402
from compiled_wiki.compat import (  # noqa: E402
    MAX_KNOWN_MANIFEST_SCHEMA_VERSION,
    WikiManifestTooNewError,
    check_manifest_version,
)
from compiled_wiki.compiler import (  # noqa: E402
    COMPILER_OWNED_PREFIXES,
    HUMAN_OWNED_PREFIXES,
    LLM_AUTHORED_PREFIXES,
    MANIFEST_SCHEMA_VERSION,
)
from compiled_wiki.lint import lint_output_dir  # noqa: E402


def _seed_manifest(outdir: Path, manifest: dict) -> None:
    outdir.mkdir(parents=True, exist_ok=True)
    (outdir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


class CheckManifestVersionTests(unittest.TestCase):
    def test_current_version_returns_int(self) -> None:
        result = check_manifest_version({"schema_version": MANIFEST_SCHEMA_VERSION})
        self.assertEqual(result, MANIFEST_SCHEMA_VERSION)

    def test_older_version_returns_int(self) -> None:
        result = check_manifest_version({"schema_version": 1})
        self.assertEqual(result, 1)

    def test_missing_field_returns_none(self) -> None:
        self.assertIsNone(check_manifest_version({}))

    def test_unparseable_field_returns_none(self) -> None:
        self.assertIsNone(check_manifest_version({"schema_version": "two"}))

    def test_future_version_raises(self) -> None:
        with self.assertRaises(WikiManifestTooNewError) as ctx:
            check_manifest_version(
                {"schema_version": MAX_KNOWN_MANIFEST_SCHEMA_VERSION + 1}
            )
        self.assertEqual(
            ctx.exception.manifest_version,
            MAX_KNOWN_MANIFEST_SCHEMA_VERSION + 1,
        )
        self.assertEqual(
            ctx.exception.client_max,
            MAX_KNOWN_MANIFEST_SCHEMA_VERSION,
        )
        self.assertIn("upgrade memd-wiki", str(ctx.exception))

    def test_non_dict_returns_none(self) -> None:
        self.assertIsNone(check_manifest_version(None))
        self.assertIsNone(check_manifest_version("not a dict"))  # type: ignore[arg-type]


class LintForwardCompatTests(unittest.TestCase):
    def test_lint_raises_on_future_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_manifest(
                outdir,
                {
                    "schema_version": MANIFEST_SCHEMA_VERSION + 1,
                    "compiler_owned_prefixes": list(COMPILER_OWNED_PREFIXES),
                    "project_id": "memd",
                    "task_ids": [],
                },
            )
            with self.assertRaises(WikiManifestTooNewError):
                lint_output_dir(outdir)

    def test_lint_tolerates_missing_schema_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_manifest(
                outdir,
                {
                    "compiler_owned_prefixes": list(COMPILER_OWNED_PREFIXES),
                    "project_id": "memd",
                    "task_ids": [],
                },
            )
            # No raise; the lint may emit other findings but not on
            # the manifest version axis.
            report = lint_output_dir(outdir)
            self.assertNotIn(
                "manifest-too-new",
                {f.check for f in report.findings},
            )


class CliMigrateTests(unittest.TestCase):
    def _seed_v1(self, outdir: Path) -> None:
        _seed_manifest(
            outdir,
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
                "project_id": "memd",
                "task_ids": ["task-1"],
            },
        )

    def test_migrate_v1_to_v2_in_place(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            self._seed_v1(outdir)
            stderr = StringIO()
            with patch.object(sys, "stderr", stderr):
                exit_code = cli_main(
                    [
                        "migrate",
                        "--output-dir",
                        str(outdir),
                        "--config-start",
                        tmp,  # avoid finding any real .memd/config.json
                    ]
                )
            self.assertEqual(exit_code, 0)
            manifest = json.loads(
                (outdir / "manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(
                manifest["llm_authored_prefixes"], list(LLM_AUTHORED_PREFIXES)
            )
            self.assertEqual(
                manifest["human_owned_prefixes"], list(HUMAN_OWNED_PREFIXES)
            )
            self.assertEqual(manifest["concept_pages"], [])
            # Unrelated fields preserved.
            self.assertEqual(manifest["project_id"], "memd")
            self.assertEqual(manifest["task_ids"], ["task-1"])
            self.assertIn("migrated manifest", stderr.getvalue())

    def test_migrate_is_idempotent_on_v2_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_manifest(
                outdir,
                {
                    "schema_version": 2,
                    "compiler_owned_prefixes": list(COMPILER_OWNED_PREFIXES),
                    "llm_authored_prefixes": list(LLM_AUTHORED_PREFIXES),
                    "human_owned_prefixes": list(HUMAN_OWNED_PREFIXES),
                    "project_id": "memd",
                    "task_ids": [],
                    "concept_pages": [],
                },
            )
            before = (outdir / "manifest.json").read_text(encoding="utf-8")
            stderr = StringIO()
            with patch.object(sys, "stderr", stderr):
                exit_code = cli_main(
                    [
                        "migrate",
                        "--output-dir",
                        str(outdir),
                        "--config-start",
                        tmp,
                    ]
                )
            after = (outdir / "manifest.json").read_text(encoding="utf-8")
            self.assertEqual(exit_code, 0)
            self.assertEqual(before, after)
            self.assertIn("nothing to do", stderr.getvalue())

    def test_migrate_refuses_future_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            _seed_manifest(
                outdir,
                {
                    "schema_version": MANIFEST_SCHEMA_VERSION + 1,
                    "compiler_owned_prefixes": list(COMPILER_OWNED_PREFIXES),
                    "project_id": "memd",
                    "task_ids": [],
                },
            )
            stderr = StringIO()
            with patch.object(sys, "stderr", stderr):
                exit_code = cli_main(
                    [
                        "migrate",
                        "--output-dir",
                        str(outdir),
                        "--config-start",
                        tmp,
                    ]
                )
            self.assertEqual(exit_code, 2)
            self.assertIn("newer than this build's max", stderr.getvalue())

    def test_migrate_dry_run_does_not_overwrite(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "compiled_wiki"
            self._seed_v1(outdir)
            before = (outdir / "manifest.json").read_text(encoding="utf-8")
            stdout = StringIO()
            stderr = StringIO()
            with patch.object(sys, "stdout", stdout), patch.object(
                sys, "stderr", stderr
            ):
                exit_code = cli_main(
                    [
                        "migrate",
                        "--output-dir",
                        str(outdir),
                        "--config-start",
                        tmp,
                        "--dry-run",
                    ]
                )
            after = (outdir / "manifest.json").read_text(encoding="utf-8")
            self.assertEqual(exit_code, 0)
            self.assertEqual(before, after)
            stdout_text = stdout.getvalue()
            self.assertIn('"schema_version": 2', stdout_text)
            self.assertIn('"llm_authored_prefixes"', stdout_text)


if __name__ == "__main__":
    unittest.main()
