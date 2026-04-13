#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MEMD_URL="${MEMD_URL:-http://127.0.0.1:8787/mcp}"
INSTALL_WRAPPERS=0
LOCAL_BIN="${HOME}/.local/bin"

usage() {
  cat >&2 <<EOF
usage: $0 [--url http://127.0.0.1:8787/mcp] [--install-wrappers]

Installs a stronger memd enforcement setup for Codex CLI and Claude Code by:
  - registering the memd MCP server for both clients
  - upserting stronger memd-usage instructions into:
      ~/.codex/AGENTS.md
      ~/.claude/CLAUDE.md
  - optionally installing convenience wrappers plus guarded one-shot wrappers in ~/.local/bin
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --url)
      [[ $# -ge 2 ]] || usage
      MEMD_URL="$2"
      shift 2
      ;;
    --install-wrappers)
      INSTALL_WRAPPERS=1
      shift
      ;;
    *)
      usage
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

install_wrapper() {
  local target="$1"
  local content="$2"
  mkdir -p "$LOCAL_BIN"
  printf '%s\n' "$content" >"$target"
  chmod +x "$target"
}

install_file() {
  local src="$1"
  local target="$2"
  mkdir -p "$LOCAL_BIN"
  cp "$src" "$target"
  chmod +x "$target"
}

require_cmd codex
require_cmd claude

codex mcp remove memd >/dev/null 2>&1 || true
codex mcp add memd --url "$MEMD_URL"

claude mcp remove --scope user memd >/dev/null 2>&1 || true
claude mcp add --transport http --scope user memd "$MEMD_URL"

read -r -d '' ENFORCEMENT_SNIPPET <<'EOF' || true
Mandatory `memd` contract for substantive technical and scientific work:

- For any non-trivial engineering, debugging, analysis, literature review, benchmarking, or multi-step scientific work, you MUST search `memd` first using the current `tenant_id` and the available `project_id` / `challenge_id` when they exist.
- Before saying the work is impossible, blocked, cannot be answered, or needs user context that might already exist in shared memory, you MUST consult `memd` first. At minimum, use the best-fit retrieval surface among `context.brief_project`, `task.resume` / `task.get`, `artifact.search`, `task.search`, `memory.search`, or the digest helpers. If trust matters, use `artifact.verify` before concluding that no grounded support exists.
- If `memd` returns no relevant record, say that explicitly. If you have not checked `memd`, you are not allowed to give up on substantive work.
- If the work changes understanding, runs tools, produces findings, or could matter to later sessions, you MUST record it in `memd`.
- Use `task.start` before substantive work, `task.progress` for meaningful checkpoints, `task.run_start` / `task.run_finish` for substantive runs, `task.add_evidence` for concrete evidence, and `task.finish` at the stopping point.
- Use `artifact.create`, `artifact.search`, `artifact.get`, and `artifact.list_thread` when critique, revision, verification, contributor tracking, or thread-level coordination matters.
- Do not provide a final substantive answer until the relevant `task_id` and/or `artifact_id` exist in `memd`.
- If `memd` is unavailable or misconfigured, treat that as a blocker and say so explicitly rather than silently skipping memory usage.
- Small talk, trivial one-shot questions, and purely local formatting rewrites do not require `memd`.
EOF

upsert_block \
  "${HOME}/.codex/AGENTS.md" \
  "<!-- memd-enforcement:start -->" \
  "<!-- memd-enforcement:end -->" \
  "$ENFORCEMENT_SNIPPET"

upsert_block \
  "${HOME}/.claude/CLAUDE.md" \
  "<!-- memd-enforcement:start -->" \
  "<!-- memd-enforcement:end -->" \
  "$ENFORCEMENT_SNIPPET"

if [[ "$INSTALL_WRAPPERS" -eq 1 ]]; then
  install_file "${SCRIPT_DIR}/memd_refusal_guard.py" "${LOCAL_BIN}/memd-refusal-guard"

  install_wrapper "${LOCAL_BIN}/codex-memd" "#!/usr/bin/env bash
set -euo pipefail
exec codex \"\$@\""

  install_wrapper "${LOCAL_BIN}/claude-memd" "#!/usr/bin/env bash
set -euo pipefail
exec claude \"\$@\""

  install_wrapper "${LOCAL_BIN}/codex-memd-guard" "#!/usr/bin/env bash
set -euo pipefail
exec \"${LOCAL_BIN}/memd-refusal-guard\" codex \"\$@\""

  install_wrapper "${LOCAL_BIN}/claude-memd-guard" "#!/usr/bin/env bash
set -euo pipefail
exec \"${LOCAL_BIN}/memd-refusal-guard\" claude \"\$@\""
fi

printf 'Installed memd enforcement for Codex CLI and Claude Code using %s\n' "$MEMD_URL"
printf 'Updated:\n'
printf '  - %s\n' "${HOME}/.codex/AGENTS.md"
printf '  - %s\n' "${HOME}/.claude/CLAUDE.md"
if [[ "$INSTALL_WRAPPERS" -eq 1 ]]; then
  printf 'Installed wrappers in %s:\n' "$LOCAL_BIN"
  printf '  - %s\n' "${LOCAL_BIN}/codex-memd"
  printf '  - %s\n' "${LOCAL_BIN}/claude-memd"
  printf '  - %s\n' "${LOCAL_BIN}/codex-memd-guard"
  printf '  - %s\n' "${LOCAL_BIN}/claude-memd-guard"
fi
