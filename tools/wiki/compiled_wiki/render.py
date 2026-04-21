from __future__ import annotations

from datetime import datetime, timezone
from typing import Any


def iso_timestamp(timestamp_ms: int | None) -> str:
    if not timestamp_ms:
        return "unknown"
    dt = datetime.fromtimestamp(timestamp_ms / 1000, tz=timezone.utc)
    return dt.strftime("%Y-%m-%d %H:%M:%SZ")


def one_line(text: str | None) -> str:
    if not text:
        return ""
    return " ".join(str(text).split())


def task_title(resume: dict[str, Any]) -> str:
    task = resume.get("task", {})
    return task.get("goal") or task.get("task_id") or "Untitled task"


def task_updated_at(resume: dict[str, Any]) -> int:
    task = resume.get("task", {})
    return int(task.get("updated_at_ms") or task.get("finished_at_ms") or task.get("started_at_ms") or 0)


def artifact_heading(artifact: dict[str, Any]) -> str:
    role = artifact.get("artifact_role")
    kind = artifact.get("artifact_kind", "artifact")
    if role:
        return f"{role} ({kind})"
    return str(kind)


def artifact_summary(artifact: dict[str, Any]) -> str:
    if artifact.get("summary"):
        return one_line(artifact["summary"])

    for key in ("what_worked", "what_failed", "validation", "followups", "blockers"):
        values = artifact.get(key) or []
        if values:
            return one_line("; ".join(str(value) for value in values))

    for key in ("goal", "scientific_question", "motivation", "hypothesis"):
        if artifact.get(key):
            return one_line(artifact[key])

    return ""


def render_grounding_refs(
    refs: list[dict[str, Any]] | None,
    task_link_prefix: str,
) -> list[str]:
    refs = refs or []
    lines: list[str] = []
    for ref in refs:
        task_id = ref.get("task_id", "unknown-task")
        artifact_id = ref.get("artifact_id", "unknown-artifact")
        artifact_kind = ref.get("artifact_kind", "artifact")
        role = ref.get("artifact_role")
        role_suffix = f", role={role}" if role else ""
        lines.append(
            f"- [{task_id}]({task_link_prefix}{task_id}.md) | artifact `{artifact_id}` | {artifact_kind}{role_suffix}"
        )
    return lines


def render_trust_block(payload: dict[str, Any], task_link_prefix: str) -> list[str]:
    trust_tier = payload.get("trust_tier") or "unknown"
    verification_hint = payload.get("verification_hint") or {}
    lines = [
        "## Trust",
        "",
        f"- Trust tier: `{trust_tier}`",
    ]
    if verification_hint:
        lines.append(
            f"- Requires verification: `{verification_hint.get('requires_verification', False)}`"
        )
        reason = one_line(verification_hint.get("reason"))
        if reason:
            lines.append(f"- Reason: {reason}")
    grounding_lines = render_grounding_refs(payload.get("grounding_refs"), task_link_prefix)
    if grounding_lines:
        lines.extend(["", "### Grounded By", ""])
        lines.extend(grounding_lines)
    lines.append("")
    return lines


