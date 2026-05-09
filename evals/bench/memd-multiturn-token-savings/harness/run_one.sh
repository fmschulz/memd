#!/usr/bin/env bash
# run_one.sh <condition> <episode_id> [run_set] [agent]
#
# condition: without | full_mcp | thin_mcp | cli_search | cli_prefetch
#            with is accepted as an alias for full_mcp
# agent:     codex | claude

set -euo pipefail

requested_condition="$1"
episode_id="$2"
run_set="${3:-pilot1}"
agent="${4:-${MEMD_MT_AGENT:-codex}}"

case "$requested_condition" in
  with)
    condition="full_mcp"
    ;;
  without|full_mcp|thin_mcp|cli_search|cli_prefetch)
    condition="$requested_condition"
    ;;
  *)
    echo "unknown condition: $requested_condition" >&2
    exit 2
    ;;
esac

bench_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(cd "$bench_dir/../../.." && pwd)"
results_dir="$bench_dir/results/$run_set"
mkdir -p \
  "$results_dir/runs" \
  "$results_dir/final" \
  "$results_dir/metrics" \
  "$results_dir/retrieval" \
  "$results_dir/worktrees" \
  "$results_dir/tests" \
  "$results_dir/diffs" \
  "$results_dir/metadata"

episode_json=$(python3 - "$bench_dir/episodes.json" "$episode_id" <<'PY'
import json
import sys

episodes = json.load(open(sys.argv[1]))["episodes"]
for item in episodes:
    if item["id"] == sys.argv[2]:
        print(json.dumps(item))
        raise SystemExit(0)
raise SystemExit(f"unknown episode id: {sys.argv[2]}")
PY
)

fixture_rel=$(python3 - <<PY
import json
e = json.loads('''$episode_json''')
print(e["fixture"])
PY
)
target_tests=$(python3 - <<PY
import json
e = json.loads('''$episode_json''')
print(e["target_tests"])
PY
)
tenant_id=$(python3 - <<PY
import json
e = json.loads('''$episode_json''')
print(e["tenant_id"])
PY
)
project_id=$(python3 - <<PY
import json
e = json.loads('''$episode_json''')
print(e["project_id"])
PY
)
experience_id=$(python3 - <<PY
import json
e = json.loads('''$episode_json''')
print(e["experience_id"])
PY
)

fixture_dir="$bench_dir/$fixture_rel"
cell="${agent}__${condition}__${episode_id}"
workdir="$results_dir/worktrees/$cell"
run_path="$results_dir/runs/$cell.txt"
final_path="$results_dir/final/$cell.txt"
pre_metrics="$results_dir/metrics/${cell}__pre.json"
post_metrics="$results_dir/metrics/${cell}__post.json"
post_test_path="$results_dir/tests/$cell.txt"
diff_path="$results_dir/diffs/$cell.diff"
metadata_path="$results_dir/metadata/$cell.json"
retrieval_cell_dir="$results_dir/retrieval/$cell"

if [[ "$agent" != "codex" && "$agent" != "claude" ]]; then
  echo "unknown agent: $agent" >&2
  exit 2
fi

rm -rf "$workdir"
cp -R "$fixture_dir" "$workdir"
rm -rf "$workdir"/__pycache__
mkdir -p "$workdir/.bench"
cp "$bench_dir/tools/memd_search.py" "$workdir/.bench/memd_search.py"
chmod +x "$workdir/.bench/memd_search.py"

if [[ -n "${MEMD_MT_MEMD_BIN:-}" ]]; then
  memd_cmd=("$MEMD_MT_MEMD_BIN")
elif [[ -x "$repo_root/target/debug/memd" ]]; then
  memd_cmd=("$repo_root/target/debug/memd")
else
  memd_cmd=(cargo run --quiet -p memd --)
fi

if [[ "$condition" == "cli_prefetch" ]]; then
  mkdir -p "$workdir/.bench/memd-search-logs"
  (
    cd "$repo_root"
    "${memd_cmd[@]}" agent-context \
      --tenant-id "$tenant_id" \
      --project-id "$project_id" \
      --query "$experience_id repair rules" \
      --k "${MEMD_MT_PREFETCH_K:-2}" \
      --token-budget "${MEMD_MT_PREFETCH_TOKEN_BUDGET:-700}" \
      --format markdown \
      --output "$workdir/.bench/memd-context.md" \
      --log-dir "$workdir/.bench/memd-search-logs" \
      --url "${MEMD_MT_MEMD_URL:-${MEMD_MCP_URL:-http://127.0.0.1:8787/mcp}}"
  )
fi

