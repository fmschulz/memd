"""Server-version compatibility gate for memd-wiki ↔ memd.

Per the Item 7 plan §9.3: compare parsed MAJOR.MINOR between the running
``memd`` server (from its MCP ``initialize`` response ``serverInfo.version``)
and this ``memd-wiki`` build (``compiled_wiki.__version__``).

- MAJOR.MINOR mismatch: hard fail (severity ``"fail"``).
- PATCH-only mismatch: warn (severity ``"warn"``).
- Match: ok.
- Unparseable server version: warn (we don't know what we're talking to).

The gate function is pure; callers decide how to surface failures
(raise, log, exit). This keeps the test matrix small and independent of
transport.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from ._semver import Semver, SemverParseError, compare_major_minor, parse

Severity = Literal["ok", "warn", "fail"]


@dataclass(frozen=True)
class CompatResult:
    severity: Severity
    message: str
    client_version: str
    server_version: str


class ServerIncompatibleError(RuntimeError):
    """Raised when MAJOR.MINOR mismatch is detected.

    Carries both versions so the caller can surface a readable diagnostic.
    """

    def __init__(self, client_version: str, server_version: str) -> None:
        super().__init__(
            f"memd server version {server_version!r} is incompatible with "
            f"memd-wiki {client_version!r} (MAJOR.MINOR must match)"
        )
        self.client_version = client_version
        self.server_version = server_version


def check_server_compat(
    server_version: str | None,
    client_version: str,
) -> CompatResult:
    """Compare ``server_version`` to ``client_version``.

    Returns a ``CompatResult``; callers decide how to surface non-ok
    severities. Does not raise on parse failures — those become
    severity ``"warn"`` with a diagnostic message so we can still talk
    to older / experimental builds while logging the skew.
    """
    client_display = client_version
    server_display = server_version if server_version is not None else "<unknown>"

    try:
        client = parse(client_version)
    except SemverParseError as exc:
        # Programmer error (we control __version__), but stay soft: warn.
        return CompatResult(
            severity="warn",
            message=f"could not parse memd-wiki __version__ {client_version!r}: {exc}",
            client_version=client_display,
            server_version=server_display,
        )

    if server_version is None or not isinstance(server_version, str):
        return CompatResult(
            severity="warn",
            message=(
                f"memd server did not report serverInfo.version; "
                f"memd-wiki is {client}. Proceeding, but compat cannot be verified."
            ),
            client_version=client_display,
            server_version=server_display,
        )

    try:
        server = parse(server_version)
    except SemverParseError as exc:
        return CompatResult(
            severity="warn",
            message=(
                f"could not parse memd server version {server_version!r}: {exc}. "
                f"Proceeding, but compat cannot be verified."
            ),
            client_version=client_display,
            server_version=server_display,
        )

    return _compare(client, server)


def _compare(client: Semver, server: Semver) -> CompatResult:
    client_display = str(client)
    server_display = str(server)
    if compare_major_minor(client, server) != 0:
        return CompatResult(
            severity="fail",
            message=(
                f"memd-wiki {client_display} requires memd server "
                f"MAJOR.MINOR={client.major}.{client.minor}.x, "
                f"but server reports {server_display}"
            ),
            client_version=client_display,
            server_version=server_display,
        )
    if client.patch != server.patch:
        return CompatResult(
            severity="warn",
            message=(
                f"patch-level version skew: memd-wiki {client_display} "
                f"vs memd server {server_display}. Proceeding."
            ),
            client_version=client_display,
            server_version=server_display,
        )
    return CompatResult(
        severity="ok",
        message=f"memd-wiki {client_display} matches memd server {server_display}",
        client_version=client_display,
        server_version=server_display,
    )
