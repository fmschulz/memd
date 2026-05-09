#!/usr/bin/env bash
set -euo pipefail

bench_dir="$(cd "$(dirname "$0")/.." && pwd)"
run_set="${1:-pilot1}"
episode_filter="${2:-}"
agent_filter="${3:-codex,claude}"
condition_filter="${4:-without,full_mcp}"

if [[ -n "$episode_filter" ]]; then
  episodes="${episode_filter//,/ }"
else
  episodes=$(python3 - "$bench_dir/episodes.json" <<'PY'
import json
import sys

print(" ".join(e["id"] for e in json.load(open(sys.argv[1]))["episodes"]))
PY
  )
fi

agents="${agent_filter//,/ }"
conditions="${condition_filter//,/ }"

for agent in $agents; do
  for episode in $episodes; do
    for condition in $conditions; do
      "$bench_dir/harness/run_one.sh" "$condition" "$episode" "$run_set" "$agent"
    done
  done
done