def render_index(
    tenant_id: str,
    project_id: str,
    snapshot_at_ms: int,
    tasks: list[dict[str, Any]],
) -> str:
    lines = [
        f"# Compiled Wiki: {project_id}",
        "",
        f"- Tenant: `{tenant_id}`",
        f"- Project: `{project_id}`",
        f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`",
        f"- Project page: [projects/{project_id}.md](projects/{project_id}.md)",
        "",
        "## Trust Model",
        "",
        "- Search and digest pages are navigation surfaces.",
        "- Canonical task and artifact records remain the trust anchor.",
        "- Use the grounded links on project, task, and library pages before treating a digest summary as authoritative.",
        "",
        "## Libraries",
        "",
        "- [Failures](libraries/failures.md)",
        "- [Decisions](libraries/decisions.md)",
        "- [Evidence](libraries/evidence.md)",
        "- [Highlights](libraries/highlights.md)",
        "",
        "## Tasks",
        "",
    ]
    for task in tasks:
        title = task_title(task["resume"])
        latest_summary = one_line(task["resume"].get("latest_summary"))
        lines.append(f"- [tasks/{task['task_id']}.md](tasks/{task['task_id']}.md) - {title}")
        if latest_summary:
            lines.append(f"  Latest: {latest_summary}")
    lines.append("")
    lines.append("## Timeline")
    lines.append("")
    lines.append("- [log.md](log.md)")
    lines.append("")
    return "\n".join(lines)


def render_project_page(
    tenant_id: str,
    project_id: str,
    snapshot_at_ms: int,
    project_payload: dict[str, Any],
    tasks: list[dict[str, Any]],
    libraries: dict[str, dict[str, Any]],
) -> str:
    brief = project_payload["brief"]
    artifact = project_payload["artifact"]
    lines = [
        f"# Project: {project_id}",
        "",
        f"- Tenant: `{tenant_id}`",
        f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`",
        f"- Source digest artifact: `{artifact.get('artifact_id', 'unknown')}`",
        f"- Source updated at: `{iso_timestamp(artifact.get('source_updated_at_ms'))}`",
        "",
        "## Overview",
        "",
        brief.get("overview", "No overview available."),
        "",
    ]
    lines.extend(render_trust_block(project_payload, "../tasks/"))
    lines.extend(
        [
            "## Libraries",
            "",
            f"- [Failures](../libraries/failures.md) ({len(libraries['failures'].get('results', []))})",
            f"- [Decisions](../libraries/decisions.md) ({len(libraries['decisions'].get('results', []))})",
            f"- [Evidence](../libraries/evidence.md) ({len(libraries['evidence'].get('results', []))})",
            f"- [Highlights](../libraries/highlights.md) ({len(libraries['highlights'].get('results', []))})",
            "",
            "## Task Pages",
            "",
        ]
    )
    for task in tasks:
        title = task_title(task["resume"])
        summary = one_line(task["resume"].get("latest_summary"))
        lines.append(f"- [../tasks/{task['task_id']}.md](../tasks/{task['task_id']}.md) - {title}")
        if summary:
            lines.append(f"  Latest: {summary}")

    lines.extend(
        [
            "",
            "## Recent Failures",
            "",
        ]
    )
    for item in libraries["failures"].get("results", [])[:10]:
        lines.append(
            f"- [../tasks/{item['task_id']}.md](../tasks/{item['task_id']}.md) - {one_line(item['summary'])}"
        )

    lines.extend(
        [
            "",
            "## Recent Decisions",
            "",
        ]
    )
    for item in libraries["decisions"].get("results", [])[:10]:
        lines.append(
            f"- [../tasks/{item['task_id']}.md](../tasks/{item['task_id']}.md) - {one_line(item['summary'])}"
        )

    lines.extend(
        [
            "",
            "## Evidence Highlights",
            "",
        ]
    )
    for item in libraries["evidence"].get("results", [])[:10]:
        lines.append(
            f"- [../tasks/{item['task_id']}.md](../tasks/{item['task_id']}.md) - {one_line(item['summary'])}"
        )

    lines.extend(
        [
            "",
            "## Highlight Library",
            "",
        ]
    )
    for item in libraries["highlights"].get("results", [])[:10]:
        lines.append(
            f"- [../tasks/{item['task_id']}.md](../tasks/{item['task_id']}.md) - {one_line(item['summary'])}"
        )

    related_projects = brief.get("related_projects") or []
    if related_projects:
        lines.extend(["", "## Related Projects", ""])
        for other_project in related_projects:
            lines.append(f"- `{other_project}`")

    lines.append("")
    return "\n".join(lines)


