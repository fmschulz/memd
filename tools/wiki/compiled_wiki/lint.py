"""memd-wiki lint: 5 health checks over a compiled output tree.

Plan §5 check inventory:

| Check | Severity | Signal |
|---|---|---|
| Digest-backed library page has ``grounding_refs=[]`` | ERROR |
| Task page snapshot older than latest canonical source artifact for its thread | WARN |
| Emitted link target missing from output | ERROR |
| Page renders from ``compiled_digest_hint`` with ``requires_verification=true`` and no canonical sibling | WARN |
| ``manifest.json`` references a page not on disk, or vice versa (scoped to ``compiler_owned_prefixes``) | ERROR |

Exit codes:
    0 — clean
    1 — warnings only
    2 — errors

Output is one line per finding in a stable format so CI can diff.

The lint operates over the emitted markdown/manifest tree. It deliberately
does not re-query memd: a separate staleness check can be layered by
passing a lookup callback, but the default is filesystem-only so the
lint is fast and offline.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterable

_TASK_LINK_RE = re.compile(r"\(\.\./tasks/(?P<task_id>[^)\s]+)\.md\)")
_TOP_LEVEL_TASK_LINK_RE = re.compile(r"\(tasks/(?P<task_id>[^)\s]+)\.md\)")
_ISO_FMT = "%Y-%m-%d %H:%M:%SZ"


def _parse_iso_to_ms(iso: str) -> int | None:
    """Parse the compiler's iso_timestamp() output back to ms since epoch.

    Returns None on anything non-conforming (including the literal
    "unknown" sentinel the compiler emits for a zero/missing timestamp).
    Keep this tolerant: a failed parse must not turn into a false
    "stale" finding.
    """
    try:
        dt = datetime.strptime(iso, _ISO_FMT).replace(tzinfo=timezone.utc)
    except (TypeError, ValueError):
        return None
    return int(dt.timestamp() * 1000)


@dataclass(frozen=True)
class LintFinding:
    severity: str  # "error" or "warn"
    check: str  # short identifier
    message: str
    path: str | None = None

    def render(self) -> str:
        prefix = f"{self.severity.upper()} {self.check}"
        if self.path is not None:
            prefix = f"{prefix} {self.path}"
        return f"{prefix}: {self.message}"


@dataclass(frozen=True)
class LintReport:
    findings: tuple[LintFinding, ...]

    @property
    def errors(self) -> tuple[LintFinding, ...]:
        return tuple(f for f in self.findings if f.severity == "error")

    @property
    def warnings(self) -> tuple[LintFinding, ...]:
        return tuple(f for f in self.findings if f.severity == "warn")

    def exit_code(self) -> int:
        if self.errors:
            return 2
        if self.warnings:
            return 1
        return 0


def lint_output_dir(
    outdir: Path,
    *,
    lookup_latest_ms: Callable[[str], int | None] | None = None,
) -> LintReport:
    """Run the full 5-check lint over a compiled output tree.

    ``lookup_latest_ms`` is an optional memd-backed oracle: given a
    task_id, return the latest canonical updated timestamp in ms, or
    None when the oracle has no opinion. When omitted, the
    task-snapshot-stale check is skipped entirely (default, offline).

    Returns findings in a stable order so CI diffs are meaningful:
    sort key is (check_name, path, message).
    """
    findings: list[LintFinding] = []
    findings.extend(_check_library_grounding_refs(outdir))
    findings.extend(
        _check_task_snapshots_stale(outdir, lookup_latest_ms=lookup_latest_ms)
    )
    findings.extend(_check_dead_backlinks(outdir))
    findings.extend(_check_trust_tier_surfacing(outdir))
    findings.extend(_check_manifest_drift(outdir))
    # Stable output ordering.
    findings.sort(key=lambda f: (f.check, f.path or "", f.message))
    return LintReport(findings=tuple(findings))


# --- Check 1: library pages without grounding refs ------------------------


def _check_library_grounding_refs(outdir: Path) -> Iterable[LintFinding]:
    library_dir = outdir / "libraries"
    if not library_dir.is_dir():
        return
    for page in sorted(library_dir.glob("*.md")):
        text = _read(page)
        if "Trust tier: `compiled_digest_hint`" not in text:
            continue
        if "### Grounded By" not in text:
            yield LintFinding(
                severity="error",
                check="library-missing-grounding",
                path=str(page.relative_to(outdir)),
                message=(
                    "digest-backed library page has no grounding_refs; "
                    "reader cannot verify"
                ),
            )


# --- Check 2: task snapshot older than latest canonical source -----------


def _check_task_snapshots_stale(
    outdir: Path,
    *,
    lookup_latest_ms: Callable[[str], int | None] | None = None,
) -> Iterable[LintFinding]:
    if lookup_latest_ms is None:
        # No oracle for "latest canonical source artifact timestamp"
        # → nothing to check. The CLI can inject a memd-backed
        # callback later; v1 accepts the gap rather than ship a
        # wrong heuristic.
        return
    tasks_dir = outdir / "tasks"
    if not tasks_dir.is_dir():
        return
    snap_re = re.compile(r"Source snapshot at: `(?P<iso>[^`]+)`")
    updated_re = re.compile(r"Updated at: `(?P<iso>[^`]+)`")
    for page in sorted(tasks_dir.glob("*.md")):
        task_id = page.stem
        text = _read(page)
        updated_match = updated_re.search(text)
        snapshot_match = snap_re.search(text)
        if updated_match is None or snapshot_match is None:
            continue
        latest = lookup_latest_ms(task_id)
        # Callback returns None to mean "unknown, skip". <=0 is also
        # meaningless (no data) and must not flag.
        if latest is None or latest <= 0:
            continue
        page_updated = updated_match.group("iso")
        page_snap = snapshot_match.group("iso")
        snap_ms = _parse_iso_to_ms(page_snap)
        if snap_ms is None:
            # Unknown / unparseable snapshot → can't tell if stale.
            # Don't guess. Skip silently (matches file-only default).
            continue
        if latest > snap_ms:
            yield LintFinding(
                severity="warn",
                check="task-snapshot-stale",
                path=str(page.relative_to(outdir)),
                message=(
                    f"task {task_id}: page snapshot {page_snap} is "
                    f"older than latest canonical source "
                    f"(memd: {latest} ms); page claims updated={page_updated}"
                ),
            )


# --- Check 3: dead backlinks (belt-and-suspenders for force-emit) --------


def _check_dead_backlinks(outdir: Path) -> Iterable[LintFinding]:
    tasks_dir = outdir / "tasks"
    if not tasks_dir.is_dir():
        return
    emitted = {p.stem for p in tasks_dir.glob("*.md")}

    candidates: list[Path] = []
    library_dir = outdir / "libraries"
    if library_dir.is_dir():
        candidates.extend(sorted(library_dir.glob("*.md")))
    project_dir = outdir / "projects"
    if project_dir.is_dir():
        candidates.extend(sorted(project_dir.glob("*.md")))
    log_path = outdir / "log.md"
    if log_path.is_file():
        candidates.append(log_path)
    index_path = outdir / "index.md"
    if index_path.is_file():
        candidates.append(index_path)

    for page in candidates:
        text = _read(page)
        rel = str(page.relative_to(outdir))
        # `../tasks/<id>.md` — used by libraries and projects pages.
        # `tasks/<id>.md` — used by top-level index/log pages.
        found: set[str] = set()
        for m in _TASK_LINK_RE.finditer(text):
            found.add(m.group("task_id"))
        for m in _TOP_LEVEL_TASK_LINK_RE.finditer(text):
            found.add(m.group("task_id"))
        for task_id in sorted(found):
            if task_id not in emitted:
                yield LintFinding(
                    severity="error",
                    check="dead-backlink",
                    path=rel,
                    message=(
                        f"references tasks/{task_id}.md which was not emitted"
                    ),
                )


# --- Check 4: trust-tier surfacing ---------------------------------------


def _check_trust_tier_surfacing(outdir: Path) -> Iterable[LintFinding]:
    tasks_dir = outdir / "tasks"
    if not tasks_dir.is_dir():
        return
    for page in sorted(tasks_dir.glob("*.md")):
        text = _read(page)
        if "Trust tier: `compiled_digest_hint`" not in text:
            continue
        if "Requires verification: `True`" not in text:
            continue
        # "Grounded By" links to a canonical sibling — absent means
        # the reader has no canonical task to click through to.
        if "### Grounded By" not in text:
            yield LintFinding(
                severity="warn",
                check="trust-tier-ungrounded",
                path=str(page.relative_to(outdir)),
                message=(
                    "page is compiled_digest_hint with requires_verification=True "
                    "but has no canonical grounding reference"
                ),
            )


# --- Check 5: manifest drift (scoped to compiler_owned_prefixes) ---------


def _check_manifest_drift(outdir: Path) -> Iterable[LintFinding]:
    manifest_path = outdir / "manifest.json"
    if not manifest_path.is_file():
        yield LintFinding(
            severity="error",
            check="manifest-missing",
            path="manifest.json",
            message="manifest.json is missing",
        )
        return
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        yield LintFinding(
            severity="error",
            check="manifest-invalid",
            path="manifest.json",
            message=f"could not parse manifest.json: {exc}",
        )
        return
    if not isinstance(manifest, dict):
        yield LintFinding(
            severity="error",
            check="manifest-invalid",
            path="manifest.json",
            message="manifest.json top level is not an object",
        )
        return
    raw_prefixes = manifest.get("compiler_owned_prefixes")
    if not isinstance(raw_prefixes, list) or not all(
        isinstance(p, str) for p in raw_prefixes
    ):
        yield LintFinding(
            severity="error",
            check="manifest-invalid",
            path="manifest.json",
            message=(
                "manifest.compiler_owned_prefixes must be a list of strings"
            ),
        )
        return
    owned_prefixes = tuple(raw_prefixes)

    declared_task_ids = manifest.get("task_ids") or []
    if not isinstance(declared_task_ids, list):
        declared_task_ids = []

    # Owned files actually on disk (relative strings).
    on_disk: set[str] = set()
    for path in outdir.rglob("*"):
        if not path.is_file():
            continue
        rel = str(path.relative_to(outdir)).replace("\\", "/")
        if _within_owned_prefixes(rel, owned_prefixes):
            on_disk.add(rel)

    # Files the manifest implicitly declares: manifest.json itself,
    # index.md, log.md, project page, library pages, and a
    # task page per task_id.
    implied: set[str] = {
        "manifest.json",
        "index.md",
        "log.md",
    }
    project_id = manifest.get("project_id")
    if isinstance(project_id, str):
        implied.add(f"projects/{project_id}.md")
    for library in ("failures", "decisions", "evidence", "highlights"):
        implied.add(f"libraries/{library}.md")
    for task_id in declared_task_ids:
        if isinstance(task_id, str) and task_id:
            implied.add(f"tasks/{task_id}.md")

    # On disk but no matching manifest implication.
    extra = sorted(on_disk - implied)
    for rel in extra:
        # Task pages for force-emit referenced tasks are NOT listed in
        # manifest.task_ids (which mirrors the primary window). Accept
        # those as owned without flagging — the force-emit invariant
        # from step 6 guarantees the page was intentional.
        if rel.startswith("tasks/") and rel.endswith(".md"):
            continue
        yield LintFinding(
            severity="error",
            check="manifest-drift",
            path=rel,
            message="file is under a compiler-owned prefix but not in manifest",
        )

    # Manifest-implied but missing from disk.
    missing = sorted(implied - on_disk)
    for rel in missing:
        yield LintFinding(
            severity="error",
            check="manifest-drift",
            path=rel,
            message="manifest references this path but file is missing on disk",
        )


def _within_owned_prefixes(rel: str, prefixes: tuple[str, ...]) -> bool:
    for prefix in prefixes:
        if prefix.endswith("/"):
            if rel.startswith(prefix):
                return True
        elif rel == prefix:
            return True
    return False


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return ""
