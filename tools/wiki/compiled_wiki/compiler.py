from __future__ import annotations

import json
import re
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .containment import (
    check_outdir_containment,
    normalize_absolute,
    reject_if_any_symlink_inside_outdir,
)
from .cli_client import MemdCliClient, MemdCliError
from .render import (
    artifact_heading,
    artifact_summary,
    render_concept_page,
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
    tenant_id: str
    project_id: str
    output_dir: Path
    memd_bin: str = "memd"
    data_dir: Path | None = None
    memd_url: str | None = None  # accepted for older configs; CLI builds use memd_bin
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
    skipped_task_count: int = 0
    skipped_task_ids: list[str] = field(default_factory=list)


def build_wiki(config: BuildConfig) -> BuildResult:
    outdir_abs = check_outdir_containment(
        config.output_dir,
        config.forbidden_data_dirs,
    )
    client = MemdCliClient(
        memd_bin=config.memd_bin,
        data_dir=config.data_dir,
        timeout=config.timeout,
    )

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

    # v2 phase 2: fetch every wiki_page artifact for this project so the
    # compiler can render LLM-authored concept / entity pages alongside
    # the deterministic compiler-owned surface. Empty result is
    # expected on installs that haven't authored any concept pages yet —
    # the compiler still emits an (empty) `concepts/` and `entities/`
    # lane in the manifest so v2 readers see the new lanes.
    wiki_pages = fetch_wiki_pages(client, config)

    primary_task_ids = list(
        project_payload["brief"].get("source_task_ids", [])
    )[: config.max_tasks]
    # Plan §5 / step 6 force-emit: union the top-max_tasks primary set
    # with every task_id referenced by emitted library or project
    # pages, so library links never dangle even when the referenced
    # task sits outside the top window. Deduplicated; primary order
    # preserved; referenced-only tasks appended in deterministic order.
    referenced_task_ids = collect_referenced_task_ids(project_payload, libraries)
    all_task_ids: list[str] = []
    seen: set[str] = set()
    for task_id in primary_task_ids + sorted(referenced_task_ids):
        if task_id and task_id not in seen:
            all_task_ids.append(task_id)
            seen.add(task_id)
    primary_set = set(primary_task_ids)

    tasks: list[dict[str, Any]] = []
    force_emit_tasks: list[dict[str, Any]] = []
    skipped_task_ids: list[str] = []
    for task_id in all_task_ids:
        try:
            resume_payload = client.call_tool(
                "task.resume",
                {"tenant_id": config.tenant_id, "task_id": task_id, "k": 8},
            )
            thread_payload = client.call_tool(
                "artifact.list_thread",
                {"tenant_id": config.tenant_id, "thread_id": task_id},
            )
        except MemdCliError as exc:
            if _is_missing_task_error(exc):
                skipped_task_ids.append(task_id)
                continue
            raise
        bundle = {
            "task_id": task_id,
            "resume_payload": resume_payload,
            "resume": resume_payload["resume"],
            "resume_artifact": resume_payload["artifact"],
            "thread": thread_payload,
        }
        if task_id in primary_set:
            tasks.append(bundle)
        else:
            force_emit_tasks.append(bundle)

    if skipped_task_ids:
        prune_missing_task_refs(project_payload, libraries, set(skipped_task_ids))
    ensure_library_grounding_refs(libraries)

    # Primary: most-recently-updated first. Secondary: task_id for
    # stable ordering when timestamps tie or when the backend
    # does not guarantee a specific order across runs.
    tasks.sort(
        key=lambda item: (
            -task_updated_at(item["resume"]),
            item["task_id"],
        ),
    )
    # Sort each thread's artifacts by (timestamp desc, artifact_id asc)
    # so render output does not depend on backend intra-thread order.
    for task_payload in tasks:
        thread = task_payload["thread"]
        artifacts = thread.get("artifacts", [])
        artifacts.sort(
            key=lambda artifact: (
                -int(artifact.get("timestamp_created") or 0),
                str(artifact.get("artifact_id") or ""),
            )
        )
        thread["artifacts"] = artifacts
    # Sort each library's results by (task_id, artifact_id) so render
    # output is stable across arbitrary backend permutations of the
    # same logical result set. The backend's own ranking is lost for
    # libraries that rely on it; plan §6 accepts this in exchange for
    # true state-level determinism.
    for library_payload in libraries.values():
        results = library_payload.get("results", [])
        results.sort(
            key=lambda item: (
                str(item.get("task_id") or ""),
                str(item.get("artifact_id") or ""),
            )
        )
        library_payload["results"] = results
    snapshot_at_ms = determine_snapshot_timestamp(project_payload, libraries, tasks)
    log_entries = build_log_entries(tasks)
    project_page_path = f"projects/{safe_project_page_stem(config.project_id)}.md"

    written_files = 0
    unchanged_files = 0

    config.output_dir.mkdir(parents=True, exist_ok=True)
    (config.output_dir / "projects").mkdir(parents=True, exist_ok=True)
    (config.output_dir / "tasks").mkdir(parents=True, exist_ok=True)
    (config.output_dir / "libraries").mkdir(parents=True, exist_ok=True)

    # v2 phase 2: stable sort + grounding-resolution for wiki_page
    # artifacts. The order is deterministic regardless of backend
    # ranking (plan §5 phase 2): primary key is the role lane
    # (concept first, then entity), then a human-readable sort key
    # (entity name when present, otherwise the page summary truncated
    # to 50 chars), then the artifact_id as a stable tie-breaker.
    sorted_wiki_pages = sort_wiki_pages(wiki_pages)
    wiki_page_records = [
        build_concept_page_record(client, config, page)
        for page in sorted_wiki_pages
    ]
    if wiki_page_records:
        (config.output_dir / "concepts").mkdir(parents=True, exist_ok=True)
        (config.output_dir / "entities").mkdir(parents=True, exist_ok=True)

    files = {
        config.output_dir / "index.md": render_index(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            tasks,
            project_page_path,
        ),
        config.output_dir / "log.md": render_log_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            log_entries,
            project_page_path,
        ),
        config.output_dir / project_page_path: render_project_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            project_payload,
            tasks,
            libraries,
        ),
        config.output_dir / "libraries" / "failures.md": render_library_page(
            "failures",
            config.project_id,
            snapshot_at_ms,
            libraries["failures"],
            project_page_path,
        ),
        config.output_dir / "libraries" / "decisions.md": render_library_page(
            "decisions",
            config.project_id,
            snapshot_at_ms,
            libraries["decisions"],
            project_page_path,
        ),
        config.output_dir / "libraries" / "evidence.md": render_library_page(
            "evidence",
            config.project_id,
            snapshot_at_ms,
            libraries["evidence"],
            project_page_path,
        ),
        config.output_dir / "libraries" / "highlights.md": render_library_page(
            "highlights",
            config.project_id,
            snapshot_at_ms,
            libraries["highlights"],
            project_page_path,
        ),
        config.output_dir / "manifest.json": render_manifest(
            config,
            snapshot_at_ms,
            tasks,
            log_entries,
            project_payload,
            wiki_page_records,
            skipped_task_ids=skipped_task_ids,
        ),
    }
    for record in wiki_page_records:
        files[config.output_dir / record["path"]] = render_concept_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            record,
        )

    # Sort force-emit tasks and their thread artifacts for determinism.
    force_emit_tasks.sort(key=lambda item: item["task_id"])
    for task_payload in force_emit_tasks:
        thread = task_payload["thread"]
        artifacts = thread.get("artifacts", [])
        artifacts.sort(
            key=lambda artifact: (
                -int(artifact.get("timestamp_created") or 0),
                str(artifact.get("artifact_id") or ""),
            )
        )
        thread["artifacts"] = artifacts
    for task in tasks + force_emit_tasks:
        files[config.output_dir / "tasks" / f"{task['task_id']}.md"] = render_task_page(
            config.tenant_id,
            config.project_id,
            snapshot_at_ms,
            task,
            project_page_path,
        )

    remove_stale_owned_output(config.output_dir, outdir_abs, set(files))
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
        skipped_task_count=len(skipped_task_ids),
        skipped_task_ids=sorted(skipped_task_ids),
    )


