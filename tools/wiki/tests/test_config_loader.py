from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.config_loader import (  # noqa: E402
    ConfigLoadError,
    DiscoveredConfig,
    find_config_file,
    load_config,
)


class FindConfigFileTests(unittest.TestCase):
    def test_returns_none_when_no_config_anywhere(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            start = root / "a" / "b" / "c"
            start.mkdir(parents=True)
            self.assertIsNone(find_config_file(start))

    def test_finds_nearest_ancestor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            project = root / "project"
            nested = project / "a" / "b"
            nested.mkdir(parents=True)
            (project / ".memd").mkdir()
            config = project / ".memd" / "config.json"
            config.write_text('{"tenant_id": "t", "project_id": "p"}', encoding="utf-8")
            self.assertEqual(find_config_file(nested).resolve(), config.resolve())

    def test_returns_inner_when_both_inner_and_outer_exist(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            outer = root
            inner = root / "inner"
            inner.mkdir()
            (outer / ".memd").mkdir()
            (outer / ".memd" / "config.json").write_text(
                '{"tenant_id": "outer", "project_id": "o"}', encoding="utf-8"
            )
            (inner / ".memd").mkdir()
            (inner / ".memd" / "config.json").write_text(
                '{"tenant_id": "inner", "project_id": "i"}', encoding="utf-8"
            )
            found = find_config_file(inner).resolve()
            self.assertEqual(found, (inner / ".memd" / "config.json").resolve())


class LoadConfigTests(unittest.TestCase):
    def test_empty_when_no_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            start = Path(tmp) / "a"
            start.mkdir()
            result = load_config(start)
            self.assertEqual(result, DiscoveredConfig.empty())

    def test_parses_top_level_fields(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"tenant_id": "memd", "project_id": "memd"}),
                encoding="utf-8",
            )
            result = load_config(project)
            self.assertEqual(result.tenant_id, "memd")
            self.assertEqual(result.project_id, "memd")
            self.assertIsNone(result.outdir)
            self.assertIsNone(result.max_tasks)
            self.assertIsNone(result.library_k)
            self.assertIsNone(result.memd_bin)
            self.assertIsNone(result.memd_url)

    def test_parses_wiki_subsection(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps(
                    {
                        "tenant_id": "memd",
                        "project_id": "memd",
                        "wiki": {
                            "outdir": "docs/compiled_wiki",
                            "max_tasks": 42,
                            "library_k": 7,
                            "memd_bin": "/opt/memd/bin/memd",
                        },
                    }
                ),
                encoding="utf-8",
            )
            result = load_config(project)
            self.assertEqual(result.max_tasks, 42)
            self.assertEqual(result.library_k, 7)
            self.assertEqual(result.memd_bin, "/opt/memd/bin/memd")
            # Relative outdir resolves against project root (parent of .memd/)
            self.assertEqual(result.outdir, project / "docs" / "compiled_wiki")

    def test_absolute_outdir_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            abs_out = Path(tmp) / "absolute-output"
            (project / ".memd" / "config.json").write_text(
                json.dumps(
                    {
                        "tenant_id": "memd",
                        "project_id": "memd",
                        "wiki": {"outdir": str(abs_out)},
                    }
                ),
                encoding="utf-8",
            )
            result = load_config(project)
            self.assertEqual(result.outdir, abs_out)

    def test_raises_on_invalid_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text("{not json}", encoding="utf-8")
            with self.assertRaises(ConfigLoadError) as ctx:
                load_config(project)
            self.assertIn("invalid JSON", ctx.exception.reason)

    def test_raises_on_non_object_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text("[1, 2, 3]", encoding="utf-8")
            with self.assertRaises(ConfigLoadError) as ctx:
                load_config(project)
            self.assertIn("must be a JSON object", ctx.exception.reason)

    def test_raises_on_non_object_wiki(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"wiki": "not-an-object"}), encoding="utf-8"
            )
            with self.assertRaises(ConfigLoadError) as ctx:
                load_config(project)
            self.assertIn("`wiki` must be an object", ctx.exception.reason)

    def test_raises_on_negative_max_tasks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"wiki": {"max_tasks": -1}}), encoding="utf-8"
            )
            with self.assertRaises(ConfigLoadError):
                load_config(project)

    def test_raises_on_non_int_library_k(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"wiki": {"library_k": "lots"}}), encoding="utf-8"
            )
            with self.assertRaises(ConfigLoadError):
                load_config(project)

    def test_raises_on_bool_max_tasks(self) -> None:
        """Python's ``isinstance(True, int)`` is True; guard against it."""
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"wiki": {"max_tasks": True}}), encoding="utf-8"
            )
            with self.assertRaises(ConfigLoadError):
                load_config(project)

    def test_empty_string_tenant_is_treated_as_missing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                json.dumps({"tenant_id": "   ", "project_id": "memd"}),
                encoding="utf-8",
            )
            result = load_config(project)
            self.assertIsNone(result.tenant_id)
            self.assertEqual(result.project_id, "memd")


if __name__ == "__main__":
    unittest.main()
