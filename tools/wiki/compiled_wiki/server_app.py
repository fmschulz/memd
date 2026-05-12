"""Request handler and pure route resolver for ``memd-wiki serve``.

Phase summary:

- P0 served the compiled tree's ``index.md`` as ``text/plain``.
- P1 switched ``index.md`` to ``text/html`` via ``html_render``.
- P2 formalized the route table with containment + symlink
  rejection.
- P3 plugged the per-page link rewriter into every rendered
  response so internal ``.md`` links resolve to the routes
  they were filed under.
- **Multi-project (this change)** discovers top-level project
  subdirectories at serve startup. Every URL whose first segment
  matches a discovered project slug is routed into that
  subdirectory as its own wiki mount; the emitted links are
  prefixed with the project slug so navigation stays self-
  consistent. URLs without a project prefix serve the top-level
  landing (``index.md``) and may reach into the standard lanes at
  the root, preserving the single-project behavior for callers
  who compile only one project.

Later phases layer on release.

The route resolver is a pure function so the routing table can be
exercised without binding a port.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler
from pathlib import Path
from typing import FrozenSet, Optional, Tuple

from .containment import (
    OutdirContainmentError,
    normalize_absolute,
    reject_if_any_symlink_inside_outdir,
)
from .html_render import make_link_rewriter, render_page

HTML_CONTENT_TYPE = "text/html; charset=utf-8"
PLAIN_CONTENT_TYPE = "text/plain; charset=utf-8"
JSON_CONTENT_TYPE = "application/json; charset=utf-8"

# URL path segments must match this character class. ``.`` and ``..``
# are also rejected explicitly in ``_is_valid_segment`` so percent-encoded
# or single-character traversal attempts cannot smuggle through.
_SEGMENT_RE = re.compile(r"^[A-Za-z0-9._-]+$")

# Top-level URL prefixes that name a lane under the compiled wiki.
# ``notes/`` is the human-owned lane (``human_owned_prefixes`` in the
# manifest); the other five are compiler- or LLM-authored.
_ROUTED_PREFIXES = frozenset(
    {"concepts", "entities", "tasks", "projects", "libraries", "notes"}
)


@dataclass(frozen=True)
class RouteResolution:
    """Outcome of resolving a request path against the compiled tree."""

    status: HTTPStatus
    content_type: str
    file_path: Optional[Path] = None


def _is_valid_segment(segment: str) -> bool:
    if segment in ("", ".", ".."):
        return False
    return bool(_SEGMENT_RE.match(segment))


def _url_to_relative(url_path: str) -> Optional[Tuple[Path, str]]:
    """Translate a request URL path to a ``(relative_path, content_type)`` pair.

    Returns ``None`` for anything outside the route whitelist. Does NOT
    touch the filesystem — the caller runs the containment guard and
    the ``is_file`` check. Character-level validation runs here so
    percent-encoded or traversal-shaped inputs never reach the FS.
    """
    trimmed = url_path.strip("/")
    if trimmed == "":
        return Path("index.md"), HTML_CONTENT_TYPE
    if trimmed == "log":
        return Path("log.md"), HTML_CONTENT_TYPE
    if trimmed == "manifest.json":
        return Path("manifest.json"), JSON_CONTENT_TYPE

    segments = trimmed.split("/")
    if not all(_is_valid_segment(s) for s in segments):
        return None

    top = segments[0]
    if top not in _ROUTED_PREFIXES:
        return None

    # Every routed prefix resolves to ``<outdir>/<...>/<leaf>.md``. The
    # ``.md`` suffix is appended once at the very end so the leaf name
    # passed in the URL stays filesystem-extension-free (matching the
    # trailing-slash URL convention at :file:`docs/plans`).
    rel_parts = list(segments[:-1]) + [segments[-1] + ".md"]
    return Path(*rel_parts), HTML_CONTENT_TYPE


def discover_project_slugs(outdir: Path) -> FrozenSet[str]:
    """Return top-level subdirectories of ``outdir`` that look like mounted wikis.

    A directory qualifies as a project mount if it contains either
    ``index.md`` or ``manifest.json`` — i.e., looks like a compiled
    wiki root. The top-level ``_ROUTED_PREFIXES`` (``concepts``,
    ``entities``, etc.) are never treated as project mounts even if
    they happen to contain an ``index.md`` the compiler emitted, so
    single-project layouts continue to resolve through the standard
    table.
    """
    if not outdir.is_dir():
        return frozenset()
    found = []
    for entry in outdir.iterdir():
        if not entry.is_dir() or entry.is_symlink():
            continue
        if entry.name in _ROUTED_PREFIXES:
            continue
        if not _is_valid_segment(entry.name):
            continue
        if (entry / "index.md").is_file() or (entry / "manifest.json").is_file():
            found.append(entry.name)
    return frozenset(found)


def resolve_route(
    outdir: Path,
    url_path: str,
    *,
    project_slugs: FrozenSet[str] = frozenset(),
) -> RouteResolution:
    """Map a URL path to a resolved file under ``outdir`` or a 404.

    The resolver is the single source of truth for the serve route
    table. Layered defenses before hitting disk:

    1. Per-segment character allowlist (``_is_valid_segment``).
    2. Top-level prefix whitelist (``_ROUTED_PREFIXES``) or a
       discovered project-slug set passed in ``project_slugs``.
    3. ``reject_if_any_symlink_inside_outdir`` — raises if the
       resolved target is outside ``outdir`` (textual containment)
       or if any component below ``outdir`` is a pre-existing
       symlink (fail-closed parity with the Rust export-markdown
       reference at ``crates/memd/src/cli.rs``).
    4. ``Path.is_file()`` — a valid route still 404s when the page
       hasn't been compiled yet.

    When the first URL segment is a discovered project slug, the
    resolver recurses into ``outdir/<slug>`` with the remainder of
    the path. This lets a single serve instance host many compiled
    project trees side-by-side.

    Every rejection funnels to a 404 ``text/plain`` response so the
    handler does not leak why the request was refused.
    """
    trimmed = url_path.strip("/")
    if trimmed:
        first_seg = trimmed.split("/", 1)[0]
        if first_seg in project_slugs and _is_valid_segment(first_seg):
            remainder = trimmed[len(first_seg):].lstrip("/")
            return resolve_route(outdir / first_seg, "/" + remainder)

    mapping = _url_to_relative(url_path)
    if mapping is None:
        return RouteResolution(
            status=HTTPStatus.NOT_FOUND, content_type=PLAIN_CONTENT_TYPE
        )
    relative, content_type = mapping

    outdir_abs = normalize_absolute(outdir)
    target = outdir_abs / relative
    try:
        reject_if_any_symlink_inside_outdir(target, outdir_abs)
    except OutdirContainmentError:
        return RouteResolution(
            status=HTTPStatus.NOT_FOUND, content_type=PLAIN_CONTENT_TYPE
        )

    if not target.is_file():
        return RouteResolution(
            status=HTTPStatus.NOT_FOUND, content_type=PLAIN_CONTENT_TYPE
        )
    return RouteResolution(
        status=HTTPStatus.OK, content_type=content_type, file_path=target
    )


def make_handler(outdir: Path, *, quiet: bool = False) -> type:
    """Build a ``BaseHTTPRequestHandler`` subclass bound to ``outdir``.

    Returning a class (rather than an instance) matches the
    ``http.server`` contract: ``ThreadingHTTPServer`` instantiates one
    handler per request.

    Project slugs are discovered once at factory time. Adding or
    removing a project subdirectory on disk requires a service
    restart to take effect.
    """
    outdir_abs = normalize_absolute(outdir)
    project_slugs = discover_project_slugs(outdir_abs)

    class WikiRequestHandler(BaseHTTPRequestHandler):
        server_version = "memd-wiki-serve/0.30.0"

        def do_GET(self) -> None:  # noqa: N802 — http.server API.
            url_path = self.path.split("?", 1)[0]
            route = resolve_route(outdir, url_path, project_slugs=project_slugs)
            if route.status is HTTPStatus.OK and route.file_path is not None:
                self._respond_file(route.file_path, route.content_type, url_path)
                return
            self._respond_bytes(
                route.status, b"not found\n", PLAIN_CONTENT_TYPE
            )

        def log_message(self, format: str, *args: object) -> None:  # noqa: A002, N802
            if quiet:
                return
            super().log_message(format, *args)

        def _respond_bytes(
            self, status: HTTPStatus, body: bytes, content_type: str
        ) -> None:
            self.send_response(status.value)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _respond_file(
            self, path: Path, content_type: str, url_path: str
        ) -> None:
            if content_type.startswith("text/html"):
                markdown = path.read_text(encoding="utf-8")
                try:
                    relative_to_outdir = path.relative_to(outdir_abs)
                except ValueError:
                    # Defense-in-depth: resolve_route already enforces
                    # containment, but fall back to an identity rewriter
                    # instead of crashing if invariants ever drift.
                    relative_to_outdir = Path(path.name)
                base_path, relative_to_mount = _split_project_prefix(
                    relative_to_outdir, project_slugs
                )
                rewriter = make_link_rewriter(relative_to_mount, base_path=base_path)
                body = render_page(
                    markdown, title=path.name, link_rewriter=rewriter
                ).encode("utf-8")
            else:
                body = path.read_bytes()
            self._respond_bytes(HTTPStatus.OK, body, content_type)

    return WikiRequestHandler


def _split_project_prefix(
    relative_to_outdir: Path, project_slugs: FrozenSet[str]
) -> Tuple[str, Path]:
    """Split a relative-to-outdir path into ``(base_path, relative_to_mount)``.

    When the first path component is a known project slug, the slug
    becomes the mount's base path (``/treeviz``) and the remaining
    components form the page's path within the mount. Otherwise the
    page is served at the root mount (empty base path).
    """
    parts = relative_to_outdir.parts
    if parts and parts[0] in project_slugs:
        base_path = "/" + parts[0]
        remainder = Path(*parts[1:]) if len(parts) > 1 else Path("index.md")
        return base_path, remainder
    return "", relative_to_outdir