def _is_missing_task_error(exc: Exception) -> bool:
    return "task not found" in str(exc).lower()


def remove_stale_owned_output(
    output_dir: Path,
    outdir_abs: Path,
    keep_files: set[Path],
) -> None:
    """Remove stale compiler-managed files before writing a fresh snapshot.

    Rebuilds must be idempotent at the directory level. Without this,
    old generated pages can survive after a project_id-safe path change
    or after a task/library falls out of the current snapshot, causing
    manifest drift and stale navigation. Current files are left in
    place so an identical second build still reports zero writes.
    """
    keep = {normalize_absolute(path) for path in keep_files}
    for rel in COMPILER_OWNED_PREFIXES + LLM_AUTHORED_PREFIXES:
        target = output_dir / rel
        if not target.exists() and not target.is_symlink():
            continue
        reject_if_any_symlink_inside_outdir(target, outdir_abs)
        if target.is_file() or target.is_symlink():
            if normalize_absolute(target) in keep:
                continue
            target.unlink()
            continue
        for path in sorted(target.rglob("*"), reverse=True):
            reject_if_any_symlink_inside_outdir(path, outdir_abs)
            if path.is_file():
                if normalize_absolute(path) in keep:
                    continue
                path.unlink()
            elif path.is_dir():
                try:
                    path.rmdir()
                except OSError:
                    pass


