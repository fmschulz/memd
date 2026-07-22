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
  - writing the Cursor user rule to ~/.cursor/rules/memd.mdc
  - wiring a Claude Code SessionStart hook in ~/.claude/settings.json
  - optionally installing the latest memd release binary into ~/.local/bin

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

# Prefer python3: modern distros ship no `python` alias.
PYTHON_BIN="$(command -v python3 || command -v python || true)"

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

  "$PYTHON_BIN" - "$target" "$start_marker" "$end_marker" "$content" >"$tmp" <<'PY'
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
  require_cmd curl
  # Download the latest prebuilt binary via the cargo-dist installer. Linux
  # builds are static musl (no glibc-version errors); macOS arm64/x64 supported.
  # Installs to ~/.local/bin (the dist install-path). Requires a published dist
  # release (memd >= the first release built by .github/workflows/release.yml).
  echo "Installing latest memd via the cargo-dist installer..." >&2
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
}

# Idempotently wire a Claude Code SessionStart hook that refreshes
# memory.md and kicks a background consolidation. Existing settings
# are preserved; the hook is only appended when not already present.
wire_session_start_hook() {
  local settings="${HOME}/.claude/settings.json"
  mkdir -p "$(dirname "$settings")"
  "$PYTHON_BIN" - "$settings" <<'PY'
import json, sys
from pathlib import Path

path = Path(sys.argv[1])
try:
    data = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
except (json.JSONDecodeError, OSError):
    print("could not parse %s; leaving SessionStart hook unset" % path, file=sys.stderr)
    sys.exit(0)
if not isinstance(data, dict):
    print("unexpected settings shape; leaving SessionStart hook unset", file=sys.stderr)
    sys.exit(0)

command = 'memd session-start --project-dir "${CLAUDE_PROJECT_DIR:-.}" 2>/dev/null || true'
hooks = data.setdefault("hooks", {})
session_start = hooks.setdefault("SessionStart", [])

already = any(
    "memd session-start" in inner.get("command", "")
    for group in session_start
    for inner in group.get("hooks", [])
)
if already:
    print("SessionStart hook already present; left unchanged")
    sys.exit(0)

session_start.append({"hooks": [{"type": "command", "command": command}]})
path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
print("added memd SessionStart hook to %s" % path)
PY
}

if [[ -z "$PYTHON_BIN" ]]; then
  echo "missing required command: python3 (or python)" >&2
  exit 1
fi

if [[ "$INSTALL_BINARY" -eq 1 ]]; then
  stop_existing_warm_worker
  install_binary
fi

read -r -d '' ENFORCEMENT_SNIPPET <<'EOF' || true
Use the `memd` CLI for shared local memory on substantive technical and scientific work. Load the memd skill for full commands and write-quality rules.

- Session start: refresh and read project-root `memory.md` with `memd memory-md`.
- Before substantive work, and before declaring anything blocked or unknowable, search with `memd agent-context` or `memd search`. Say what you checked when no record matches.
- Before the final answer, persist reusable decisions, findings, fixes, or evidence with `memd add`.
- Time-anchored facts: store `event_time_ms` through `memory.add` or `batch`; recall with `render_event_time: true`. Do not put dates in the text. (v1.3+)
- Document-per-add stores: search with `--dedupe-by-source`; leave it off for conversational stores. Retry `memd:dense-index-busy` writes after the repair; reads fall back automatically. (v1.3.1+)
- Long writes: retain every `stored_chunk_ids` value. Use individual adds instead of `memory.add_batch` when later retrieval or outcome attribution needs all split-child IDs. (v1.5+)
- Iterative improvement: retain `retrieval_episode_id` when memory affects a task, then record only independently verified `--used` or `--harmful` chunks with `memd outcome`. Agent self-reports are audit-only; `outcome-v1` is shadow-only. (v1.5+)
- Consolidation is review-gated: `memd consolidate` stages hidden candidates. List them with `memd consolidate-review --list`, then explicitly accept or reject. Session start cannot promote a run without durable prior promotion intent. (v1.5+)
- Reproducible retrieval: call `memory.search` with `ranking_time_ms` and require `retrieval_episode_id: null`. This pins ranking decay but is not a lifecycle snapshot. (v1.5+)
- Never store secrets, credentials, PII, or sensitive log values.
- Treat unavailable or misconfigured memd as a blocker. Small talk, trivial one-shot questions, and local formatting rewrites do not require memd.
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

wire_session_start_hook

# Install Cursor user-level rule so Cursor reads the same memd CLI
# contract that Claude Code and Codex see. Cursor reads .mdc files
# under ~/.cursor/rules/; a rule with `alwaysApply: true` is loaded
# into every conversation system prompt.
install_cursor_rule() {
  local rule_dir="${HOME}/.cursor/rules"
  local rule_path="${rule_dir}/memd.mdc"
  mkdir -p "$rule_dir"
  cat >"$rule_path" <<EOF
---
description: memd CLI contract for shared local memory across sessions
alwaysApply: true
---

${ENFORCEMENT_SNIPPET}
EOF
  # Pin a deterministic mode rather than depending on the caller's
  # umask. The rule isn't sensitive; we just don't want it to be 0600
  # on some hosts and 0644 on others.
  chmod 0644 "$rule_path"
}

install_cursor_rule

printf 'Installed memd skill + CLI enforcement.\n'
printf 'Updated:\n'
printf '  - %s\n' "${HOME}/.codex/AGENTS.md"
printf '  - %s\n' "${HOME}/.claude/CLAUDE.md"
printf '  - %s (Cursor user rule)\n' "${HOME}/.cursor/rules/memd.mdc"
printf '  - %s (SessionStart hook)\n' "${HOME}/.claude/settings.json"
printf 'Codex: copy memd-skill/examples/codex_session_start_hook.json into your\n'
printf '       project .codex/hooks.json to enable the equivalent hook.\n'
if [[ "$INSTALL_BINARY" -eq 1 ]]; then
  printf 'Installed bundled memd CLI: %s\n' "${LOCAL_BIN}/memd"
fi
printf '\nRun '"'"'memd doctor'"'"' to verify the install ('"'"'memd doctor --strict'"'"' exits non-zero on failure; a fresh store reports data dir/project scope as pending until your first session-start).\n'
