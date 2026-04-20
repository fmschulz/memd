from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .compiler import BuildConfig, build_wiki
from .config_loader import ConfigLoadError, DiscoveredConfig, load_config
from .containment import OutdirContainmentError, resolve_forbidden_data_dirs
from .lint import LintReport, lint_output_dir

DEFAULT_MEMD_URL = "http://127.0.0.1:8787/mcp"
DEFAULT_MAX_TASKS = 25
DEFAULT_LIBRARY_K = 20
DEFAULT_OUTPUT_SUBDIR = "compiled_wiki"
DEFAULT_TIMEOUT = 30.0


def _add_shared_config_args(parser: argparse.ArgumentParser) -> None:
    """CLI flags shared by build and lint for config discovery."""
    parser.add_argument(
        "--tenant-id",
        default=None,
        help=(
            "memd tenant_id (build only; required unless set via "
            "`.memd/config.json` top-level `tenant_id`)."
        ),
    )
    parser.add_argument(
        "--project-id",
        default=None,
        help=(
            "memd project_id (build only; required unless set via "
            "`.memd/config.json` top-level `project_id`)."
        ),
    )
    parser.add_argument(
        "--output-dir",
        default=None,
        help=(
            "Directory of the compiled wiki. Default: `wiki.outdir` from "
            ".memd/config.json (resolved against the project root that "
            f"owns the config), else ./{DEFAULT_OUTPUT_SUBDIR}/ under CWD."
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


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="memd-wiki",
        description=(
            "memd-wiki: compile and lint a Karpathy-style markdown wiki "
            "over memd project state (MCP HTTP)."
        ),
    )
    subparsers = parser.add_subparsers(dest="command")
    _add_build_subparser(subparsers)
    _add_lint_subparser(subparsers)

    args = parser.parse_args(argv)
    # Backwards-compat: if no subcommand and no subcommand-specific
    # flags were given, default to `build` for the simple first-run
    # ergonomics ("memd-wiki" with a config file does the right thing).
    if args.command is None:
        args.command = "build"
    return args


def _add_build_subparser(subparsers: argparse._SubParsersAction) -> None:
    build = subparsers.add_parser(
        "build",
        help="Compile a markdown wiki from live memd project state.",
        description=(
            "Compile a markdown wiki. Missing flags fall through to the "
            "nearest ancestor `.memd/config.json` (wiki subsection and "
            "top-level tenant_id/project_id) before built-in defaults."
        ),
    )
    _add_shared_config_args(build)
    build.add_argument(
        "--memd-url",
        default=None,
        help=(
            f"HTTP MCP endpoint. Default: `wiki.memd_url` from config, "
            f"else {DEFAULT_MEMD_URL!r}."
        ),
    )
    build.add_argument(
        "--max-tasks",
        type=int,
        default=None,
        help=(
            f"Max task pages from project_brief.source_task_ids. "
            f"Default: `wiki.max_tasks` from config, else {DEFAULT_MAX_TASKS}."
        ),
    )
    build.add_argument(
        "--library-k",
        type=int,
        default=None,
        help=(
            f"Max items per digest-backed library page. Default: "
            f"`wiki.library_k` from config, else {DEFAULT_LIBRARY_K}."
        ),
    )
    build.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=f"HTTP timeout for MCP requests. Default: {DEFAULT_TIMEOUT}s.",
    )
    build.add_argument(
        "--data-dir",
        type=Path,
        default=None,
        help=(
            "Explicit memd data directory. When set, the containment "
            "guard refuses only this path (overrides "
            "$HOME/.memd/data + tenant_scope discovery)."
        ),
    )


def _add_lint_subparser(subparsers: argparse._SubParsersAction) -> None:
    lint = subparsers.add_parser(
        "lint",
        help="Run 5 health checks over a compiled wiki output tree.",
        description=(
            "Run lint. Exit code 0=clean, 1=warnings only, 2=errors. "
            "Output is one finding per line in a stable format suitable "
            "for CI diffing."
        ),
    )
    _add_shared_config_args(lint)
    lint.add_argument(
        "--check-staleness",
        action="store_true",
        help=(
            "Query memd via MCP HTTP and warn when a task page snapshot "
            "is older than the task's current updated_at_ms. Default is "
            "offline filesystem-only; this flag adds one `task.resume` "
            "call per emitted task page."
        ),
    )
    lint.add_argument(
        "--memd-url",
        default=None,
        help=(
            f"HTTP MCP endpoint (lint --check-staleness only). Default: "
            f"`wiki.memd_url` from config, else {DEFAULT_MEMD_URL!r}."
        ),
    )
    lint.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=f"HTTP timeout for MCP requests. Default: {DEFAULT_TIMEOUT}s.",
    )


def _normalize(raw: str | None) -> str | None:
    if raw is None:
        return None
    stripped = raw.strip()
    return stripped or None


def _resolve_output_dir(
    cli_value: str | Path | None,
    discovered: DiscoveredConfig,
) -> Path:
    if cli_value is not None:
        return Path(cli_value)
    if discovered.outdir is not None:
        return discovered.outdir
    return Path.cwd() / DEFAULT_OUTPUT_SUBDIR


