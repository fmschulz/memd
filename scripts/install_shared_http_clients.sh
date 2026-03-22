#!/usr/bin/env bash
set -euo pipefail

MEMD_URL="${MEMD_URL:-http://127.0.0.1:8787/mcp}"
APPEND_SNIPPETS=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --url)
      MEMD_URL="$2"
      shift 2
      ;;
    --append-snippets)
      APPEND_SNIPPETS=1
      shift
      ;;
    *)
      echo "usage: $0 [--url http://127.0.0.1:8787/mcp] [--append-snippets]" >&2
      exit 2
      ;;
  esac
done

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

append_once() {
  local target="$1"
  local marker="$2"
  local content="$3"

  mkdir -p "$(dirname "$target")"
  touch "$target"
  if grep -Fq "$marker" "$target"; then
    return
  fi

  {
    printf '\n%s\n' "$marker"
    printf '%s\n' "$content"
    printf '%s\n' "$marker"
  } >>"$target"
}

require_cmd codex
require_cmd claude

codex mcp remove memd >/dev/null 2>&1 || true
codex mcp add memd --url "$MEMD_URL"

claude mcp remove --scope user memd >/dev/null 2>&1 || true
claude mcp add --transport http --scope user memd "$MEMD_URL"

if [[ "$APPEND_SNIPPETS" -eq 1 ]]; then
  read -r -d '' SNIPPET <<'EOF' || true
Use the `memd` MCP server for shared memory across sessions and agents.

Before substantive work, search `memd` with the current `tenant_id`.
For meaningful work, record `task.start`, `task.progress`, `task.run_start`, `task.run_finish`, `task.add_evidence`, and `task.finish`.
Use the same `tenant_id` for agents that should share knowledge unless the user asks for a different memory scope.
EOF

  append_once "$HOME/.codex/AGENTS.md" "<!-- memd-shared-http -->" "$SNIPPET"
  append_once "$HOME/.claude/CLAUDE.md" "<!-- memd-shared-http -->" "$SNIPPET"
fi

printf 'Configured Codex CLI and Claude Code for %s\n' "$MEMD_URL"