def render_task_page(
    tenant_id: str,
    project_id: str,
    snapshot_at_ms: int,
    task_payload: dict[str, Any],
) -> str:
    resume = task_payload["resume"]
    resume_payload = task_payload["resume_payload"]
    thread = task_payload["thread"]
    task = resume["task"]
    artifacts = thread.get("artifacts", [])
    title = task_title(resume)

    lines = [
        f"# Task: {title}",
        "",
        f"- Task ID: `{task.get('task_id')}`",
        f"- Tenant: `{tenant_id}`",
        f"- Project: [`{project_id}`](../projects/{project_id}.md)",
        f"- Status: `{task.get('status', 'unknown')}`",
        f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`",
        f"- Source digest artifact: `{task_payload['resume_artifact'].get('artifact_id', 'unknown')}`",
        f"- Started at: `{iso_timestamp(task.get('started_at_ms'))}`",
        f"- Updated at: `{iso_timestamp(task_updated_at(resume))}`",
        "",
    ]
    lines.extend(render_trust_block(resume_payload, "../tasks/"))

    latest_summary = one_line(resume.get("latest_summary"))
    if latest_summary:
        lines.extend(["## Latest Summary", "", latest_summary, ""])

    scientific_question = task.get("scientific_question")
    if scientific_question:
        lines.extend(["## Scientific Question", "", scientific_question, ""])

    for heading, key in (
        ("What Worked", "what_worked"),
        ("What Failed", "what_failed"),
        ("Validation", "validation"),
        ("Followups", "followups"),
    ):
        values = resume.get(key) or []
        if not values:
            continue
        lines.extend([f"## {heading}", ""])
        for value in values:
            lines.append(f"- {one_line(value)}")
        lines.append("")

    lines.extend(["## Thread Events", ""])
    for artifact in artifacts:
        if artifact.get("artifact_kind") == "digest":
            continue
        trust_bits: list[str] = []
        if artifact.get("verification_status"):
            trust_bits.append(f"verification={artifact['verification_status']}")
        if artifact.get("promotion_state"):
            trust_bits.append(f"promotion={artifact['promotion_state']}")
        suffix = f" | {'; '.join(trust_bits)}" if trust_bits else ""
        lines.append(
            f"- `{iso_timestamp(artifact.get('timestamp_created'))}` `{artifact_heading(artifact)}` `{artifact.get('artifact_id')}`{suffix}"
        )
        summary = artifact_summary(artifact)
        if summary:
            lines.append(f"  {summary}")

    lines.append("")
    return "\n".join(lines)


def render_library_page(
    library_name: str,
    project_id: str,
    snapshot_at_ms: int,
    payload: dict[str, Any],
) -> str:
    artifact = payload["artifact"]
    results = payload.get("results", [])
    lines = [
        f"# {library_name.title()} Library",
        "",
        f"- Project: [`{project_id}`](../projects/{project_id}.md)",
        f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`",
        f"- Source digest artifact: `{artifact.get('artifact_id', 'unknown')}`",
        f"- Source updated at: `{iso_timestamp(artifact.get('source_updated_at_ms'))}`",
        "",
    ]
    lines.extend(render_trust_block(payload, "../tasks/"))

    summary = artifact.get("summary")
    if summary:
        lines.extend(["## Summary", "", one_line(summary), ""])

    lines.extend(["## Items", ""])
    for item in results:
        task_id = item.get("task_id", "unknown-task")
        lines.append(
            f"- [../tasks/{task_id}.md](../tasks/{task_id}.md) - {one_line(item.get('summary'))}"
        )
        extras: list[str] = []
        if "confidence" in item:
            extras.append(f"confidence={item['confidence']}")
        if "category" in item:
            extras.append(f"category={item['category']}")
        if "explicit" in item:
            extras.append(f"explicit={item['explicit']}")
        if "supports_claim" in item:
            extras.append(f"supports_claim={item['supports_claim']}")
        if "support_count" in item:
            extras.append(f"support_count={item['support_count']}")
        if extras:
            lines.append(f"  {'; '.join(extras)}")
        rationale = item.get("rationale")
        if rationale:
            lines.append(f"  rationale: {one_line(rationale)}")

    lines.append("")
    return "\n".join(lines)


