#!/usr/bin/env bash
# Build isolated cases for a read-only reflection trial using the real CLI.
set -euo pipefail

MEMD_BIN=$(realpath "${1:?usage: seed.sh MEMD_BIN}")
CASE_ROOT=$(mktemp -d /var/tmp/memd-reflection.XXXXXX)
mkdir -p "$CASE_ROOT/project/.memd" "$CASE_ROOT/project/.agents/skills/cache-sync"
jq -n --arg root "$CASE_ROOT/project" \
  '{tenant_id:"reflection_trial",project_id:"demo",interface:"cli",cli_command:"memd",agent_context_output:".memd/context.md",project_dir:$root}' \
  > "$CASE_ROOT/project/.memd/project_scope.json"
cd "$CASE_ROOT/project"

memory() {
  "$MEMD_BIN" --data-dir "$CASE_ROOT/store" --search-variant bm25-only "$@"
}
experience() {
  memory experience --tenant-id reflection_trial --project-id demo --warm off "$@"
}
cat > .agents/skills/cache-sync/SKILL.md <<'SKILL'
---
name: cache-sync
description: Repair and verify a cache configuration that rejects its format.
---
# Cache configuration repair

Inspect the cache configuration and record the failed validation. Normalize its
line endings, require its content to equal `ready`, then run the configuration
check again. Keep the failed and passing check receipts. This fixture covers
cache-check version 1 on the local machine.
SKILL
cat > .agents/AGENTS.md <<'RULES'
# Fixture rules

Canonical source: .agents/AGENTS.md. CLAUDE.md is generated from this file.
Write reports with decorative emoji markers.
RULES
cp .agents/AGENTS.md CLAUDE.md

# These small checks validate fixture contents, not a production workflow.
cat > "$CASE_ROOT/cases.tsv" <<'CASES'
manifest_a	manifest-check	manifest schema rejected	schema=2	Read the manifest schema, set schema=2, and rerun the schema check.
manifest_b	manifest-check	manifest schema rejected	schema=2	Read the manifest schema, set schema=2, and rerun the schema check.
cache_a	cache-check	cache format rejected	ready	Normalize the cache configuration, require ready, and rerun the cache check.
cache_b	cache-check	cache format rejected	ready	Normalize the cache configuration, require ready, and rerun the cache check.
queue_a	queue-check	queue worker count mismatch	workers=1	Set workers=1 and rerun the queue check.
queue_b	queue-check	queue worker count mismatch	workers=4	Set workers=4 and rerun the queue check.
CASES
: > "$CASE_ROOT/cases.jsonl"
while IFS=$'\t' read -r label tool symptom expected guidance; do
  problem=$(experience record --json "$(jq -n --arg symptom "$symptom" \
    --arg label "$label" '{event:{kind:"problem",description:($symptom+" in "+$label),symptom_terms:[$symptom]}}')" | jq -er .artifact_id)
  printf '%s\n' broken > "$label.conf"
  failed_attempt=$(experience record --json "$(jq -n --arg problem "$problem" \
    '{event:{kind:"attempt",problem_id:$problem,action:"Check the original fixture configuration"}}')" | jq -er .artifact_id)
  experience check "$failed_attempt" --evidence "$label.conf" -- \
    sh -c 'test "$(cat "$1")" = "$2"' _ "$label.conf" "$expected" \
    | jq -e '.status == "failed"' > /dev/null
  printf '%s\n' "$expected" > "$label.conf"
  attempt=$(experience record --json "$(jq -n --arg problem "$problem" \
    --arg previous "$failed_attempt" --arg guidance "$guidance" \
    '{event:{kind:"attempt",problem_id:$problem,previous_attempt_id:$previous,action:$guidance}}')" | jq -er .artifact_id)
  check=$(experience check "$attempt" --evidence "$label.conf" -- \
    sh -c 'test "$(cat "$1")" = "$2"' _ "$label.conf" "$expected")
  jq -e '.status == "passed"' <<< "$check" > /dev/null
  check_id=$(jq -er .artifact_id <<< "$check")
  lesson=$(experience record --json "$(jq -n --arg problem "$problem" \
    --arg check "$check_id" --arg machine "$(hostname)" --arg tool "$tool" \
    --arg symptom "$symptom" --arg guidance "$guidance" --arg path "$label.conf" \
    '{provenance:{path:$path},event:{kind:"lesson",problem_id:$problem,supporting_check_id:$check,guidance:$guidance,symptom_terms:[$symptom],applicability:{machine:$machine,tool:$tool,version:"1"},visibility:"project"}}')" | jq -er .artifact_id)
  jq -n --arg label "$label" --arg problem "$problem" --arg check "$check_id" \
    --arg lesson "$lesson" '{label:$label,problem_id:$problem,check_id:$check,lesson_id:$lesson}' \
    >> "$CASE_ROOT/cases.jsonl"
done < "$CASE_ROOT/cases.tsv"

experience record --json '{"event":{"kind":"problem","description":"Unverified storage repair suggestion","symptom_terms":["storage cache repair"]}}' \
  > "$CASE_ROOT/unchecked.json"
source_id=$(memory add --tenant-id reflection_trial --project-id demo --warm off \
  --chunk-type doc --text 'The fixture reports currently use decorative emoji markers.' \
  | jq -er .chunk_id)
export MEMD_CONSOLIDATOR=mock
MEMD_CONSOLIDATOR_MOCK_RESPONSE=$(jq -nc --arg id "$source_id" \
  '[{text:"Reports should use decorative emoji markers.",agent_action:"Keep decorative emoji markers when formatting a fixture report.",evidence:[$id],confidence:0.99,supersedes:[$id],priority:7}]')
export MEMD_CONSOLIDATOR_MOCK_RESPONSE
memory consolidate --tenant-id reflection_trial --project-id demo --project-dir . \
  --force --warm off > "$CASE_ROOT/staged.json"
run_id=$(jq -er .run_id "$CASE_ROOT/staged.json")
memory consolidate-review --list --limit 5 > "$CASE_ROOT/list.json"
memory consolidate-review "$run_id" > "$CASE_ROOT/inspection.json"
memory call task.search --warm off --json '{"tenant_id":"reflection_trial","query":"","filters":{"project_id":"demo","artifact_role":"experience_check","status":"passed"},"k":20}' \
  > "$CASE_ROOT/check-search.json"
jq -n --arg root "$CASE_ROOT" --arg binary "$MEMD_BIN" --arg run "$run_id" \
  --slurpfile cases "$CASE_ROOT/cases.jsonl" \
  '{fixture_root:$root,binary:$binary,tenant_id:"reflection_trial",project_id:"demo",run_id:$run,cases:$cases}' \
  | tee "$CASE_ROOT/manifest.json"
