"""CLI entry for ``memd-wiki serve`` — read-only HTTP over a compiled tree.

P0 ships a minimum viable stdlib ``ThreadingHTTPServer`` that serves
``index.md`` of the compiled tree. Later phases add HTML rendering,
expanded routes with a containment guard, and link rewriting.

Zero third-party dependencies. ``ThreadingHTTPServer`` handles concurrent
reads fine for a local dev wiki.
"""

from __future__ import annotations

import argparse
import signal
import sys
from dataclasses import dataclass
from http.server import ThreadingHTTPServer
from pathlib import Path
from typing import Callable, Optional

from .config_loader import DiscoveredConfig
from .server_app import make_handler


DEFAULT_SERVE_HOST = "127.0.0.1"
DEFAULT_SERVE_PORT = 8099


@dataclass(frozen=True)
class ServeConfig:
    host: str
    port: int
    output_dir: Path


def resolve_serve_config(
    args: argparse.Namespace,
    discovered: DiscoveredConfig,
    *,
    resolve_output_dir: Callable[[object, DiscoveredConfig], Path],
) -> ServeConfig:
    """Derive a ``ServeConfig`` from CLI args + discovered config.

    ``resolve_output_dir`` is injected from ``cli.py`` so this module
    does not depend on CLI private helpers (keeps the dependency arrow
    ``cli -> serve`` rather than ``serve -> cli``).
    """
    output_dir = resolve_output_dir(getattr(args, "output_dir", None), discovered)
    host = getattr(args, "host", None) or DEFAULT_SERVE_HOST
    port = getattr(args, "port", None)
    port = DEFAULT_SERVE_PORT if port is None else int(port)
    return ServeConfig(host=host, port=port, output_dir=output_dir)


def run_serve(config: ServeConfig, *, quiet: bool = False) -> int:
    """Bind and serve until SIGINT/SIGTERM. Returns the process exit code."""
    if not config.output_dir.is_dir():
        print(
            f"memd-wiki: error: serve target {config.output_dir} is not a directory",
            file=sys.stderr,
        )
        return 2

    handler_cls = make_handler(config.output_dir, quiet=quiet)
    server = ThreadingHTTPServer((config.host, config.port), handler_cls)
    bound_host, bound_port = server.server_address[:2]
    print(
        f"memd-wiki: serving {config.output_dir} on http://{bound_host}:{bound_port}/",
        file=sys.stderr,
    )

    def _graceful_shutdown(_signum: int, _frame: object) -> None:
        # ``shutdown`` must be called from a different thread than
        # ``serve_forever`` per the stdlib contract, but signal handlers
        # run on the main thread while ``serve_forever`` is also on the
        # main thread here. We use a sentinel exception instead.
        raise KeyboardInterrupt

    previous_int = signal.signal(signal.SIGINT, _graceful_shutdown)
    previous_term = signal.signal(signal.SIGTERM, _graceful_shutdown)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        signal.signal(signal.SIGINT, previous_int)
        signal.signal(signal.SIGTERM, previous_term)
    return 0


def _run_serve(args: argparse.Namespace, discovered: DiscoveredConfig) -> int:
    """CLI entry point invoked from ``cli.main``."""
    from .cli import _resolve_output_dir

    config = resolve_serve_config(
        args, discovered, resolve_output_dir=_resolve_output_dir
    )
    return run_serve(config, quiet=False)


def _add_serve_subparser(subparsers: argparse._SubParsersAction) -> None:
    serve = subparsers.add_parser(
        "serve",
        help="Serve a compiled wiki over read-only HTTP (localhost).",
        description=(
            "Serve a compiled wiki tree over read-only HTTP. "
            "Zero-dependency stdlib server; binds localhost by default. "
            "P0 exposes the tree's index.md as text/plain; HTML rendering "
            "and expanded routing land in later phases. "
            "Note: serve does not rebuild the tree — run `memd-wiki build` "
            "first when state changes."
        ),
    )
    from .cli import _add_shared_config_args

    _add_shared_config_args(serve)
    serve.add_argument(
        "--host",
        default=None,
        help=(
            f"Address to bind. Default: {DEFAULT_SERVE_HOST!r}. "
            "Use 0 for any free port (useful for tests and CI)."
        ),
    )
    serve.add_argument(
        "--port",
        type=int,
        default=None,
        help=(
            f"TCP port to bind. Default: {DEFAULT_SERVE_PORT}. Pass 0 "
            "for an ephemeral OS-assigned port."
        ),
    )
