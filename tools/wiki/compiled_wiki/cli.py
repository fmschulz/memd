from __future__ import annotations

import argparse
from pathlib import Path

from .compiler import BuildConfig, build_wiki


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a compiled markdown wiki from memd project state."
    )
    parser.add_argument(
        "--memd-url",
        default="http://127.0.0.1:8787/mcp",
        help="HTTP MCP endpoint for the memd daemon.",
    )
    parser.add_argument("--tenant-id", required=True, help="memd tenant_id to read from.")
    parser.add_argument("--project-id", required=True, help="memd project_id to compile.")
    parser.add_argument(
        "--output-dir",
        default=str(Path(__file__).resolve().parents[1] / "output"),
        help="Directory where compiled markdown files will be written.",
    )
    parser.add_argument(
        "--max-tasks",
        type=int,
        default=25,
        help="Maximum number of task pages to compile from project_brief source_task_ids.",
    )
    parser.add_argument(
        "--library-k",
        type=int,
        default=20,
        help="Maximum items to request for each digest-backed library page.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=30.0,
        help="HTTP timeout in seconds for MCP requests.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    config = BuildConfig(
        memd_url=args.memd_url,
        tenant_id=args.tenant_id,
        project_id=args.project_id,
        output_dir=Path(args.output_dir),
        max_tasks=args.max_tasks,
        library_k=args.library_k,
        timeout=args.timeout,
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