def prune_missing_task_refs(
    project_payload: dict[str, Any],
    libraries: dict[str, dict[str, Any]],
    missing_task_ids: set[str],
) -> None:
    """Drop links to task records that the local memd store can no longer resume.

    Historical digest artifacts can outlive the canonical task rows they
    cite. The compiler's force-emit guarantee should avoid dangling links
    for resolvable tasks, but it must not let one stale task id fail an
    otherwise buildable project.
    """
    if not missing_task_ids:
        return

    brief = project_payload.get("brief")
    if isinstance(brief, dict):
        source_task_ids = brief.get("source_task_ids")
        if isinstance(source_task_ids, list):
            brief["source_task_ids"] = [
                task_id
                for task_id in source_task_ids
                if not (isinstance(task_id, str) and task_id in missing_task_ids)
            ]

    project_payload["grounding_refs"] = _drop_missing_refs(
        project_payload.get("grounding_refs") or [],
        missing_task_ids,
    )

    for library_payload in libraries.values():
        library_payload["results"] = [
            item
            for item in library_payload.get("results") or []
            if not _item_has_missing_task(item, missing_task_ids)
        ]
        library_payload["grounding_refs"] = _drop_missing_refs(
            library_payload.get("grounding_refs") or [],
            missing_task_ids,
        )


def _drop_missing_refs(
    refs: list[Any],
    missing_task_ids: set[str],
) -> list[Any]:
    return [
        ref
        for ref in refs
        if not (
            isinstance(ref, dict)
            and isinstance(ref.get("task_id"), str)
            and ref["task_id"] in missing_task_ids
        )
    ]


def _item_has_missing_task(item: Any, missing_task_ids: set[str]) -> bool:
    return (
        isinstance(item, dict)
        and isinstance(item.get("task_id"), str)
        and item["task_id"] in missing_task_ids
    )


def ensure_library_grounding_refs(libraries: dict[str, dict[str, Any]]) -> None:
    """Backfill digest-library grounding refs from concrete result rows.

    Some historical digest helpers return useful result items without
    populating the digest payload's top-level ``grounding_refs`` field.
    The rendered library page can still ground those items by linking
    each result's canonical task/artifact pair.
    """
    for payload in libraries.values():
        if payload.get("grounding_refs"):
            continue
        refs: list[dict[str, str]] = []
        seen: set[tuple[str, str]] = set()
        for item in payload.get("results") or []:
            if not isinstance(item, dict):
                continue
            task_id = item.get("task_id")
            artifact_id = item.get("artifact_id")
            if not isinstance(task_id, str) or not task_id:
                continue
            if not isinstance(artifact_id, str) or not artifact_id:
                continue
            key = (task_id, artifact_id)
            if key in seen:
                continue
            seen.add(key)
            refs.append(
                {
                    "task_id": task_id,
                    "artifact_id": artifact_id,
                    "artifact_kind": str(item.get("artifact_kind") or "artifact"),
                }
            )
        if refs:
            refs.sort(key=lambda ref: (ref["task_id"], ref["artifact_id"]))
            payload["grounding_refs"] = refs


