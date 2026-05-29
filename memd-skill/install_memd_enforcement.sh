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
  python - "$settings" <<'PY'
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

require_cmd python

if [[ "$INSTALL_BINARY" -eq 1 ]]; then
  stop_existing_warm_worker
  install_binary
fi

read -r -d '' ENFORCEMENT_SNIPPET <<'EOF' || true
Mandatory `memd` CLI contract for substantive technical and scientific work:

- At session start for substantive work, refresh and read project-root `memory.md`: `memd memory-md --project-dir . --output memory.md` when `.memd/project_scope.json` is available, otherwise include `--tenant-id <tenant>` and `--project-id <project>`.
- For non-trivial engineering, debugging, analysis, literature review, benchmarking, or multi-step scientific work, search `memd` first with the CLI using the current `tenant_id` and available `project_id`.
- Preferred retrieval command: `memd agent-context --tenant-id <tenant> --project-id <project> --query "<task>" --k 2 --token-budget 700 --format markdown --output .memd/context.md --log-dir .memd/search-logs`.
- For direct lookup, use `memd search --tenant-id <tenant> --project-id <project> --query "<query>" --compact --token-budget 2000 --format markdown`.
- Before saying the work is impossible, blocked, cannot be answered, or needs user context that might already exist in shared memory, run a relevant `memd` CLI search first. If no relevant record is found, say exactly what you checked.
- If the work changes understanding, runs tools, produces findings, or could matter to later sessions, record it with `memd add` before the final answer.
- Use `memd add --chunk-type summary|trace|decision|research|plan --tags kind:progress|run|evidence|decision|finish,priority:N,... --text "<summary>"`; include `--project-id` when known. Use `priority:N` for durable lessons that should be candidates for future `memory.md` refreshes.
- Keep durable writes bounded: a normal single task should leave fewer than 10 durable chunks, usually one decision, one evidence/run record, and one finish summary.
- Durable writes should contain a decision+rationale, validated fix/result, root cause, command/path/parameter/metric/version, evidence for a claim, or durable follow-up. Avoid "starting", "looking", or "made progress" notes without concrete outcomes.
- Do not store full chat logs, play-by-play transcripts, generated digest wrappers, or duplicate summaries that add no new evidence/tags/provenance. Store concise, durable facts that another agent is likely to reuse.
- If startup memory looks noisy, inspect it with `memd eval-memory-md --project-dir . --min-useful-ratio 0.8 --max-generated-wrappers 0` and `memd memory-md --project-dir . --output memory.md --explain-output .memd/memory-explain.json`.
- Do not store secrets or private credentials in `memd`: cookies, tokens, API keys, passwords, verification codes, ID numbers, bank cards, private contact details, third-party account configuration, or sensitive values copied from logs.
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
printf '\nRun '"'"'memd doctor'"'"' to verify the install.\n'
