from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .compiler import BuildConfig, build_wiki
from .config_loader import ConfigLoadError, DiscoveredConfig, load_config

DEFAULT_MEMD_URL = "http://127.0.0.1:8787/mcp"
DEFAULT_MAX_TASKS = 25
DEFAULT_LIBRARY_K = 20
DEFAULT_OUTPUT_SUBDIR = "compiled_wiki"
DEFAULT_TIMEOUT = 30.0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Build a compiled markdown wiki from memd project state. "
            "Missing flags fall through to the nearest ancestor "
            "`.memd/config.json` (wiki subsection and top-level "
            "tenant_id/project_id) before built-in defaults."
        )
    )
    parser.add_argument(
        "--memd-url",
        default=None,
        help=(
            "HTTP MCP endpoint for the memd daemon. "
            f"Default: `wiki.memd_url` from .memd/config.json, else "
            f"{DEFAULT_MEMD_URL!r}."
        ),
    )
    parser.add_argument(
        "--tenant-id",
        default=None,
        help=(
            "memd tenant_id to read from. Required unless set via "
            "`.memd/config.json` top-level `tenant_id`."
        ),
    )
    parser.add_argument(
        "--project-id",
        default=None,
        help=(
            "memd project_id to compile. Required unless set via "
            "`.memd/config.json` top-level `project_id`."
        ),
    )
    parser.add_argument(
        "--output-dir",
        default=None,
        help=(
            "Directory where compiled markdown files will be written. "
            "Default: `wiki.outdir` from .memd/config.json (resolved "
            "against the project root that owns the config), else "
            f"./{DEFAULT_OUTPUT_SUBDIR}/ under the current working directory."
        ),
    )
    parser.add_argument(
        "--max-tasks",
        type=int,
        default=None,
        help=(
            "Maximum number of task pages to compile from project_brief "
            "source_task_ids. Default: `wiki.max_tasks` from "
            f".memd/config.json, else {DEFAULT_MAX_TASKS}."
        ),
    )
    parser.add_argument(
        "--library-k",
        type=int,
        default=None,
        help=(
            "Maximum items to request for each digest-backed library page. "
            f"Default: `wiki.library_k` from .memd/config.json, else "
            f"{DEFAULT_LIBRARY_K}."
        ),
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=(
            "HTTP timeout in seconds for MCP requests. "
            f"Default: {DEFAULT_TIMEOUT}."
        ),
    )
    parser.add_argument(
        "--config-start",
        type=Path,
        default=None,
        help=(
            "Directory to start the `.memd/config.json` ancestor search "
            "from. Defaults to the current working directory."
        ),
    )
    return parser.parse_args(argv)


def _normalize(raw: str | None) -> str | None:
    """Whitespace-normalize a CLI-supplied identifier so empty strings are treated as missing."""
    if raw is None:
        return None
    stripped = raw.strip()
    return stripped or None


def resolve_build_config(
    args: argparse.Namespace,
    discovered: DiscoveredConfig,
) -> BuildConfig:
    """Merge CLI flags over ``discovered`` config and fill hardcoded defaults."""
    tenant_id = _normalize(args.tenant_id) or discovered.tenant_id
    project_id = _normalize(args.project_id) or discovered.project_id
    if not tenant_id or not project_id:
        source_note = (
            f" (searched from {args.config_start or Path.cwd()})"
            if discovered.source_path is None
            else f" (read {discovered.source_path})"
        )
        missing = [
            name
            for name, value in (("tenant_id", tenant_id), ("project_id", project_id))
            if not value
        ]
        raise SystemExit(
            "memd-wiki: error: "
            + ", ".join(missing)
            + " must be set via CLI flag or .memd/config.json"
            + source_note
        )

    if args.output_dir is not None:
        output_dir = Path(args.output_dir)
    elif discovered.outdir is not None:
        output_dir = discovered.outdir
    else:
        output_dir = Path.cwd() / DEFAULT_OUTPUT_SUBDIR

    memd_url = args.memd_url or discovered.memd_url or DEFAULT_MEMD_URL
    max_tasks = args.max_tasks if args.max_tasks is not None else (
        discovered.max_tasks if discovered.max_tasks is not None else DEFAULT_MAX_TASKS
    )
    library_k = args.library_k if args.library_k is not None else (
        discovered.library_k if discovered.library_k is not None else DEFAULT_LIBRARY_K
    )
    return BuildConfig(
        memd_url=memd_url,
        tenant_id=tenant_id,
        project_id=project_id,
        output_dir=output_dir,
        max_tasks=max_tasks,
        library_k=library_k,
        timeout=args.timeout,
    )


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        discovered = load_config(args.config_start)
    except ConfigLoadError as exc:
        print(f"memd-wiki: error: {exc}", file=sys.stderr)
        return 2

    config = resolve_build_config(args, discovered)
    if discovered.source_path is not None:
        print(
            f"memd-wiki: using config {discovered.source_path}",
            file=sys.stderr,
        )
    result = build_wiki(config)
    print(
        f"compiled wiki written to {result.output_dir} "
        f"(written={result.written_files}, unchanged={result.unchanged_files}, "
        f"tasks={result.task_count}, log_entries={result.log_entry_count})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
