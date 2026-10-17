# Record and reuse a checked repair

Use Bash and `jq` from a memd source checkout. Build `target/debug/memd` with
`cargo build -p memd` before starting. The commands below use temporary stores
and a small configuration file; they do not alter the normal memory store.

## Record a solved problem in one command

When you have solved a problem, write a case file and record it with
`experience note`. This is the normal way to capture a case.

```bash
set -euo pipefail
MEMD_BIN="$(pwd)/target/debug/memd"
NOTE_ROOT=$(mktemp -d)
cat > "$NOTE_ROOT/case.json" <<'JSON'
{
  "problem": "Demo service rejects its configuration",
  "symptoms": ["configuration rejected"],
  "failed": ["restart the service without changing the file"],
  "fix": "Set the demo configuration to ready, then run the configuration check.",
  "check": {"method": "observed", "result": "demo-check exited 0", "command": "demo-check config"},
  "applies_when": {"tool": "demo-check", "version": "1"},
  "evidence": ["docs/experience-how-to.md"]
}
JSON
exp() {
  "$MEMD_BIN" --data-dir "$NOTE_ROOT/store" --search-variant bm25-only \
    experience --tenant-id tutorial --project-id demo "$@"
}
"$MEMD_BIN" experience note "$NOTE_ROOT/case.json" --emit | jq -e '.visibility == "shared"'
exp note "$NOTE_ROOT/case.json" --shared-id EXP-1 | jq -e '.evidence == "observed"'
exp find 'configuration rejected' --tool demo-check --version 1 \
  | jq -e '.status == "matches" and .lessons[0].evidence == "observed"'
exp find 'configuration rejected' --tool demo-check --version 1 --project-only \
  | jq -e '.status == "matches"'
"$MEMD_BIN" --data-dir "$NOTE_ROOT/store" --search-variant bm25-only warm stop
```

`--emit` checks the file and prints the canonical case without opening a store.
`--shared-id` ties the local copy to an external exchange and makes a repeated
call return the stored case. The lesson reads `evidence: "observed"` because
memd did not run the command; the sections below show the audit path, where
memd runs the check and the lesson reads `executed`. Run `find` before you
try a fix, and read `evidence` in the result: an observed lesson is a lead to
recheck, not a verified fact. The file format is in
[Experience memory](experience-memory.md#a-case-file-records-a-solved-problem-in-one-call).

## Audit path: record each step

The remaining sections record the same kind of case one step at a time and let
memd run the verifying command.

### Record the failure

```bash
set -euo pipefail
MEMD_BIN="$(pwd)/target/debug/memd"
CASE_ROOT=$(mktemp -d)
TARGET_MACHINE=$(hostname)
memory() {
  "$MEMD_BIN" --data-dir "$CASE_ROOT/store" --search-variant bm25-only \
    experience --tenant-id tutorial --project-id demo "$@"
}
printf '%s\n' broken > "$CASE_ROOT/config"
PROBLEM=$(memory record --json '{"provenance":{"path":"docs/experience-how-to.md"},"event":{"kind":"problem","description":"Demo service rejects its configuration","symptom_terms":["configuration rejected"]}}' | jq -er .artifact_id)
ATTEMPT_ONE=$(memory record --json "$(jq -n --arg problem "$PROBLEM" \
  '{event:{kind:"attempt",problem_id:$problem,action:"Check the current configuration"}}')" | jq -er .artifact_id)
memory check "$ATTEMPT_ONE" --evidence "$CASE_ROOT/config" -- \
  sh -c 'test "$(cat "$1")" = ready' _ "$CASE_ROOT/config" \
  | jq -e '.status == "failed"'
```

A completed check command stores a receipt even when the verifier fails.
Inspect the receipt's `status`; a successful memd invocation means that the
receipt was recorded.

### Repair, check, and state the lesson

```bash
printf '%s\n' ready > "$CASE_ROOT/config"
ATTEMPT_TWO=$(memory record --json "$(jq -n --arg problem "$PROBLEM" --arg previous "$ATTEMPT_ONE" \
  '{event:{kind:"attempt",problem_id:$problem,previous_attempt_id:$previous,action:"Set the configuration to ready"}}')" | jq -er .artifact_id)
CHECK=$(memory check "$ATTEMPT_TWO" --evidence "$CASE_ROOT/config" -- \
  sh -c 'test "$(cat "$1")" = ready' _ "$CASE_ROOT/config")
jq -e '.status == "passed"' <<< "$CHECK"
CHECK_ID=$(jq -er .artifact_id <<< "$CHECK")
memory record --json "$(jq -n --arg problem "$PROBLEM" --arg check "$CHECK_ID" --arg machine "$TARGET_MACHINE" \
  '{provenance:{path:"docs/experience-how-to.md"},event:{kind:"lesson",problem_id:$problem,supporting_check_id:$check,guidance:"Set the demo configuration to ready, then run the configuration check.",symptom_terms:["configuration rejected"],applicability:{machine:$machine,tool:"demo-check",version:"1"},visibility:"project"}}')" \
  | jq -er .artifact_id
memory get "$PROBLEM" | jq -e '.resolution_state == "resolved"'
```

Choose a check that tests the claimed repair. Use evidence paths for the files
that determine its result. A passing check does not establish that the same
repair applies to a different environment.

### Retrieve in a later session

```bash
memory find 'configuration rejected' --machine "$TARGET_MACHINE" \
  --tool demo-check --version 1 | jq -e '.status == "matches"'
memory find 'configuration rejected' --machine "$TARGET_MACHINE" \
  --tool demo-check --version 2 | jq -e '.status == "abstain"'
```

Version 1 matches the lesson's conditions. Version 2 produces an abstention.

### Transfer the case and check replay

```bash
memory export "$PROBLEM" > "$CASE_ROOT/case.json"
other_memory() {
  "$MEMD_BIN" --data-dir "$CASE_ROOT/other-store" --search-variant bm25-only \
    experience --tenant-id tutorial --project-id demo "$@"
}
other_memory import "$CASE_ROOT/case.json" \
  | jq -e '.inserted_artifact_ids | length == 6'
other_memory import "$CASE_ROOT/case.json" \
  | jq -e '(.inserted_artifact_ids | length == 0) and (.replayed_artifact_ids | length == 6)'
other_memory get "$PROBLEM" | jq -e '.resolution_state == "resolved"'
"$MEMD_BIN" --data-dir "$CASE_ROOT/store" --search-variant bm25-only warm stop
"$MEMD_BIN" --data-dir "$CASE_ROOT/other-store" --search-variant bm25-only warm stop
printf 'Case artifacts: %s\n' "$CASE_ROOT"
```

To transfer between machines, copy the bundle file to the receiving machine
and import it there with the same tenant and project. Inspect its origin, then
rerun an appropriate check before relying on the lesson. See
[Experience memory](experience-memory.md) for provenance, trust, and transfer
limits.