prompt=$(python3 - <<PY
import json
import pathlib
import shlex
e = json.loads('''$episode_json''')
condition = "$condition"
base = e["prompt"]
if condition == "full_mcp":
    print(
        "Controlled multi-turn token-savings benchmark. "
        "You are in a copied fixture workspace. Prior debugging experience may exist in memd. "
        "Before broad exploration, call memory.search for relevant prior failures or repair rules. "
        "Use only memories that match current evidence. "
        f"Search tenant_id={e['tenant_id']} and project_id={e['project_id']}. "
        f"The expected useful prior, if relevant, has experience_id={e['experience_id']}. "
        "Patch the workspace directly. "
        + base
    )
elif condition == "thin_mcp":
    print(
        "Controlled multi-turn token-savings benchmark. "
        "You are in a copied fixture workspace. Prior debugging experience may exist in memd. "
        "Before broad exploration, call the single thin MCP retrieval tool named memd_search "
        "for relevant prior failures or repair rules. Use only memories that match current evidence. "
        f"Search tenant_id={e['tenant_id']} and project_id={e['project_id']}. "
        f"The expected useful prior, if relevant, has experience_id={e['experience_id']}. "
        "Patch the workspace directly. "
        + base
    )
elif condition == "cli_search":
    query = f"{e['experience_id']} repair rules"
    command = shlex.join([
        "python3",
        ".bench/memd_search.py",
        "--tenant-id",
        e["tenant_id"],
        "--project-id",
        e["project_id"],
        "--query",
        query,
        "--k",
        "5",
        "--token-budget",
        "1200",
        "--log-dir",
        ".bench/memd-search-logs",
        "--pretty",
    ])
    print(
        "Controlled multi-turn token-savings benchmark. "
        "You are in a copied fixture workspace. External MCP memory tools are unavailable, "
        "but a read-only direct CLI retrieval wrapper is available. "
        "Before broad exploration, run this exact command and use only results that match current evidence: "
        f"{command}. "
        f"The expected useful prior, if relevant, has experience_id={e['experience_id']}. "
        "Patch the workspace directly. "
        + base
    )
elif condition == "cli_prefetch":
    context_path = pathlib.Path("$workdir/.bench/memd-context.md")
    context = context_path.read_text() if context_path.exists() else ""
    print(
        "Controlled multi-turn token-savings benchmark. "
        "You are in a copied fixture workspace. External MCP memory tools are unavailable. "
        "A controller-side CLI-only memd prefetch has already run before this agent invocation. "
        "Use the embedded memory context below only when it matches current file and test evidence. "
        f"The expected useful prior, if relevant, has experience_id={e['experience_id']}. "
        "Patch the workspace directly. "
        "Prefetched memd context follows.\\n\\n"
        + context
        + "\\n\\nTask follows. "
        + base
    )
else:
    print(
        "Controlled multi-turn token-savings benchmark. "
        "You are in a copied fixture workspace. External memory is unavailable. "
        "Solve from the current files, tests, and logs only. "
        "Do not use memd or any external memory. Patch the workspace directly. "
        + base
    )
PY
)

tmp_home=$(mktemp -d /tmp/memd-mt.XXXXXX)
cleanup() {
  rm -rf "$tmp_home" || true
}
trap cleanup EXIT

if [[ -f "$HOME/.codex/auth.json" ]]; then
  ln -s "$HOME/.codex/auth.json" "$tmp_home/auth.json"
fi

python3 - "$condition" "$tmp_home/config.toml" "$bench_dir/tools/thin_mcp_search_server.py" "$workdir/.bench/memd-search-logs" <<'PY'
import json
import pathlib
import sys

condition = sys.argv[1]
out = pathlib.Path(sys.argv[2])
thin_server = pathlib.Path(sys.argv[3])
thin_log_dir = pathlib.Path(sys.argv[4])
src = pathlib.Path.home() / ".codex/config.toml"

if condition == "full_mcp" and src.exists():
    out.write_text(src.read_text())
    raise SystemExit(0)

skip = False
lines = []
if src.exists():
    for line in src.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            skip = stripped.startswith("[mcp_servers.memd")
        if not skip:
            lines.append(line)
if condition == "thin_mcp":
    if lines and lines[-1].strip():
        lines.append("")
    lines.extend([
        "[mcp_servers.memdthin]",
        'command = "env"',
        f"args = [{json.dumps(f'MEMD_THIN_LOG_DIR={thin_log_dir}')}, \"python3\", {json.dumps(str(thin_server))}]",
        "",
        '[mcp_servers.memdthin.tools."memd_search"]',
        'approval_mode = "approve"',
    ])
out.write_text("\n".join(lines) + "\n")
PY

echo "[run] agent=$agent condition=$condition episode=$episode_id cwd=$workdir" >&2
if [[ "$condition" == "full_mcp" || "$condition" == "thin_mcp" || "$condition" == "cli_search" || "$condition" == "cli_prefetch" ]]; then
  python3 "$bench_dir/tools/metrics_snapshot.py" > "$pre_metrics"
fi