def resolve_build_config(
    args: argparse.Namespace,
    discovered: DiscoveredConfig,
) -> BuildConfig:
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

    output_dir = _resolve_output_dir(args.output_dir, discovered)
    memd_url = args.memd_url or discovered.memd_url or DEFAULT_MEMD_URL
    max_tasks = args.max_tasks if args.max_tasks is not None else (
        discovered.max_tasks if discovered.max_tasks is not None else DEFAULT_MAX_TASKS
    )
    library_k = args.library_k if args.library_k is not None else (
        discovered.library_k if discovered.library_k is not None else DEFAULT_LIBRARY_K
    )
    forbidden_data_dirs = resolve_forbidden_data_dirs(
        explicit=args.data_dir,
        start=args.config_start or Path.cwd(),
    )
    return BuildConfig(
        memd_url=memd_url,
        tenant_id=tenant_id,
        project_id=project_id,
        output_dir=output_dir,
        max_tasks=max_tasks,
        library_k=library_k,
        timeout=args.timeout,
        forbidden_data_dirs=forbidden_data_dirs,
    )


def _run_build(args: argparse.Namespace, discovered: DiscoveredConfig) -> int:
    config = resolve_build_config(args, discovered)
    if discovered.source_path is not None:
        print(
            f"memd-wiki: using config {discovered.source_path}",
            file=sys.stderr,
        )
    try:
        result = build_wiki(config)
    except OutdirContainmentError as exc:
        print(f"memd-wiki: error: {exc}", file=sys.stderr)
        return 2
    print(
        f"compiled wiki written to {result.output_dir} "
        f"(written={result.written_files}, unchanged={result.unchanged_files}, "
        f"tasks={result.task_count}, log_entries={result.log_entry_count})"
    )
    return 0


def _run_lint(args: argparse.Namespace, discovered: DiscoveredConfig) -> int:
    outdir = _resolve_output_dir(args.output_dir, discovered)
    if not outdir.is_dir():
        print(
            f"memd-wiki: error: lint target {outdir} is not a directory",
            file=sys.stderr,
        )
        return 2
    lookup_latest_ms = None
    if args.check_staleness:
        lookup_latest_ms = _build_staleness_lookup(args, discovered)
    report: LintReport = lint_output_dir(
        outdir, lookup_latest_ms=lookup_latest_ms
    )
    for finding in report.findings:
        print(finding.render())
    summary = (
        f"lint: {len(report.errors)} errors, {len(report.warnings)} warnings"
    )
    print(summary, file=sys.stderr)
    return report.exit_code()


def _build_staleness_lookup(
    args: argparse.Namespace,
    discovered: DiscoveredConfig,
):
    """Build a memd-backed `lookup_latest_ms(task_id)` closure.

    One ``task.resume`` call per distinct task_id, memoised across the
    lint run. Daemon errors degrade gracefully: the first failure logs
    to stderr and subsequent lookups return None so the lint falls back
    to file-only behavior for the remaining pages.
    """
    from .mcp_client import McpHttpClient

    tenant_id = _normalize(args.tenant_id) or discovered.tenant_id
    project_id = _normalize(args.project_id) or discovered.project_id
    if not tenant_id or not project_id:
        print(
            "memd-wiki: warning: --check-staleness requires tenant_id and "
            "project_id; skipping staleness check",
            file=sys.stderr,
        )
        return lambda _task_id: None

    memd_url = args.memd_url or discovered.memd_url or DEFAULT_MEMD_URL
    client = McpHttpClient(memd_url, timeout=args.timeout)
    try:
        client.initialize()
    except Exception as exc:  # noqa: BLE001 — daemon is an opaque dep here.
        print(
            f"memd-wiki: warning: could not initialize memd client at "
            f"{memd_url}: {exc}; skipping staleness check",
            file=sys.stderr,
        )
        return lambda _task_id: None

    cache: dict[str, int | None] = {}
    disabled = {"flag": False}

    def _lookup(task_id: str) -> int | None:
        if disabled["flag"]:
            return None
        if task_id in cache:
            return cache[task_id]
        try:
            payload = client.call_tool(
                "task.resume",
                {
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "task_id": task_id,
                },
            )
        except Exception as exc:  # noqa: BLE001
            print(
                f"memd-wiki: warning: task.resume failed for {task_id}: "
                f"{exc}; disabling staleness checks for remainder of run",
                file=sys.stderr,
            )
            disabled["flag"] = True
            cache[task_id] = None
            return None
        task = (payload or {}).get("task", {}) if isinstance(payload, dict) else {}
        latest = (
            task.get("updated_at_ms")
            or task.get("finished_at_ms")
            or task.get("started_at_ms")
        )
        try:
            latest_ms = int(latest) if latest is not None else None
        except (TypeError, ValueError):
            latest_ms = None
        cache[task_id] = latest_ms
        return latest_ms

    return _lookup


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        discovered = load_config(args.config_start)
    except ConfigLoadError as exc:
        print(f"memd-wiki: error: {exc}", file=sys.stderr)
        return 2

    if args.command == "lint":
        return _run_lint(args, discovered)
    return _run_build(args, discovered)


if __name__ == "__main__":
    raise SystemExit(main())