_PROJECT_PAGE_STEM_RE = re.compile(r"[^A-Za-z0-9._-]+")


def safe_project_page_stem(project_id: str) -> str:
    stem = _PROJECT_PAGE_STEM_RE.sub("-", project_id).strip("-._")
    return stem or "project"


def collect_referenced_task_ids(
    project_payload: dict[str, Any],
    libraries: dict[str, dict[str, Any]],
) -> set[str]:
    """Return every task_id referenced by emitted library or project pages.

    Step 6 invariant: the compiler force-emits ``tasks/<id>.md`` for
    every task_id that a library or project page links to, not just
    the top ``max_tasks`` primary set. This prevents the "linked task
    outside the top window" dead-backlink class documented in plan
    §5 (dead-backlink primitive).

    Sources scanned:
      - ``project_payload.grounding_refs[].task_id``
      - ``libraries[*].results[].task_id``
      - ``libraries[*].grounding_refs[].task_id``
    """
    ids: set[str] = set()
    for ref in project_payload.get("grounding_refs") or []:
        _add_id(ids, ref.get("task_id") if isinstance(ref, dict) else None)
    for library_payload in libraries.values():
        for result in library_payload.get("results") or []:
            if isinstance(result, dict):
                _add_id(ids, result.get("task_id"))
        for ref in library_payload.get("grounding_refs") or []:
            if isinstance(ref, dict):
                _add_id(ids, ref.get("task_id"))
    return ids


def _add_id(acc: set[str], raw: Any) -> None:
    if isinstance(raw, str) and raw.strip():
        acc.add(raw)


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
    # Primary: most-recent first. Secondary: artifact_id for stable
    # ordering when timestamps tie.
    entries.sort(
        key=lambda entry: (
            -int(entry["timestamp_created"]),
            str(entry["artifact_id"]),
        )
    )
    return entries


MANIFEST_SCHEMA_VERSION = 2

# Plan §6.1: prefixes of `output/` that the memd-wiki compiler owns.
# v1 lints only within these; anything outside (e.g. a future
# human-authored `concepts/` tree) is the caller's concern and the
# compiler will not manage it. Pinned so v2 can add LLM-authored
# prefixes without changing the manifest format.
COMPILER_OWNED_PREFIXES: tuple[str, ...] = (
    "index.md",
    "log.md",
    "manifest.json",
    "projects/",
    "tasks/",
    "libraries/",
)

# Plan §4.5: the LLM-authoring lane. Concept and entity pages live
# here; the compiler renders them from `wiki_page` artifacts but does
# not manage their content (that comes from `artifact.create` calls
# made out-of-band). Phase 3 adds `concept-*` lint checks scoped to
# this prefix tuple.
LLM_AUTHORED_PREFIXES: tuple[str, ...] = (
    "concepts/",
    "entities/",
)

# Plan §4.5: human-authored lane. The compiler never writes here and
# the lint tolerates dangling references INTO this lane. v2 ships the
# manifest declaration so v3 can add `notes/` enforcement without a
# manifest version bump.
HUMAN_OWNED_PREFIXES: tuple[str, ...] = (
    "notes/",
)


def fetch_wiki_pages(
    client: MemdCliClient, config: BuildConfig
) -> list[dict[str, Any]]:
    """Return every `wiki_page` artifact for the configured project.

    Wraps `artifact.search` with `artifact_kind=wiki_page` and
    requests up to 100 hits in one call. v2
    treats concept pages as a small set per project; if it grows past
    100 we will need to paginate or change the surface.
    """
    payload = client.call_tool(
        "artifact.search",
        {
            "tenant_id": config.tenant_id,
            "k": 100,
            "filters": {
                "project_id": config.project_id,
                "artifact_kind": "wiki_page",
            },
        },
    )
    # `artifact.search` returns `{"results": [ArtifactSearchHit, ...]}`
    # per `ArtifactSearchResult` in the Rust operation handlers.
    # Each hit carries the resolved canonical artifact under `.artifact`.
    results = payload.get("results") or []
    return [
        hit.get("artifact")
        for hit in results
        if isinstance(hit, dict) and hit.get("artifact")
    ]


