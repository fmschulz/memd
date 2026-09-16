# Session-Start memory.md

Read the existing project-root `memory.md`. Refresh it when its date, scope,
or contents are stale, or when checking the startup integration:

```bash
memd memory-md \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --project-dir . \
  --output memory.md
```

If `.memd/project_scope.json` exists and contains the right scope, this shorter
form is preferred:

```bash
memd memory-md --project-dir . --output memory.md
```

The file shows its generation date and scope, memory-health warnings, and
ranked facts with chunk IDs, scores, and tags. Defaults are at most 10 project
facts and 2 machine-wide facts (`--global-limit 0` disables the latter).
Selection scans stored candidates and suppresses ephemeral progress, generated
wrappers, and facts covered by indexed repository documents. The coverage
heuristic can miss duplicates; the agent still decides where a fact belongs.

Explicit priority, durable categories, freshness, and verified outcomes affect
ranking. Retrieval exposure alone is not proof of usefulness. Source inspection,
task state, and next actions belong in repository files, not this digest.

For `priority:8+` or `importance:8+` writes, include a concrete `Agent action:`
sentence. Without one, the write is admitted at priority 7 with a warning.
The digest displays a bounded text summary; use `memd get` for the full record.

Output writes follow existing or dangling symlinks; the target directory must
exist. Existing Unix mode bits are preserved.
New files use mode `0600`. Set the target file's permissions after the first
write if other users need access. Replacement requires write access to an
existing target and its directory. It changes the inode and does not preserve
ownership, access-control lists, extended attributes, or hard-link identity.
Readers see a complete old or new file, but replacement does not guarantee
recovery after a system crash.

### Automatic session-start

The bundled [installer](../install_memd_enforcement.sh) adds a Claude Code
`SessionStart` hook. Codex users can copy the
[hook example](../examples/codex_session_start_hook.json). The hook runs:

```bash
memd session-start --project-dir "${CLAUDE_PROJECT_DIR:-.}" 2>/dev/null || true
```

This recovers stale journaled consolidation runs and refreshes `memory.md`
synchronously. With at least 10 dirty chunks since the last consolidation,
it attempts a detached `memd consolidate` in the background. Missing backends
or scope contention are reported in `consolidation_skipped`.
Recovery skips runs updated within the last 30 seconds and promotes only a
run whose durable promotion intent was recorded before the interruption.

The Codex hook example uses `--native-hook codex`. This adapter reads at most
64 KiB from the hook JSON on standard input.
When `--project-dir` is the default `.`, it uses the payload `cwd`; an explicit
non-default path takes precedence. The result returns only allowlisted session,
thread, agent, parent, model, harness, and working-directory context. It does
not retain prompts, transcripts, or raw tool input, persist a current-session
file, or attach that context to later commands. Missing identity and model
fields remain absent.

`session-start` reads `.memd/project_scope.json` first, then a valid legacy
`.memd/config.json` scope when the project scope is absent. If neither provides
a scope, it creates `.memd/project_scope.json` using `$MEMD_DEFAULT_TENANT`
(then `$USER`, then `"default"`) as `tenant_id` and the repo basename as
`project_id`. Derived IDs use lowercase ASCII letters, digits, and underscores;
separator runs collapse, leading and trailing separators are removed, and IDs
are capped at 64 characters. Empty IDs fall back to `default` for the tenant
and `project` for the project.

Automatic scope creation leaves agent rules and tenant guardrails unchanged.
`MEMD_AUTO_SCOPE=0` disables creation; `.memd-skip` skips session startup even
with an existing scope. Invalid or unreadable scopes are preserved and reported
without refreshing `memory.md`. A valid legacy configuration with neither scope
field, such as a wiki-only configuration, allows automatic scope creation.
A legacy project without a tenant is invalid. Concurrent first starts can
briefly report an incomplete scope; retry after the first process exits.
Run `memd init` explicitly when you want the full guardrail suite.

### Write-time priority

`memd add` (and the MCP `memory.add` handler) automatically stamp a heuristic
`priority:N` tag (3..=7) based on `--chunk-type`, `kind:*` tags, and
validation/finish text signals when the caller does not pass one. Explicit
user tags take precedence. Reserve `priority:8`/`priority:9` for verified lessons
that should appear in future startup context.

### LLM consolidation

For overlapping operational facts already in the store, a manual consolidation
pass can propose a smaller set of lessons:

```bash
memd consolidate --project-dir .
```

The selector reads `MEMD_CONSOLIDATOR`: `claude` runs
`claude -p --model claude-haiku-4-5-20251001 --output-format json`,
`codex` runs `codex exec --model codex-5.3-spark --json --skip-git-repo-check
--sandbox read-only`, `auto` picks Codex when `$CODEX_*` is set and falls
back to `claude` on `PATH`. The whole spawn → stdin write → wait sequence
runs under one 60 s timeout that explicitly kills and reaps the child on
expiry. The region is sent to the model as a JSON array so untrusted chunk
text cannot forge prompt framing.

Each response is journaled under a `run_id` before Candidate payloads are
written. Every proposed lesson must include a concrete agent action, exact
source evidence, and confidence in `[0, 1]`. The journal records the backend
command, model, and CLI version; a permission-restricted, size-capped local
artifact preserves the raw response and integrity hashes for audit.

Candidate text is unavailable to search, `memory.get`, agent context,
`memory.md`, exports, and reports. The default command stops after validation:

```bash
memd consolidate-review --list
memd consolidate-review <run_id> --accept
```

Use `--reject` to close a staged run without changing its sources. Acceptance
records durable promotion intent, then one SQLite transaction promotes the
candidates to `Final` and, for project-scoped runs, changes every source to
`Superseded`. A failure before commit leaves sources active and recovery can
finish only an accepted run. Exact source-set reruns reuse the same active or
committed run. Workflows that require explicit automatic promotion can run
`memd consolidate --project-dir . --promote`. The deprecated
`--legacy-immediate` flag has the same behavior for one migration release.

Project-scoped source chunks are soft-tombstoned (lifecycle status
`Superseded`) — nothing is deleted; the raw records remain accessible via
`memd search --include-superseded`. Their consolidated chunks carry
`kind:consolidated, priority:N, supersedes:<csv>, consolidator:<name>` plus
the dominant inherited `ctx:*` tags. Tenant-wide runs instead use
`derives_from:<csv>` and keep project-scoped sources active.

Skipped without `--force` when fewer than 10 chunks have accumulated since
the previous run; `.memd/data/consolidate.state.json` tracks the watermark.
Background proposals from session start are discoverable with
`memd consolidate-review --list`.

For cross-project transfer, run a tenant-wide consolidation (explicit
`--tenant-id`, no `--project-id`): the consolidated lessons are written
without a `project_id` and surface in every project's `memory.md`
through the `Machine-Wide Fact Library`. Project sources stay searchable in
their original scope.

### Counterfactual retrieval eval

To measure how consolidated lessons affect retrieval, run:

```bash
memd eval-counterfactual \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --k 5
```

This replays `evals/bench/queries/counterfactual_queries.jsonl` (one JSON
object per line; `{"query": "...", "label": "..."}`) and writes a Markdown
report to `evals/bench/reports/counterfactual_<unix>.md` with overlap@k
loss and mean rank shift between the full retrieval pass and the same pass
with `kind:consolidated` rows filtered out. Higher overlap loss means the
consolidated lessons change more of the retrieved results.

This command runs on the cold path; stop the warm worker first (`memd warm stop`).