start=$(date +%s)
set +e
if [[ "$agent" == "codex" ]]; then
  codex_sandbox="${MEMD_MT_CODEX_SANDBOX:-workspace-write}"
  if [[ "$condition" == "cli_search" ]]; then
    codex_sandbox="${MEMD_MT_CODEX_CLI_SANDBOX:-danger-full-access}"
  fi
  codex_args=(
    exec
    --model "${CODEX_MODEL:-gpt-5.5}"
    --skip-git-repo-check
    --ephemeral
    --ignore-rules
    --sandbox "$codex_sandbox"
    --config "model_reasoning_effort=${MEMD_MT_CODEX_REASONING_EFFORT:-low}"
    -C "$workdir"
    -o "$final_path"
  )
  printf '%s' "$prompt" | CODEX_HOME="$tmp_home" MEMD_THIN_LOG_DIR="$workdir/.bench/memd-search-logs" timeout "${MEMD_MT_TIMEOUT:-360}" codex "${codex_args[@]}" - > "$run_path" 2>&1
  cli_rc=$?
else
  claude_mcp="$tmp_home/claude-mcp.json"
  if [[ "$condition" == "full_mcp" ]]; then
    cat > "$claude_mcp" <<'JSON'
{
  "mcpServers": {
    "memd": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
JSON
  elif [[ "$condition" == "thin_mcp" ]]; then
    python3 - "$claude_mcp" "$bench_dir/tools/thin_mcp_search_server.py" "$workdir/.bench/memd-search-logs" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
thin_server = pathlib.Path(sys.argv[2])
thin_log_dir = pathlib.Path(sys.argv[3])
path.write_text(json.dumps({
    "mcpServers": {
        "memdthin": {
            "type": "stdio",
            "command": "env",
            "args": [f"MEMD_THIN_LOG_DIR={thin_log_dir}", "python3", str(thin_server)],
        }
    }
}, indent=2) + "\n")
PY
  else
    printf '{"mcpServers":{}}\n' > "$claude_mcp"
  fi

  allowed_tools="Read,Edit,MultiEdit,Write,Bash"
  if [[ "$condition" == "full_mcp" ]]; then
    allowed_tools="$allowed_tools,mcp__memd__memory_search"
  elif [[ "$condition" == "thin_mcp" ]]; then
    allowed_tools="$allowed_tools,mcp__memdthin__memd_search"
  fi

  claude_args=(
    -p
    --verbose
    --output-format stream-json
    --no-session-persistence
    --model "${CLAUDE_MODEL:-sonnet}"
    --effort "${CLAUDE_EFFORT:-low}"
    --permission-mode bypassPermissions
    --setting-sources project
    --mcp-config "$claude_mcp"
    --strict-mcp-config
    --allowedTools "$allowed_tools"
    --max-budget-usd "${CLAUDE_MAX_BUDGET_USD:-1.50}"
  )
  (cd "$workdir" && printf '%s' "$prompt" | MEMD_THIN_LOG_DIR="$workdir/.bench/memd-search-logs" timeout "${MEMD_MT_TIMEOUT:-360}" claude "${claude_args[@]}") > "$run_path" 2>&1
  cli_rc=$?
  python3 - "$run_path" "$final_path" <<'PY'
import json
import pathlib
import sys

run_path = pathlib.Path(sys.argv[1])
final_path = pathlib.Path(sys.argv[2])
result = ""
for line in run_path.read_text(errors="replace").splitlines():
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("type") == "result":
        result = event.get("result") or ""
final_path.write_text(result)
PY
fi
set -e
end=$(date +%s)

if [[ "$condition" == "full_mcp" || "$condition" == "thin_mcp" || "$condition" == "cli_search" || "$condition" == "cli_prefetch" ]]; then
  python3 "$bench_dir/tools/metrics_snapshot.py" > "$post_metrics"
fi

set +e
(cd "$workdir" && bash -lc "$target_tests") > "$post_test_path" 2>&1
test_rc=$?
diff -ru --exclude='__pycache__' "$fixture_dir" "$workdir" > "$diff_path" 2>&1
diff_rc=$?
set -e

rm -rf "$retrieval_cell_dir"
if [[ -d "$workdir/.bench/memd-search-logs" ]]; then
  mkdir -p "$retrieval_cell_dir"
  cp -R "$workdir/.bench/memd-search-logs/." "$retrieval_cell_dir"/
fi

python3 - "$metadata_path" <<PY
import json
import pathlib

path = pathlib.Path("$metadata_path")
path.write_text(json.dumps({
    "agent": "$agent",
    "requested_condition": "$requested_condition",
    "condition": "$condition",
    "interface_condition": "$condition",
    "episode_id": "$episode_id",
    "tenant_id": "$tenant_id",
    "project_id": "$project_id",
    "experience_id": "$experience_id",
    "workdir": "$workdir",
    "retrieval_log_dir": "$retrieval_cell_dir",
    "target_tests": "$target_tests",
    "cli_rc": $cli_rc,
    "test_rc": $test_rc,
    "diff_rc": $diff_rc,
    "elapsed_seconds": $((end-start)),
}, indent=2) + "\\n")
PY

echo "[done] cli_rc=$cli_rc test_rc=$test_rc elapsed=$((end-start))s run=$run_path final=$final_path" >&2
exit 0