def render_concept_page(
    tenant_id: str,
    project_id: str,
    snapshot_at_ms: int,
    record: dict[str, Any],
) -> str:
    """Render an LLM-authored wiki_page artifact (plan §5 phase 2).

    The rendered page has four sections:
    1. YAML frontmatter — artifact_id, role, trust_tier, snapshot.
    2. ``# {summary}`` heading + the summary as the page subtitle.
    3. The raw markdown ``content`` body (LLM-authored).
    4. ``## Grounded By`` footer — links back to every cited artifact.

    Concept pages link grounding refs back to ``../tasks/<id>.md``
    using the same convention as project / library pages so internal
    backlinks stay consistent across the wiki.
    """
    page = record["page"]
    summary = one_line(page.get("summary"))
    role = record["artifact_role"]
    artifact_id = record["artifact_id"]

    frontmatter = [
        "---",
        f"artifact_id: {artifact_id}",
        f"artifact_kind: wiki_page",
        f"artifact_role: {role}",
        f"trust_tier: {record['trust_tier']}",
        f"source_snapshot_at_ms: {snapshot_at_ms}",
        f"source_updated_at_ms: {record['source_updated_at_ms']}",
        f"tenant_id: {tenant_id}",
        f"project_id: {project_id}",
        "---",
        "",
    ]

    title_text = summary or f"Concept page {artifact_id}"
    body_lines = [
        f"# {title_text}",
        "",
    ]
    if summary and summary != title_text:
        body_lines.extend([summary, ""])
    body_lines.append(f"- Lane: `{record['lane']}`")
    body_lines.append(f"- Role: `{role}`")
    body_lines.append(f"- Trust tier: `{record['trust_tier']}`")
    body_lines.append(f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`")
    body_lines.append(
        f"- Source updated at: `{iso_timestamp(record['source_updated_at_ms'])}`"
    )
    body_lines.append("")

    content = page.get("content")
    if isinstance(content, str) and content.strip():
        body_lines.append("## Body")
        body_lines.append("")
        body_lines.append(content.rstrip())
        body_lines.append("")

    grounding_lines = render_concept_grounding(record["grounding_refs"])
    body_lines.extend(grounding_lines)

    return "\n".join(frontmatter + body_lines)


def render_concept_grounding(
    refs: list[dict[str, Any]],
) -> list[str]:
    """Render the Grounded By footer for a concept page.

    Each ref points back at the canonical task page via
    ``../tasks/<id>.md``; unresolved refs render with an
    `(unresolved)` marker so the Phase 3 lint can pin the data
    integrity issue, and the human reader is not silently misled.
    """
    if not refs:
        return ["## Grounded By", "", "- (no grounding refs)", ""]
    lines = ["## Grounded By", ""]
    for ref in refs:
        task_id = ref.get("task_id", "unknown-task")
        artifact_id = ref.get("artifact_id", "unknown-artifact")
        artifact_kind = ref.get("artifact_kind", "artifact")
        role = ref.get("artifact_role")
        suffix = f", role={role}" if role else ""
        marker = "" if ref.get("resolved", True) else " *(unresolved)*"
        lines.append(
            f"- [{task_id}](../tasks/{task_id}.md) | artifact `{artifact_id}` | {artifact_kind}{suffix}{marker}"
        )
    lines.append("")
    return lines


def render_log_page(
    tenant_id: str,
    project_id: str,
    snapshot_at_ms: int,
    entries: list[dict[str, Any]],
) -> str:
    lines = [
        f"# Project Log: {project_id}",
        "",
        f"- Tenant: `{tenant_id}`",
        f"- Project: [`{project_id}`](projects/{project_id}.md)",
        f"- Source snapshot at: `{iso_timestamp(snapshot_at_ms)}`",
        "",
    ]

    for entry in entries:
        lines.append(
            f"## [{iso_timestamp(entry['timestamp_created'])}] {entry['heading']} | [tasks/{entry['task_id']}.md](tasks/{entry['task_id']}.md)"
        )
        lines.append("")
        if entry["summary"]:
            lines.append(entry["summary"])
            lines.append("")
        lines.append(f"- Artifact ID: `{entry['artifact_id']}`")
        lines.append("")

    return "\n".join(lines)
