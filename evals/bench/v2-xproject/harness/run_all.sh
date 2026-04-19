#!/usr/bin/env bash
# Sweep every (agent × condition × question) cell.
set -u
bench_dir="$(cd "$(dirname "$0")/.." && pwd)"
log="$bench_dir/results/sweep.log"
: > "$log"

qids=$(python3 -c "import json; print(' '.join(q['id'] for q in json.load(open('$bench_dir/questions/prompts.json'))['questions']))")
for qid in $qids; do
  for agent in claude codex; do
    for cond in without with; do
      echo "=== $qid / $agent / $cond ===" >> "$log"
      "$bench_dir/harness/run.sh" "$agent" "$cond" "$qid" 2>> "$log"
      echo "" >> "$log"
    done
  done
done
echo "SWEEP DONE $(date -Iseconds)" >> "$log"
