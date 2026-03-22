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

upsert_block() {
  local target="$1"
  local start_marker="$2"
  local end_marker="$3"
  local content="$4"
  local tmp

  mkdir -p "$(dirname "$target")"
  touch "$target"
  tmp="$(mktemp)"

  python - "$target" "$start_marker" "$end_marker" "$content" >"$tmp" <<'PY'
from pathlib import Path
import sys

target = Path(sys.argv[1])
start = sys.argv[2]
end = sys.argv[3]
content = sys.argv[4]
text = target.read_text(encoding="utf-8") if target.exists() else ""

block = f"{start}\n{content}\n{end}\n"
if start in text and end in text:
    before, rest = text.split(start, 1)
    _, after = rest.split(end, 1)
    new_text = before.rstrip() + "\n\n" + block + after.lstrip("\n")
else:
    new_text = text.rstrip()
    if new_text:
        new_text += "\n\n"
    new_text += block

sys.stdout.write(new_text)
PY

  mv "$tmp" "$target"
}

require_cmd codex
require_cmd claude

codex mcp remove memd >/dev/null 2>&1 || true
codex mcp add memd --url "$MEMD_URL"

claude mcp remove --scope user memd >/dev/null 2>&1 || true
claude mcp add --transport http --scope user memd "$MEMD_URL"

if [[ "$APPEND_SNIPPETS" -eq 1 ]]; then
  read -r -d '' SNIPPET <<'EOF' || true
Use the `memd` MCP server as a shared knowledge base across sessions and agents.

Before substantive work, search `memd` with the current `tenant_id`.
For meaningful work, record `task.start`, `task.progress`, `task.run_start`, `task.run_finish`, `task.add_evidence`, and `task.finish`.
Use `artifact.create`, `artifact.search`, `artifact.get`, and `artifact.list_thread` when critique, revision, verification, or thread inspection matters.
Use the same `tenant_id` for agents that should share knowledge unless the user asks for a different memory scope.
EOF

  upsert_block "$HOME/.codex/AGENTS.md" "<!-- memd-shared-http:start -->" "<!-- memd-shared-http:end -->" "$SNIPPET"
  upsert_block "$HOME/.claude/CLAUDE.md" "<!-- memd-shared-http:start -->" "<!-- memd-shared-http:end -->" "$SNIPPET"
fi

printf 'Configured Codex CLI and Claude Code for %s\n' "$MEMD_URL"
