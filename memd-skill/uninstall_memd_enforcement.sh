#!/usr/bin/env bash
set -euo pipefail

# Prefer python3: modern distros ship no `python` alias.
PYTHON_BIN="$(command -v python3 || command -v python || true)"

if [[ -z "$PYTHON_BIN" ]]; then
  echo "missing required command: python3 (or python)" >&2
  exit 1
fi

START_MARKER="<!-- memd-enforcement:start -->"
END_MARKER="<!-- memd-enforcement:end -->"
REMOVED=()

remove_block() {
  local target="$1"
  local tmp

  if [[ ! -f "$target" ]] || ! grep -Fq "$START_MARKER" "$target"; then
    echo "no memd enforcement block in $target"
    return
  fi

  # Stage beside the referent so the rename is atomic and does not replace a
  # symlink with a regular file (see the matching note in the installer).
  local real
  real="$(readlink -f "$target")"
  tmp="$(mktemp "$(dirname "$real")/.memd-enforcement.XXXXXX")"
  if "$PYTHON_BIN" - "$target" "$START_MARKER" "$END_MARKER" >"$tmp" <<'PY'
from pathlib import Path
import sys

target = Path(sys.argv[1])
start = sys.argv[2]
end = sys.argv[3]
text = target.read_text(encoding="utf-8") if target.exists() else ""

if start not in text or end not in text:
    print(f"warning: malformed memd enforcement block in {target}; left unchanged", file=sys.stderr)
    sys.stdout.write(text)
    sys.exit(1)

before, rest = text.split(start, 1)
_, after = rest.split(end, 1)
new_text = before.rstrip() + "\n\n" + after.lstrip("\n")
if new_text.strip():
    sys.stdout.write(new_text.strip("\n") + "\n")
else:
    sys.stdout.write("")
PY
  then
    chmod --reference="$real" "$tmp" 2>/dev/null || true
    mv "$tmp" "$real"
    echo "removed memd enforcement block from $target"
    REMOVED+=("$target enforcement block")
  else
    rm -f "$tmp"
  fi
}

remove_session_start_hook() {
  local settings="${HOME}/.claude/settings.json"
  local status

  if "$PYTHON_BIN" - "$settings" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.exists():
    print(f"warning: {path} missing; leaving SessionStart hook unchanged", file=sys.stderr)
    sys.exit(2)

try:
    data = json.loads(path.read_text(encoding="utf-8"))
except (json.JSONDecodeError, OSError) as exc:
    print(f"warning: could not parse {path}; leaving SessionStart hook unchanged: {exc}", file=sys.stderr)
    sys.exit(2)

if not isinstance(data, dict):
    print(f"warning: unexpected settings shape in {path}; leaving SessionStart hook unchanged", file=sys.stderr)
    sys.exit(2)

hooks = data.get("hooks")
if not isinstance(hooks, dict):
    print(f"no memd SessionStart hook in {path}")
    sys.exit(0)

session_start = hooks.get("SessionStart")
if not isinstance(session_start, list):
    print(f"no memd SessionStart hook in {path}")
    sys.exit(0)

kept = []
removed = False
for group in session_start:
    group_hooks = group.get("hooks", []) if isinstance(group, dict) else []
    has_memd_hook = any(
        isinstance(inner, dict) and "memd session-start" in str(inner.get("command", ""))
        for inner in group_hooks
    )
    if has_memd_hook:
        removed = True
    else:
        kept.append(group)

if not removed:
    print(f"no memd SessionStart hook in {path}")
    sys.exit(0)

if kept:
    hooks["SessionStart"] = kept
else:
    hooks.pop("SessionStart", None)
if not hooks:
    data.pop("hooks", None)

path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
print(f"removed memd SessionStart hook from {path}")
sys.exit(10)
PY
  then
    status=0
  else
    status=$?
  fi

  if [[ "$status" -eq 10 ]]; then
    REMOVED+=("$settings SessionStart hook")
  elif [[ "$status" -ne 0 && "$status" -ne 2 ]]; then
    exit "$status"
  fi
}

remove_block "${HOME}/.codex/AGENTS.md"
remove_block "${HOME}/.claude/CLAUDE.md"

CURSOR_RULE="${HOME}/.cursor/rules/memd.mdc"
if [[ -e "$CURSOR_RULE" || -L "$CURSOR_RULE" ]]; then
  rm -f "$CURSOR_RULE"
  echo "removed Cursor memd rule at $CURSOR_RULE"
  REMOVED+=("$CURSOR_RULE")
else
  echo "Cursor memd rule not present at $CURSOR_RULE"
fi

remove_session_start_hook

printf '\nSummary:\n'
if [[ "${#REMOVED[@]}" -eq 0 ]]; then
  printf '  removed: nothing\n'
else
  printf '  removed:\n'
  for item in "${REMOVED[@]}"; do
    printf '  - %s\n' "$item"
  done
fi
printf 'kept: ~/.memd (memory data) and per-project .memd/ directories — remove manually if desired.\n'
