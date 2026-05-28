#!/usr/bin/env bash
#
# Check that every place the memd version is recorded agrees with
# the canonical version in workspace Cargo.toml.
#
# Sources checked:
#   - Cargo.toml                          [workspace.package].version  (canonical)
#   - tools/wiki/pyproject.toml           [project].version
#   - README.md                           shields.io version badge
#   - docs/index.md                       shields.io version badge
#   - CHANGELOG.md                        top "## [X.Y.Z]" header
#   - (advisory) latest git tag           must be >= canonical
#
# Exit codes:
#   0 — all version-bearing files agree
#   1 — drift detected; the failing file(s) and expected/actual are printed
#
# Designed to run unchanged in CI and locally.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

CANONICAL="$(awk -F\" '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)"
if [[ -z "${CANONICAL}" ]]; then
  echo "ERROR: could not extract canonical version from Cargo.toml" >&2
  exit 1
fi
echo "canonical = ${CANONICAL} (Cargo.toml [workspace.package])"

drift=0
report() {
  local file="$1" actual="$2"
  if [[ "${actual}" != "${CANONICAL}" ]]; then
    printf '  ✗ %s: expected %s, found %s\n' "${file}" "${CANONICAL}" "${actual}" >&2
    drift=1
  else
    printf '  ✓ %s: %s\n' "${file}" "${actual}"
  fi
}

# tools/wiki/pyproject.toml
wiki_version="$(awk -F\" '/^version[[:space:]]*=/ {print $2; exit}' tools/wiki/pyproject.toml)"
report "tools/wiki/pyproject.toml" "${wiki_version}"

# README.md badge — supports the shields.io 'version-X.Y.Z-color' pattern.
readme_version="$(grep -oE 'shields\.io/badge/version-[0-9]+\.[0-9]+\.[0-9]+' README.md | head -1 | awk -F- '{print $NF}')"
report "README.md (shields.io badge)" "${readme_version}"

# docs/index.md badge
index_version="$(grep -oE 'shields\.io/badge/version-[0-9]+\.[0-9]+\.[0-9]+' docs/index.md | head -1 | awk -F- '{print $NF}')"
report "docs/index.md (shields.io badge)" "${index_version}"

# CHANGELOG.md top entry — "## [X.Y.Z] - YYYY-MM-DD"
changelog_version="$(awk '/^## \[[0-9]+\.[0-9]+\.[0-9]+\]/ {match($0,/[0-9]+\.[0-9]+\.[0-9]+/); print substr($0,RSTART,RLENGTH); exit}' CHANGELOG.md)"
report "CHANGELOG.md (top entry)" "${changelog_version}"

# Advisory: latest git tag.
if latest_tag="$(git tag --sort=-creatordate | head -1)"; then
  if [[ -n "${latest_tag}" ]]; then
    tag_version="${latest_tag#v}"
    if [[ "${tag_version}" == "${CANONICAL}" ]]; then
      printf '  ✓ git tag %s matches canonical\n' "${latest_tag}"
    else
      printf '  ! git tag %s does not match canonical %s (advisory; no v%s release exists yet)\n' \
        "${latest_tag}" "${CANONICAL}" "${CANONICAL}"
    fi
  fi
fi

if (( drift != 0 )); then
  echo
  echo "Version drift detected. Update the file(s) above to ${CANONICAL} or bump Cargo.toml first." >&2
  exit 1
fi

echo
echo "All version-bearing files agree on ${CANONICAL}."
