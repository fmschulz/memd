#!/usr/bin/env bash
# Verify no gold_phrase leaks into the filesystem outside the bench dir.
# Each gold_phrase must exist in memd only — if any appears elsewhere
# on disk, the question is contaminated and must be excluded.

set -u
bench_dir="$(cd "$(dirname "$0")/.." && pwd)"
facts_file="$bench_dir/seed/gold_facts.json"

if ! command -v rg >/dev/null; then
  echo "rg (ripgrep) required" >&2
  exit 2
fi

fail=0
# Extract all gold_phrase values (including cross_project_questions) as
# bare lines. Uses python because jq isn't guaranteed.
phrases=$(python3 -c '
import json,sys
d = json.load(open("'"$facts_file"'"))
for tenant, spec in d["tenants"].items():
    for f in spec["facts"]:
        print(f["gold_phrase"])
for q in d.get("cross_project_questions", []):
    print(q["gold_phrase"])
')

echo "[canary] checking ${#phrases} gold phrases against /home/fschulz outside bench dir..."
while IFS= read -r phrase; do
  # Escape regex-special chars crudely by using -F (fixed string).
  hits=$(rg -l -F --no-messages \
    --glob '!evals/bench/v2-xproject/**' \
    --glob '!.memd/**' \
    --glob '!**/node_modules/**' \
    --glob '!**/target/**' \
    --glob '!**/.git/**' \
    --glob '!**/.claude/projects/**' \
    --glob '!**/.cargo/**' \
    --glob '!**/.nvm/**' \
    --glob '!**/.pyenv/**' \
    --glob '!**/tmp/**' \
    "$phrase" /home/fschulz 2>/dev/null | head -3)
  if [ -n "$hits" ]; then
    echo "  LEAK: \"$phrase\" found in:"
    echo "$hits" | sed 's/^/    /'
    fail=1
  fi
done <<< "$phrases"

if [ "$fail" -eq 0 ]; then
  echo "[canary] OK — no gold phrases leak into the filesystem"
else
  echo "[canary] FAIL — contamination detected; address before running the bench"
fi
exit $fail
