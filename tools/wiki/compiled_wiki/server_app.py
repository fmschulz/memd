"""Request handler and pure route resolver for ``memd-wiki serve``.

Phase summary:

- P0 served the compiled tree's ``index.md`` as ``text/plain``.
- **P1 (this phase)** renders ``index.md`` to ``text/html`` using
  the hand-rolled markdown renderer in ``html_render.py`` and wraps
  it in a minimal self-contained document with an inline ``<style>``
  block. Route table still resolves only ``/``; expanded routes land
  in P2.

Later phases layer on route expansion + containment (P2) and link
rewriting (P3).

The route resolver is a pure function so the routing table can be
exercised without binding a port.
"""

from __future__ import annotations

from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler
from pathlib import Path
from typing import Optional

from .html_render import render_page

HTML_CONTENT_TYPE = "text/html; charset=utf-8"
PLAIN_CONTENT_TYPE = "text/plain; charset=utf-8"


@dataclass(frozen=True)
class RouteResolution:
    """Outcome of resolving a request path against the compiled tree."""

    status: HTTPStatus
    content_type: str
    file_path: Optional[Path] = None


def resolve_route(outdir: Path, url_path: str) -> RouteResolution:
    """Map a URL path to a file under ``outdir`` or a 404.

    P1 still recognizes only the root path, which serves the tree's
    ``index.md`` rendered as HTML. Every other path is 404. Expanded
    routing (concept/entity/task/project/library pages, ``/log``,
    ``/manifest.json``) lands in P2.
    """
    if url_path in ("", "/"):
        index = outdir / "index.md"
        if index.is_file():
            return RouteResolution(
                status=HTTPStatus.OK,
                content_type=HTML_CONTENT_TYPE,
                file_path=index,
            )
    return RouteResolution(
        status=HTTPStatus.NOT_FOUND,
        content_type=PLAIN_CONTENT_TYPE,
    )


def make_handler(outdir: Path, *, quiet: bool = False) -> type:
    """Build a ``BaseHTTPRequestHandler`` subclass bound to ``outdir``.

    Returning a class (rather than an instance) matches the
    ``http.server`` contract: ``ThreadingHTTPServer`` instantiates one
    handler per request.
    """

    class WikiRequestHandler(BaseHTTPRequestHandler):
        server_version = "memd-wiki-serve/0.11.0"

        def do_GET(self) -> None:  # noqa: N802 — http.server API.
            route = resolve_route(outdir, self.path.split("?", 1)[0])
            if route.status is HTTPStatus.OK and route.file_path is not None:
                self._respond_file(route.file_path, route.content_type)
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

        def _respond_file(self, path: Path, content_type: str) -> None:
            if content_type.startswith("text/html"):
                markdown = path.read_text(encoding="utf-8")
                body = render_page(markdown, title=path.name).encode("utf-8")
            else:
                body = path.read_bytes()
            self._respond_bytes(HTTPStatus.OK, body, content_type)

    return WikiRequestHandler
