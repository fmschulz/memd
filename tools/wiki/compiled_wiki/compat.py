"""Executable-version compatibility gate for memd-wiki ↔ memd.

Compare parsed MAJOR.MINOR between the local ``memd`` executable and this
``memd-wiki`` build (``compiled_wiki.__version__``).

- MAJOR.MINOR mismatch: hard fail (severity ``"fail"``).
- PATCH-only mismatch: warn (severity ``"warn"``).
- Match: ok.
- Unparseable executable version: warn (we don't know what we're calling).

The gate function is pure; callers decide how to surface failures
(raise, log, exit). This keeps the test matrix small and independent of
process execution details.

v2 phase 4 also adds ``check_manifest_version`` for the manifest
schema-version compat gate documented in plan §4.4 — a memd-wiki that
encounters a manifest from a *future* schema version must fail clearly
rather than silently misparse missing fields.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from ._semver import Semver, SemverParseError, compare_major_minor, parse

Severity = Literal["ok", "warn", "fail"]

# Plan §4.4: the largest manifest ``schema_version`` this build of
# memd-wiki knows how to read. Bumped lockstep with
# ``compiled_wiki.compiler.MANIFEST_SCHEMA_VERSION`` whenever a new
# manifest revision lands. A reader that sees schema_version > this
# value raises ``WikiManifestTooNewError`` so the operator gets a
# clear "upgrade memd-wiki" diagnostic instead of a silent
# JSON-parse fallback.
MAX_KNOWN_MANIFEST_SCHEMA_VERSION = 2


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
            f"memd executable version {server_version!r} is incompatible with "
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
                f"memd executable did not report a version; "
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
                f"could not parse memd executable version {server_version!r}: {exc}. "
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
                f"memd-wiki {client_display} requires memd executable "
                f"MAJOR.MINOR={client.major}.{client.minor}.x, "
                f"but executable reports {server_display}"
            ),
            client_version=client_display,
            server_version=server_display,
        )
    if client.patch != server.patch:
        return CompatResult(
            severity="warn",
            message=(
                f"patch-level version skew: memd-wiki {client_display} "
                f"vs memd executable {server_display}. Proceeding."
            ),
            client_version=client_display,
            server_version=server_display,
        )
    return CompatResult(
        severity="ok",
        message=f"memd-wiki {client_display} matches memd executable {server_display}",
        client_version=client_display,
        server_version=server_display,
    )


class WikiManifestTooNewError(RuntimeError):
    """Raised when a manifest's schema_version exceeds memd-wiki's known max.

    The operator is expected to upgrade memd-wiki rather than have the
    reader silently degrade. Mirrors ``ServerIncompatibleError`` shape
    so CLI handlers can switch on either via ``isinstance``.
    """

    def __init__(self, manifest_version: int, client_max: int) -> None:
        super().__init__(
            f"manifest schema_version {manifest_version} is newer than "
            f"the maximum supported by this memd-wiki ({client_max}); "
            f"upgrade memd-wiki to read this wiki"
        )
        self.manifest_version = manifest_version
        self.client_max = client_max


def check_manifest_version(
    manifest: dict[str, Any] | None,
    *,
    client_max: int = MAX_KNOWN_MANIFEST_SCHEMA_VERSION,
) -> int | None:
    """Validate ``manifest['schema_version']`` against this build's max.

    - Returns the parsed integer schema_version when within the
      supported range.
    - Returns ``None`` when the manifest is missing or has no
      ``schema_version`` field (treat as v0 / unknown — caller
      decides whether to default-to-v1 or refuse).
    - Raises ``WikiManifestTooNewError`` when the manifest version
      exceeds ``client_max``.
    - Returns the integer when ``schema_version <= client_max`` (older
      manifests are forward-compat by design — v2 readers handle v1).
    """
    if not isinstance(manifest, dict):
        return None
    raw = manifest.get("schema_version")
    if raw is None:
        return None
    try:
        version = int(raw)
    except (TypeError, ValueError):
        # Malformed schema_version → treat as unknown rather than
        # crashing; downstream lint will flag manifest-invalid.
        return None
    if version > client_max:
        raise WikiManifestTooNewError(
            manifest_version=version, client_max=client_max
        )
    return version
