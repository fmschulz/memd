#!/usr/bin/env bash
set -euo pipefail

INSTALL_BINARY=0
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

usage() {
  cat >&2 <<EOF
usage: $0 [--install-binary]

Current behavior:
  - installs the skill + CLI workflow instructions
  - optionally installs the bundled memd CLI into ~/.local/bin
  - does not register client integrations
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-binary)
      INSTALL_BINARY=1
      shift
      ;;
    --url|--append-snippets)
      echo "$0 is CLI-only; client URL registration was removed." >&2
      usage
      ;;
    *)
      usage
      ;;
  esac
done

ARGS=()
if [[ "${INSTALL_BINARY}" -eq 1 ]]; then
  ARGS+=(--install-binary)
fi

exec "${REPO_ROOT}/memd-skill/install_memd_enforcement.sh" "${ARGS[@]}"