def sort_wiki_pages(pages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Return wiki pages in deterministic order (plan §5 phase 2).

    Sort key: ``(artifact_role, entity_name or summary[:50], artifact_id)``.
    artifact_role goes first so the concept lane renders before the
    entity lane in any consumer that walks the manifest. The
    middle key is the human-readable disambiguator. The artifact_id
    tail breaks ties when two pages share both role and name.
    """
    def key(page: dict[str, Any]) -> tuple[str, str, str]:
        role = page.get("artifact_role") or ""
        entity_refs = page.get("entity_refs") or []
        name = ""
        if entity_refs and isinstance(entity_refs[0], dict):
            name = entity_refs[0].get("name", "") or ""
        if not name:
            name = (page.get("summary") or "")[:50]
        return (role, name, page.get("artifact_id") or "")

    return sorted(pages, key=key)


def build_concept_page_record(
    client: MemdCliClient,
    config: BuildConfig,
    page: dict[str, Any],
) -> dict[str, Any]:
    """Materialize a renderable concept-page record from a wiki_page artifact.

    Resolves each `related_artifact_ids` entry into a full grounding
    artifact via `artifact.get` so the renderer can cite the
    artifact_kind, role, and trust tier of every grounded record. Any
    id that fails to resolve is preserved as a stub so the page still
    renders something — the Phase 3 `concept-missing-grounding` lint
    catches the underlying data integrity issue.
    """
    artifact_id = page.get("artifact_id")
    role = page.get("artifact_role") or "concept"
    lane = "concepts" if role == "concept" else "entities"
    path = f"{lane}/{artifact_id}.md"

    grounding_refs: list[dict[str, Any]] = []
    for related_id in page.get("related_artifact_ids") or []:
        if not isinstance(related_id, str) or not related_id.strip():
            continue
        try:
            ref_payload = client.call_tool(
                "artifact.get",
                {
                    "tenant_id": config.tenant_id,
                    "artifact_id": related_id,
                },
            )
        except Exception:  # noqa: BLE001 — preserve dangling refs as stubs
            grounding_refs.append({
                "artifact_id": related_id,
                "task_id": "unknown-task",
                "artifact_kind": "unknown",
                "trust_tier": "unknown",
                "resolved": False,
            })
            continue
        ref_artifact = ref_payload.get("artifact") if isinstance(ref_payload, dict) else None
        if not isinstance(ref_artifact, dict):
            grounding_refs.append({
                "artifact_id": related_id,
                "task_id": "unknown-task",
                "artifact_kind": "unknown",
                "trust_tier": "unknown",
                "resolved": False,
            })
            continue
        grounding_refs.append({
            "artifact_id": ref_artifact.get("artifact_id", related_id),
            "task_id": ref_artifact.get("task_id", "unknown-task"),
            "artifact_kind": ref_artifact.get("artifact_kind", "unknown"),
            "artifact_role": ref_artifact.get("artifact_role"),
            "trust_tier": _grounding_trust_tier(ref_artifact),
            "resolved": True,
        })

    verifications = _fetch_verification_children(client, config, artifact_id)

    return {
        "page": page,
        "path": path,
        "lane": lane,
        "artifact_id": artifact_id,
        "artifact_role": role,
        "trust_tier": _grounding_trust_tier(page),
        "source_updated_at_ms": int(
            page.get("source_updated_at_ms")
            or page.get("timestamp_created")
            or 0
        ),
        "grounding_refs": grounding_refs,
        "verifications": verifications,
    }


def _fetch_verification_children(
    client: MemdCliClient,
    config: BuildConfig,
    wiki_page_artifact_id: str | None,
) -> list[dict[str, Any]]:
    """Return distinct-writer Verification artifacts that target this page.

    Plan §4.2 trust model: a wiki_page itself stays at
    ``CanonicalRecord``; UI-facing "verified" state is derived from
    presence of children whose ``reply_to_artifact_id`` points at the
    page AND whose ``promotion_state`` is ``verified``. The renderer surfaces these as
    ``Verified by: <agent_id> on <date>`` lines in the page footer; the
    Phase 3 lint validates the contract.
    """
    if not wiki_page_artifact_id:
        return []
    try:
        payload = client.call_tool(
            "artifact.search",
            {
                "tenant_id": config.tenant_id,
                "k": 50,
                "filters": {
                    "project_id": config.project_id,
                    "artifact_kind": "verification",
                    "reply_to_artifact_id": wiki_page_artifact_id,
                },
            },
        )
    except Exception:  # noqa: BLE001 — verification fetch is best-effort
        return []
    verifications: list[dict[str, Any]] = []
    # Same `{"results": [...]}` shape as the wiki_page query above.
    for hit in payload.get("results") or []:
        if not isinstance(hit, dict):
            continue
        artifact = hit.get("artifact")
        if not isinstance(artifact, dict):
            continue
        if artifact.get("promotion_state") != "verified":
            continue
        verifications.append({
            "artifact_id": artifact.get("artifact_id", "unknown"),
            "agent_id": artifact.get("agent_id") or "unknown-agent",
            "timestamp_created": int(artifact.get("timestamp_created") or 0),
        })
    # Stable ordering: earliest verification first, then artifact_id.
    verifications.sort(
        key=lambda v: (v["timestamp_created"], v["artifact_id"])
    )
    return verifications


def _grounding_trust_tier(artifact: dict[str, Any]) -> str:
    """Mirror Rust's `derive_artifact_trust_tier` for a Python dict.

    Used for rendering only. The authoritative computation lives in
    the Rust task-memory model; this surface keeps Phase 2 free of an
    extra CLI call per artifact when all we need is the displayed tier.
    """
    promotion = artifact.get("promotion_state")
    if promotion == "verified":
        return "verified_record"
    if artifact.get("artifact_kind") == "digest":
        return "compiled_digest_hint"
    return "canonical_record"


def render_manifest(
    config: BuildConfig,
    snapshot_at_ms: int,
    tasks: list[dict[str, Any]],
    log_entries: list[dict[str, Any]],
    project_payload: dict[str, Any],
    wiki_page_records: list[dict[str, Any]] | None = None,
    skipped_task_ids: list[str] | None = None,
) -> str:
    wiki_page_records = wiki_page_records or []
    skipped_task_ids = sorted(skipped_task_ids or [])
    concept_pages = [
        {
            "artifact_id": record["artifact_id"],
            "path": record["path"],
            "trust_tier": record["trust_tier"],
            "artifact_role": record["artifact_role"],
            "grounding_refs": [
                {
                    "artifact_id": ref["artifact_id"],
                    "task_id": ref["task_id"],
                    "artifact_kind": ref["artifact_kind"],
                }
                for ref in record["grounding_refs"]
            ],
            "source_updated_at_ms": record["source_updated_at_ms"],
        }
        for record in wiki_page_records
    ]
    manifest = {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "compiler_owned_prefixes": list(COMPILER_OWNED_PREFIXES),
        "llm_authored_prefixes": list(LLM_AUTHORED_PREFIXES),
        "human_owned_prefixes": list(HUMAN_OWNED_PREFIXES),
        "source_snapshot_at_ms": snapshot_at_ms,
        "memd_bin": config.memd_bin,
        "tenant_id": config.tenant_id,
        "project_id": config.project_id,
        "project_page_path": f"projects/{safe_project_page_stem(config.project_id)}.md",
        "task_count": len(tasks),
        "log_entry_count": len(log_entries),
        "project_digest_artifact_id": project_payload["artifact"].get("artifact_id"),
        "project_trust_tier": project_payload.get("trust_tier"),
        "task_ids": [task["task_id"] for task in tasks],
        "skipped_task_ids": skipped_task_ids,
        "concept_pages": concept_pages,
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
