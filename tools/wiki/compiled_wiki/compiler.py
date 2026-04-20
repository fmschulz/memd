from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .containment import (
    check_outdir_containment,
    reject_if_any_symlink_inside_outdir,
)
from .mcp_client import McpHttpClient
from .render import (
    artifact_heading,
    artifact_summary,
    render_index,
    render_library_page,
    render_log_page,
    render_project_page,
    render_task_page,
    task_title,
    task_updated_at,
)


@dataclass
class BuildConfig:
    memd_url: str
    tenant_id: str
    project_id: str
    output_dir: Path
    max_tasks: int = 25
    library_k: int = 20
    timeout: float = 30.0
    # If non-empty, refuse to build when output_dir is inside any of
    # these paths. Ported from the Rust export-markdown containment
    # guard; see compiled_wiki.containment. The CLI populates this via
    # resolve_forbidden_data_dirs. Leave empty to skip the guard in
    # library use.
    forbidden_data_dirs: list[Path] = field(default_factory=list)


@dataclass
class BuildResult:
    written_files: int
    unchanged_files: int
    task_count: int
    log_entry_count: int
    output_dir: Path


def build_wiki(config: BuildConfig) -> BuildResult:
    outdir_abs = check_outdir_containment(
        config.output_dir,
        config.forbidden_data_dirs,
    )
    client = McpHttpClient(url=config.memd_url, timeout=config.timeout)

    project_payload = client.call_tool(
        "context.brief_project",
        {
            "tenant_id": config.tenant_id,
            "project_id": config.project_id,
            "k": max(config.max_tasks, 10),
        },
    )

    libraries = {
        "failures": client.call_tool(
            "artifact.find_failures",
            {
                "tenant_id": config.tenant_id,
                "project_id": config.project_id,
                "k": config.library_k,
            },
        ),
        "decisions": client.call_tool(
            "artifact.find_decisions",
            {
                "tenant_id": config.tenant_id,
                "project_id": config.project_id,
                "k": config.library_k,
            },
        ),
        "evidence": client.call_tool(
            "artifact.find_evidence",
            {
                "tenant_id": config.tenant_id,
                "project_id": config.project_id,
                "k": config.library_k,
            },
        ),
        "highlights": client.call_tool(
            "artifact.find_highlights",
            {
                "tenant_id": config.tenant_id,
                "project_id": config.project_id,
                "k": config.library_k,
            },
        ),
    }

    task_ids = project_payload["brief"].get("source_task_ids", [])[: config.max_tasks]
    tasks: list[dict[str, Any]] = []
    for task_id in task_ids:
        resume_payload = client.call_tool(
            "task.resume",
            {"tenant_id": config.tenant_id, "task_id": task_id, "k": 8},
        )
        thread_payload = client.call_tool(
            "artifact.list_thread",
            {"tenant_id": config.tenant_id, "thread_id": task_id},
        )
        tasks.append(
            {
                "task_id": task_id,
                "resume_payload": resume_payload,
                "resume": resume_payload["resume"],
                "resume_artifact": resume_payload["artifact"],
                "thread": thread_payload,
            }
        )

    tasks.sort(key=lambda item: task_updated_at(item["resume"]), reverse=True)
    snapshot_at_ms = determine_snapshot_timestamp(project_payload, libraries, tasks)
    log_entries = build_log_entries(tasks)

    written_files = 0
    unchanged_files = 0

    config.output_dir.mkdir(parents=True, exist_ok=True)
    (config.output_dir / "projects").mkdir(parents=True, exist_ok=True)
    (config.output_dir / "tasks").mkdir(parents=True, exist_ok=True)
    (config.output_dir / "libraries").mkdir(parents=True, exist_ok=True)

    files = {
        config.output_dir / "index.md": render_index(
            config.tenant_id, config.project_id, snapshot_at_ms, tasks
        ),
        config.output_dir / "log.md": render_log_page(
            config.tenant_id, config.project_id, snapshot_at_ms, log_entries
        ),
        config.output_dir / "projects" / f"{config.project_id}.md": render_project_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            project_payload,
            tasks,
            libraries,
        ),
        config.output_dir / "libraries" / "failures.md": render_library_page(
            "failures", config.project_id, snapshot_at_ms, libraries["failures"]
        ),
        config.output_dir / "libraries" / "decisions.md": render_library_page(
            "decisions", config.project_id, snapshot_at_ms, libraries["decisions"]
        ),
        config.output_dir / "libraries" / "evidence.md": render_library_page(
            "evidence", config.project_id, snapshot_at_ms, libraries["evidence"]
        ),
        config.output_dir / "libraries" / "highlights.md": render_library_page(
            "highlights", config.project_id, snapshot_at_ms, libraries["highlights"]
        ),
        config.output_dir / "manifest.json": render_manifest(
            config, snapshot_at_ms, tasks, log_entries, project_payload
        ),
    }

    for task in tasks:
        files[config.output_dir / "tasks" / f"{task['task_id']}.md"] = render_task_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            task,
        )

    for path, content in files.items():
        changed = write_text_if_changed(path, content, outdir_abs=outdir_abs)
        if changed:
            written_files += 1
        else:
            unchanged_files += 1

    return BuildResult(
        written_files=written_files,
        unchanged_files=unchanged_files,
        task_count=len(tasks),
        log_entry_count=len(log_entries),
        output_dir=config.output_dir,
    )


