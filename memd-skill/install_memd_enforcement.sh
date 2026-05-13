#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_BIN="${HOME}/.local/bin"
INSTALL_BINARY=0

usage() {
  cat >&2 <<EOF
usage: $0 [--install-binary]

Installs the skill + CLI memd workflow by:
  - upserting CLI-first memd instructions into:
      ~/.codex/AGENTS.md
      ~/.claude/CLAUDE.md
  - optionally copying the bundled Linux memd binary into ~/.local/bin

This script does not register external client tools and does not install wrappers
or refusal guards.
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-binary)
      INSTALL_BINARY=1
      shift
      ;;
    --url|--install-wrappers)
      echo "$0 is CLI-only; URL registration and guarded wrappers were removed." >&2
      usage
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

stop_existing_warm_worker() {
  if command -v memd >/dev/null 2>&1; then
    memd warm stop >/dev/null 2>&1 || true
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

install_binary() {
  local src="${SCRIPT_DIR}/bin/linux-x64/memd"
  local target="${LOCAL_BIN}/memd"
  if [[ ! -x "$src" ]]; then
    echo "bundled memd binary not found or not executable: $src" >&2
    exit 1
  fi
  mkdir -p "$LOCAL_BIN"
  local tmp
  tmp="$(mktemp "${target}.tmp.XXXXXX")"
  cp "$src" "$tmp"
  chmod +x "$tmp"
  mv "$tmp" "$target"
}

require_cmd python

if [[ "$INSTALL_BINARY" -eq 1 ]]; then
  stop_existing_warm_worker
  install_binary
fi

read -r -d '' ENFORCEMENT_SNIPPET <<'EOF' || true
Mandatory `memd` CLI contract for substantive technical and scientific work:

- For non-trivial engineering, debugging, analysis, literature review, benchmarking, or multi-step scientific work, search `memd` first with the CLI using the current `tenant_id` and available `project_id`.
- Preferred retrieval command: `memd agent-context --tenant-id <tenant> --project-id <project> --query "<task>" --k 2 --token-budget 700 --format markdown --output .memd/context.md --log-dir .memd/search-logs`.
- For direct lookup, use `memd search --tenant-id <tenant> --project-id <project> --query "<query>" --compact --token-budget 2000 --format markdown`.
- Before saying the work is impossible, blocked, cannot be answered, or needs user context that might already exist in shared memory, run a relevant `memd` CLI search first. If no relevant record is found, say exactly what you checked.
- If the work changes understanding, runs tools, produces findings, or could matter to later sessions, record it with `memd add` before the final answer.
- Use `memd add --chunk-type summary|trace|decision|research|plan --tags kind:progress|run|evidence|decision|finish,... --text "<summary>"`; include `--project-id` when known.
- Do not provide a final substantive answer until the CLI retrieval and CLI write have both happened, unless `memd` is unavailable.
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

printf 'Installed memd skill + CLI enforcement.\n'
printf 'Updated:\n'
printf '  - %s\n' "${HOME}/.codex/AGENTS.md"
printf '  - %s\n' "${HOME}/.claude/CLAUDE.md"
if [[ "$INSTALL_BINARY" -eq 1 ]]; then
  printf 'Installed bundled memd CLI: %s\n' "${LOCAL_BIN}/memd"
fi
