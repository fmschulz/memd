#!/usr/bin/env bash
# run.sh <agent> <condition> <q_id>
#
# v2 correctness note: memd's segment payload files under ~/.memd/ are
# plaintext JSON, so any agent with unrestricted filesystem access can
# bypass the MCP boundary entirely. For a fair benchmark we pin
# Claude's tool restrictions (works) and accept that Codex, without a
# built-in read-path scope, will happily grep the payloads — we treat
# Codex as a secondary signal only.

set -u
agent="$1"
condition="$2"
q_id="$3"

bench_dir="$(cd "$(dirname "$0")/.." && pwd)"
runs_dir="$bench_dir/results/runs"
mkdir -p "$runs_dir"

prompt_and_cwd=$(python3 <<PY
import json,sys
d=json.load(open("$bench_dir/questions/prompts.json"))
for q in d["questions"]:
    if q["id"]=="$q_id":
        print(q["cwd"])
        print(q["prompt"])
        sys.exit(0)
sys.exit(1)
PY
)
if [ -z "$prompt_and_cwd" ]; then
  echo "unknown q_id: $q_id" >&2; exit 2
fi
cwd_rel=$(echo "$prompt_and_cwd" | head -1)
prompt=$(echo "$prompt_and_cwd" | tail -n +2)
cwd="$bench_dir/$cwd_rel"

out="$runs_dir/${agent}__${condition}__${q_id}.txt"
echo "[run] agent=$agent cond=$condition q=$q_id cwd=$cwd" >&2
start=$(date +%s)

cd "$cwd" || { echo "cd failed: $cwd"; exit 1; }

# Claude tool-restriction: block Read against ~/.memd and common Bash
# openings into it. The list is conservative — any extra command not
# listed here should not be able to egress ~/.memd either because the
# agent defaults to `Read` for plain files (which is the first entry).
CLAUDE_DISALLOW=(
  'Read(/home/fschulz/.memd/**)'
  'Read(~/.memd/**)'
  'Bash(cat *~/.memd*)'
  'Bash(cat */.memd/*)'
  'Bash(cat /home/fschulz/.memd*)'
  'Bash(rg *~/.memd*)'
  'Bash(rg */.memd/*)'
  'Bash(rg * /home/fschulz/.memd*)'
  'Bash(grep *~/.memd*)'
  'Bash(grep */.memd/*)'
  'Bash(grep *home/fschulz/.memd*)'
  'Bash(ls *~/.memd*)'
  'Bash(ls */.memd/*)'
  'Bash(strings *~/.memd*)'
  'Bash(strings */.memd/*)'
  'Bash(find */.memd/*)'
  'Bash(find *~/.memd*)'
  'Bash(find /home/fschulz/.memd*)'
  'Bash(head *~/.memd*)'
  'Bash(head */.memd/*)'
  'Bash(tail *~/.memd*)'
  'Bash(tail */.memd/*)'
  'Bash(less *~/.memd*)'
  'Bash(xxd *~/.memd*)'
  'Bash(hexdump *~/.memd*)'
  'Bash(od *~/.memd*)'
)

case "$agent.$condition" in
  claude.with)
    printf '%s' "$prompt" | timeout 240 claude -p --output-format text \
      --permission-mode bypassPermissions \
      --disallowedTools "${CLAUDE_DISALLOW[@]}" > "$out" 2>&1
    ;;
  claude.without)
    printf '%s' "$prompt" | timeout 240 claude -p --output-format text \
      --permission-mode bypassPermissions \
      --strict-mcp-config --mcp-config "$bench_dir/harness/no-memd.mcp.json" \
      --disallowedTools "${CLAUDE_DISALLOW[@]}" > "$out" 2>&1
    ;;
  codex.with)
    printf '%s' "$prompt" | timeout 240 codex exec \
      --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
      --config model_reasoning_effort=medium - > "$out" 2>&1
    ;;
  codex.without)
    printf '%s' "$prompt" | timeout 240 codex exec \
      --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
      --config model_reasoning_effort=medium -c 'mcp_servers={}' - > "$out" 2>&1
    ;;
  *) echo "unknown combo $agent.$condition"; exit 2;;
esac
rc=$?
end=$(date +%s)
echo "[done] rc=$rc elapsed=$((end-start))s output=$out" >&2
exit $rc