def build_log_entries(tasks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    for task_payload in tasks:
        title = task_title(task_payload["resume"])
        for artifact in task_payload["thread"].get("artifacts", []):
            if artifact.get("artifact_kind") == "digest":
                continue
            entries.append(
                {
                    "timestamp_created": int(artifact.get("timestamp_created") or 0),
                    "task_id": task_payload["task_id"],
                    "artifact_id": artifact.get("artifact_id", "unknown"),
                    "heading": f"{artifact_heading(artifact)} | {title}",
                    "summary": artifact_summary(artifact),
                }
            )
    entries.sort(key=lambda entry: entry["timestamp_created"], reverse=True)
    return entries


def render_manifest(
    config: BuildConfig,
    snapshot_at_ms: int,
    tasks: list[dict[str, Any]],
    log_entries: list[dict[str, Any]],
    project_payload: dict[str, Any],
) -> str:
    manifest = {
        "source_snapshot_at_ms": snapshot_at_ms,
        "memd_url": config.memd_url,
        "tenant_id": config.tenant_id,
        "project_id": config.project_id,
        "task_count": len(tasks),
        "log_entry_count": len(log_entries),
        "project_digest_artifact_id": project_payload["artifact"].get("artifact_id"),
        "project_trust_tier": project_payload.get("trust_tier"),
        "task_ids": [task["task_id"] for task in tasks],
    }
    return json.dumps(manifest, indent=2, sort_keys=True) + "\n"


def determine_snapshot_timestamp(
    project_payload: dict[str, Any],
    libraries: dict[str, dict[str, Any]],
    tasks: list[dict[str, Any]],
) -> int:
    timestamps: list[int] = []

    project_artifact = project_payload.get("artifact", {})
    timestamps.append(int(project_artifact.get("source_updated_at_ms") or 0))

    for payload in libraries.values():
        artifact = payload.get("artifact", {})
        timestamps.append(int(artifact.get("source_updated_at_ms") or 0))

    for task_payload in tasks:
        resume_task = task_payload.get("resume", {}).get("task", {})
        timestamps.append(int(resume_task.get("updated_at_ms") or 0))
        for artifact in task_payload.get("thread", {}).get("artifacts", []):
            if artifact.get("artifact_kind") == "digest":
                continue
            timestamps.append(int(artifact.get("timestamp_created") or 0))

    return max(timestamps) if timestamps else int(time.time() * 1000)


def write_text_if_changed(
    path: Path,
    content: str,
    *,
    outdir_abs: Path | None = None,
) -> bool:
    if outdir_abs is not None:
        reject_if_any_symlink_inside_outdir(path, outdir_abs)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        existing = path.read_text(encoding="utf-8")
        if existing == content:
            return False
    path.write_text(content, encoding="utf-8")
    return True
