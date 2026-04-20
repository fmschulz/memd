"""Minimal semver parser for memd-wiki ↔ memd server compat checks.

Covers the subset we care about:

- MAJOR.MINOR.PATCH with optional ``-prerelease`` and/or ``+build`` suffixes.
- Comparison on MAJOR.MINOR (the plan's compat gate granularity).
- Comparison on PATCH (for the patch-only warn path).

Vendored per the Item 7 plan §9.3 Parser choice: packaging.version.Version
is not stdlib, and a ~15-line MAJOR.MINOR.PATCH parser is sufficient for
memd's version policy (0.X.Y during the 0.x line, no pre-releases yet).
Any future version-policy change should revisit the shape handled here.
"""

from __future__ import annotations

from dataclasses import dataclass


class SemverParseError(ValueError):
    """Raised when a version string cannot be parsed as MAJOR.MINOR.PATCH."""


@dataclass(frozen=True)
class Semver:
    major: int
    minor: int
    patch: int

    def major_minor(self) -> tuple[int, int]:
        return (self.major, self.minor)

    def as_tuple(self) -> tuple[int, int, int]:
        return (self.major, self.minor, self.patch)

    def __str__(self) -> str:
        return f"{self.major}.{self.minor}.{self.patch}"


def parse(version: str) -> Semver:
    """Parse ``MAJOR.MINOR.PATCH`` (ignoring ``-prerelease`` / ``+build`` suffixes).

    Raises ``SemverParseError`` on malformed input.
    """
    if not isinstance(version, str):
        raise SemverParseError(f"version must be str, got {type(version).__name__}")
    core = version.strip()
    if not core:
        raise SemverParseError("version is empty")
    # Drop build metadata first, then prerelease.
    core = core.split("+", 1)[0]
    core = core.split("-", 1)[0]
    parts = core.split(".")
    if len(parts) != 3:
        raise SemverParseError(
            f"expected MAJOR.MINOR.PATCH, got {version!r}"
        )
    try:
        major = int(parts[0])
        minor = int(parts[1])
        patch = int(parts[2])
    except ValueError as exc:
        raise SemverParseError(f"non-integer component in {version!r}") from exc
    if major < 0 or minor < 0 or patch < 0:
        raise SemverParseError(f"negative component in {version!r}")
    return Semver(major=major, minor=minor, patch=patch)


def compare_major_minor(a: Semver, b: Semver) -> int:
    """Three-way compare on ``(MAJOR, MINOR)`` only. Returns -1 / 0 / 1."""
    lhs = a.major_minor()
    rhs = b.major_minor()
    if lhs < rhs:
        return -1
    if lhs > rhs:
        return 1
    return 0
