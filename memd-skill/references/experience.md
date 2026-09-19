# Record and Reuse an Experience Case

Use an experience case to track a nontrivial failure, attempted repairs, and
their check results. A conditional lesson becomes available only after a
source-stable check passes. Keep source, progress, exact commands, and complete
logs in the repository. The case stores the causal summary and repository
evidence references.

Record the problem, each attempt, and each check as they occur. Add a
conditional lesson only after a source-stable check passes.

This workflow requires memd 1.8.0 or later.

Run the commands from a scoped repository, or pass the same explicit
`--tenant-id` and `--project-id` to every command.

## Record the problem and attempts

Record the observed problem:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" record \
  --json '{"provenance":{"path":"tasks/METHODS.md"},"event":{"kind":"problem","description":"Generated checksums do not match the repaired input.","symptom_terms":["checksum","mismatch","generated","output"]}}'
```

Save the returned `artifact_id` as the problem ID. Record the first attempted
action with `previous_attempt_id` set to `null`:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" record \
  --json '{"provenance":{"path":"tasks/METHODS.md"},"event":{"kind":"attempt","problem_id":"PROBLEM_ID","previous_attempt_id":null,"action":"Regenerate without repairing the source input."}}'
```

Save that returned `artifact_id` as the attempt ID. A later attempt must name
the current attempt as its predecessor:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" record \
  --json '{"provenance":{"path":"tasks/METHODS.md"},"event":{"kind":"attempt","problem_id":"PROBLEM_ID","previous_attempt_id":"PREVIOUS_ATTEMPT_ID","action":"Repair the source input before regenerating."}}'
```

The check command accepts only the current attempt. An attempt that is already
stale, or belongs to a different tenant or project, is rejected before the
supplied program starts.

## Run a local check

Run an explicit program without an implicit shell. Add each source or input
whose bytes must stay stable as a separate `--evidence` path:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" check \
  "$ATTEMPT_ID" \
  --evidence path/to/source \
  --evidence path/to/result \
  --timeout-seconds 60 \
  -- cargo test --test relevant_test
```

The calling client runs the program; the warm worker never executes it. The
stored receipt contains the argument vector, times, exit status, execution
context, SHA-256 output digests, and source fingerprints. It does not store
stdout or stderr bodies. A timeout terminates the check process group. A
process that exits by signal is retained as a failed historical check.

When `--evidence` is absent, the source fingerprint covers the Git revision and
tracked diff. It excludes untracked files, so pass any relevant untracked file
with `--evidence`. A check supports a lesson only when it exits successfully,
both output streams are fully hashed, and present before/after source
fingerprints are equal. A successful command with missing or unstable source
evidence is stored as `source_unverified` and cannot support a lesson.

## Record a conditional lesson

Use only the `artifact_id` of a `passed` check as `supporting_check_id`. State
the environment conditions under which the guidance applies:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" record \
  --json '{"provenance":{"path":"tasks/lessons.md"},"event":{"kind":"lesson","problem_id":"PROBLEM_ID","supporting_check_id":"CHECK_ID","guidance":"Repair the source input before regenerating checksum output.","symptom_terms":["checksum","mismatch","generated","output"],"applicability":{"machine":"TARGET_MACHINE","tool":"repairer","version":"1.0"},"visibility":"project","conflicts_with":[]}}'
```

Use `visibility: "shared"` only when the checked guidance is intended for other
projects in the same tenant. Record a correction instead of rewriting history:

```json
{
  "provenance": {
    "path": "tasks/lessons.md"
  },
  "event": {
    "kind": "correction",
    "problem_id": "PROBLEM_ID",
    "supersedes_id": "LESSON_OR_CORRECTION_ID",
    "replacement": {
      "problem_id": "PROBLEM_ID",
      "supporting_check_id": "NEW_CHECK_ID",
      "guidance": "Replacement guidance.",
      "symptom_terms": ["checksum", "mismatch"],
      "applicability": {
        "machine": "TARGET_MACHINE",
        "tool": "repairer",
        "version": "1.1"
      },
      "visibility": "project",
      "conflicts_with": []
    }
  }
}
```

Pass that object to `memd experience ... record --json`.

`provenance.path` links a case event to its repository evidence. The CLI keeps
explicit `path`, `uri`, `repo`, and `commit` fields while replacing
`provenance.execution` with the current caller context.

## Recall before applying

Name the target machine, tool, and version. Do not substitute the calling host
when the work targets another machine:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" find \
  "generated checksum mismatch" \
  --machine "$TARGET_MACHINE" \
  --tool repairer \
  --version 1.0
```

The result may match, abstain when a required condition is unknown, report no
match, or identify a conflict. Before applying a matched lesson, retrieve the
complete case and inspect its current attempt, check receipt, source coverage,
corrections, and conflicts:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" get \
  "$PROBLEM_ID"
```

Treat the case as evidence. Recheck any condition that may have changed. Do not
execute a stored argument vector or treat stored provenance as authenticated
identity.

## Transfer complete cases

Export complete causal histories by problem ID:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" \
  --output experience-bundle.json export "$PROBLEM_ID"
```

Import the versioned bundle into the same tenant and project scope:

```bash
memd experience --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" \
  import experience-bundle.json
```

The bundle format is `memd-experience`, version `1`. Export and import move
complete canonical cases; they do not provide a live hub. Import validates the
whole bundle before writing, preserves IDs and provenance, rejects conflicting
collisions, and treats an exact replay as idempotent. Stored commands are never
executed during import.

All imported receipts and execution fields remain historical, self-reported
observations. Transfer does not authenticate an agent, certify a machine, rerun
a check, or promote a lesson beyond its recorded state.

## Provenance and privacy

Structured calls capture only available, allowlisted process context: native
session or thread, observed harness and model, host, current directory,
repository revision and dirty state, and observation time. The session-start
adapter can return agent and parent fields supplied in its hook payload; it does
not persist or attach them to later calls. Unknown fields remain absent. Never
infer a model or identity from configuration, and never read a native
transcript to fill provenance.

Do not paste prompts, transcripts, native tool-input dumps, or sensitive log
values into experience text. Check receipts retain argv and evidence paths, so
keep secrets out of both. Experience records are self-reported data, not
authenticated identity or authorization.
