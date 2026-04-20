"""Precedence tests for CLI flag > config file > hardcoded defaults.

Exercises ``compiled_wiki.cli.resolve_build_config`` directly, without
actually running the compiler (which needs a live memd server).
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.cli import (  # noqa: E402
    DEFAULT_LIBRARY_K,
    DEFAULT_MAX_TASKS,
    DEFAULT_MEMD_URL,
    DEFAULT_OUTPUT_SUBDIR,
    DEFAULT_TIMEOUT,
    resolve_build_config,
)
from compiled_wiki.config_loader import DiscoveredConfig  # noqa: E402


def _args(**overrides: object) -> argparse.Namespace:
    base = dict(
        memd_url=None,
        tenant_id=None,
        project_id=None,
        output_dir=None,
        max_tasks=None,
        library_k=None,
        timeout=DEFAULT_TIMEOUT,
        config_start=None,
        data_dir=None,
    )
    base.update(overrides)
    return argparse.Namespace(**base)


def _discovered(**overrides: object) -> DiscoveredConfig:
    base = dict(
        source_path=Path("/tmp/fake/.memd/config.json"),
        tenant_id=None,
        project_id=None,
        outdir=None,
        max_tasks=None,
        library_k=None,
        memd_url=None,
    )
    base.update(overrides)
    return DiscoveredConfig(**base)  # type: ignore[arg-type]


class CliWinsOverConfigTests(unittest.TestCase):
    def test_cli_tenant_beats_config_tenant(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="cli", project_id="p"),
            _discovered(tenant_id="config", project_id="config"),
        )
        self.assertEqual(cfg.tenant_id, "cli")

    def test_cli_project_beats_config_project(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="cli"),
            _discovered(tenant_id="config", project_id="config"),
        )
        self.assertEqual(cfg.project_id, "cli")

    def test_cli_memd_url_beats_config_memd_url(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p", memd_url="http://x/mcp"),
            _discovered(tenant_id="t", project_id="p", memd_url="http://y/mcp"),
        )
        self.assertEqual(cfg.memd_url, "http://x/mcp")

    def test_cli_max_tasks_beats_config_max_tasks(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p", max_tasks=3),
            _discovered(tenant_id="t", project_id="p", max_tasks=99),
        )
        self.assertEqual(cfg.max_tasks, 3)

    def test_cli_output_dir_beats_config_outdir(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p", output_dir="/tmp/from-cli"),
            _discovered(tenant_id="t", project_id="p", outdir=Path("/tmp/from-config")),
        )
        self.assertEqual(cfg.output_dir, Path("/tmp/from-cli"))

    def test_cli_library_k_beats_config_library_k(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p", library_k=4),
            _discovered(tenant_id="t", project_id="p", library_k=50),
        )
        self.assertEqual(cfg.library_k, 4)

    def test_whitespace_only_cli_tenant_treated_as_missing(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="   ", project_id="p"),
            _discovered(tenant_id="config-tenant", project_id="config-project"),
        )
        self.assertEqual(cfg.tenant_id, "config-tenant")


class ConfigWinsOverDefaultsTests(unittest.TestCase):
    def test_config_memd_url_used_when_cli_missing(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p"),
            _discovered(tenant_id="t", project_id="p", memd_url="http://cfg/mcp"),
        )
        self.assertEqual(cfg.memd_url, "http://cfg/mcp")

    def test_config_max_tasks_used_when_cli_missing(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p"),
            _discovered(tenant_id="t", project_id="p", max_tasks=77),
        )
        self.assertEqual(cfg.max_tasks, 77)

    def test_config_outdir_used_when_cli_missing(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p"),
            _discovered(tenant_id="t", project_id="p", outdir=Path("/tmp/cfg-out")),
        )
        self.assertEqual(cfg.output_dir, Path("/tmp/cfg-out"))


class DefaultsWhenNothingSetTests(unittest.TestCase):
    def test_defaults_applied(self) -> None:
        cfg = resolve_build_config(
            _args(tenant_id="t", project_id="p"),
            _discovered(tenant_id="t", project_id="p"),
        )
        self.assertEqual(cfg.memd_url, DEFAULT_MEMD_URL)
        self.assertEqual(cfg.max_tasks, DEFAULT_MAX_TASKS)
        self.assertEqual(cfg.library_k, DEFAULT_LIBRARY_K)
        self.assertEqual(cfg.output_dir, Path.cwd() / DEFAULT_OUTPUT_SUBDIR)


class MissingRequiredTests(unittest.TestCase):
    def test_system_exit_when_tenant_missing(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            resolve_build_config(
                _args(project_id="p"),
                _discovered(),
            )
        msg = str(ctx.exception)
        self.assertIn("tenant_id", msg)

    def test_system_exit_when_project_missing(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            resolve_build_config(
                _args(tenant_id="t"),
                _discovered(),
            )
        msg = str(ctx.exception)
        self.assertIn("project_id", msg)

    def test_both_missing_listed(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            resolve_build_config(_args(), _discovered())
        msg = str(ctx.exception)
        self.assertIn("tenant_id", msg)
        self.assertIn("project_id", msg)


class EndToEndArgvTests(unittest.TestCase):
    """Uses parse_args + resolve_build_config through a real config file."""

    def test_cli_argv_with_discovered_config(self) -> None:
        from compiled_wiki.cli import parse_args
        from compiled_wiki.config_loader import load_config

        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "config.json").write_text(
                '{"tenant_id": "memd", "project_id": "memd", '
                '"wiki": {"max_tasks": 11, "library_k": 3}}',
                encoding="utf-8",
            )
            argv = ["--config-start", str(project), "--max-tasks", "5"]
            args = parse_args(argv)
            discovered = load_config(args.config_start)
            cfg = resolve_build_config(args, discovered)
            self.assertEqual(cfg.tenant_id, "memd")      # from config
            self.assertEqual(cfg.project_id, "memd")     # from config
            self.assertEqual(cfg.max_tasks, 5)           # CLI beats 11
            self.assertEqual(cfg.library_k, 3)           # config beats default


if __name__ == "__main__":
    unittest.main()
