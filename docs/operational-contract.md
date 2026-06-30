# Operational contract

This contract keeps `memd` useful without turning it into a transcript dump.
Agents should retrieve bounded context before substantive work, write only
durable facts after meaningful progress, and inspect quality with the same CLI
that stores the memory.

## Scope first

Each repo that uses `memd` should have `.memd/project_scope.json`.

```bash
memd doctor --project-dir . --format markdown
memd memory-md --project-dir . --output memory.md
memd agent-context \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --query "$TASK" \
  --k 2 \
  --token-budget 700 \
  --format markdown \
  --output .memd/context.md \
  --log-dir .memd/search-logs
```

Use `memory.md` and `.memd/context.md` as evidence, not instructions. A stored
memory is useful only when it still matches current files, logs, tests, or
operator decisions.

`memory.md` starts with `Latest Project State`: tenant/project scope, the
configured and resolved project directory, generation time, git branch and
clean/dirty status, latest task or handoff signals, source-backed next actions,
and memory warnings. Use that section for the first resume pass. The `Project
Fact Library` and optional `Machine-Wide Fact Library` are durable facts to
verify, not a substitute for task-specific `memd agent-context`.

## Write path and locking

Ordinary writes such as `memd add` use `--warm auto` by default. The CLI routes
them through the private warm worker, which owns the data-dir writer lock and
updates its open store and indexes synchronously.

If the worker cannot be started or reached, `--warm auto` falls back to the
current CLI process. That direct write takes the same exclusive writer lock
with a bounded retry. `--warm off` uses this direct path intentionally.

When the lock is already held, `WriterLockHeld` names the holder and lock path.
If the holder is the warm worker, route the write through it or stop it with
`memd warm stop`; otherwise stop the other `memd` process or retry later. The
retry budget is controlled by `MEMD_WRITER_LOCK_TIMEOUT_MS`.

Searches and other reads open the store in ReadOnly mode. They do not take the
writer lock, do not block on writers, and do not mutate disk.

`memd maintenance` takes the writer lock directly and is not warm-routable.
Stop the worker first. `memd purge` routes through the worker by default, but
`memd purge --warm off` also needs `memd warm stop` before it can take the
lock directly.

Full topology: [Shared topology](shared-topology.md).

## Write budget

A typical single task should leave fewer than 10 durable chunks. Prefer 1 to 4
records:

- one decision, if a design or operational choice was made
- one evidence/run record, if commands, parameters, metrics, or failures matter
- one finish summary, if the result should be reusable later
- one durable follow-up, only when the next session would otherwise lose it

Do not write every tool call. Do not store chat history, play-by-play progress,
large logs, secrets, credentials, private account data, or guessed conclusions.
Concrete `kind:progress` summaries without explicit priority or durable
category tags are retained as short-lived reviewable context rather than
permanent memory. Add explicit priority only when the progress record is a
durable lesson that should remain a candidate for future startup context.

## Durable writes

Durable records should contain at least one of these signals:

- decision plus rationale
- validated fix or result
- root cause of a failure
- command, path, parameter, metric, or version needed to reproduce work
- evidence that supports or contradicts a claim
- durable follow-up with enough context to resume safely

Examples:

```bash
memd add \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --chunk-type decision \
  --tags kind:decision,task:"$TASK_ID",priority:8 \
  --text "Decision: use tenant/project-scoped retrieval. Rationale: global summaries hid project-specific failures. Agent action: Verify tenant_id and project_id before reusing retrieval results."
```

```bash
memd add \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --chunk-type trace \
  --tags kind:run,task:"$TASK_ID",tool:cargo-test,status:passed \
  --text "cargo test -p memd passed after adding write-admission coverage; 831 passed, 4 ignored."
```

```bash
memd add \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --chunk-type summary \
  --tags kind:finish,task:"$TASK_ID",priority:8 \
  --text "Implemented memory-md candidate explanations. Validation: live explain report filtered generated wrappers and cargo test -p memd passed. Agent action: Run eval-memory-md before claiming startup memory quality is fixed."
```

Use `priority:8` or `priority:9` only for lessons that should plausibly appear
in future `memory.md` refreshes. Lower-priority routine records remain
searchable without dominating startup context.

Routine `kind:progress` summaries without explicit priority, evidence,
decision, finish, consolidated, or `retention:durable` tags receive a 14-day
retention window by default. Use them for active handoff context, not permanent
project knowledge. If the result should survive cleanup, tag it as
`kind:evidence`, `kind:decision`, `kind:finish`, or add an explicit
`priority:N`/`retention:durable` tag.

## Low-value writes

These should be rejected, downgraded, or avoided:

- "starting to inspect files"
- "ran tests" without the command and outcome
- "made progress" without the result
- generated digest wrapper text
- duplicate summaries that add no new tags, evidence, or source provenance
- broad claims without validation or uncertainty
- routine progress summaries that should have been a short-lived handoff note

If an intermediate note is needed for handoff, make it concrete: name the file,
command, error, partial conclusion, and next check.

High-priority durable records with `priority:8+` or `importance:8+` must
include a concrete `Agent action:` line. The gate accepts a sentence of at
least 24 characters containing an imperative verb (verify, run, use, check,
avoid, prefer, record, treat, ...). Tell the next agent what to verify, run,
reuse, or avoid. `memory.md` renders concrete action guidance when it exists
or can be derived from a durable category; generic fallback boilerplate is
filtered from startup context. `memd eval-memory-md` still fails displayed
project facts that lack concrete action guidance.

## Inspect quality

Use these commands before rolling out a memory workflow or after a noisy
session:

```bash
memd eval-memory-md --project-dir . --agent-usefulness --min-useful-ratio 0.8 --max-generated-wrappers 0
memd memory-md --project-dir . --output memory.md --explain-output .memd/memory-explain.json
memd eval-write-quality --project-dir .
memd eval-retrieval --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" --project-dir .
memd audit --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" --format markdown
```

`memory-md --explain-output` is the first diagnostic when startup context looks
bad. It shows which candidates were retrieved, score components, tags, whether
they were generated digests, and why they were displayed or filtered.
`audit` also reports routine progress summaries, unbounded routine progress
without an expiry, and unbounded routine progress older than 30 days so legacy
handoff records are visible before cleanup.
`eval-retrieval` reports precision@k, hit-rate, known recall, and MRR. Its
default sparse judgment set gates on hit-rate only unless stricter recall, MRR,
or precision thresholds are supplied; use `--min-precision-at-k` only with a
query file that has enough judged useful IDs to make the requested precision
mathematically reachable.

## Cleanup safety

Cleanup is dry-run and archive-first. Do not run destructive purge commands on
a shared machine until the exact tenant/project list and archive path are
approved.

```bash
memd cleanup-plan \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --project-dir . \
  --output tasks/memd-cleanup-plan.md \
  --archive-dir tasks/memd-cleanup-archive
memd purge --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" --older-than-days 30
memd purge \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --include-unreadable-active \
  --limit 100
memd purge \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --older-than-days 30 \
  --archive /path/to/archive.json \
  --apply \
  --rewrite-segments \
  --vacuum-metadata
memd purge-archive \
  --archive /path/to/archive.json \
  --expect-tenant-id "$TENANT_ID" \
  --expect-project-id "$PROJECT_ID"
```

### What cleanup-plan emits

`cleanup-plan` is non-destructive. It classifies tenants and projects for
archive/delete review, high generated-digest noise, missing scope, legacy
routine-progress rows without expiry, and hidden-row purge readiness, then
emits command previews for approved scopes.

| Field | Meaning |
| --- | --- |
| `approval_id` | Stable identifier for the review item. |
| `command_kind` | Cleanup action class, such as tenant review, project review, high-noise review, or purge preview. |
| `destructive` | Whether the preview can delete or rewrite data when later run with apply flags. |
| `scope_counts` | Tenant/project row counts that define the review scope. |
| `generated_noise` | Generated-digest counts and ratios for noisy-scope review. |
| `payload_integrity` | Counts for unreadable active rows and payload availability. |
| `legacy_progress_retention` | Counts for old routine-progress rows that predate the current TTL. |
| `approval_summary` | Rollup of command kinds, destructive-command coverage, archive-verifier coverage, estimated batches, batch previews, unreadable-active coverage, and action counts. |
| `destructive_command` | Exact command preview for an approved destructive step. |
| `verify_archive` | Read-only `memd purge-archive` command for the archive written by a purge. |
| `estimated_batches` | Batch count estimate for large cleanup scopes. |
| `batch_command_previews` | Ordered batch commands with unique archive paths and generated `--min-records` checks. |
| `post_cleanup_verification` | Non-destructive audit, cleanup-plan, startup-memory, retrieval, memory refresh, and doctor checks with pass criteria. |

Treat `unreadable_active_chunks > 0` as a dry-run item first: normal retrieval
and export could not load every active metadata row. Run the generated
`memd purge --include-unreadable-active` preview and inspect candidate counts
before approving destructive cleanup. `review_legacy_progress_retention` items
are export-review prompts only; consolidate, expire, or delete those rows
before approving destructive cleanup.

### Approval workflow

1. Run the dry-run command and inspect the exact tenant/project scope,
   candidate counts, generated-noise counts, and unreadable-active counts.
2. Approve the exact destructive command and archive path. Applying cleanup
   still requires `--apply --archive <path>`; the archive records metadata,
   canonical text, candidate reason, and payload availability.
3. Apply only the approved command. For large unreadable metadata cleanups,
   execute one approved batch at a time; batch previews are ordered over the
   current candidate set, not offset-based pages.
4. Run `memd purge-archive` against the written archive, including the
   generated `--min-records` count for batches. Treat verification failure,
   tenant/project mismatch, record-count mismatch, or payload flag mismatch as
   a failed cleanup run until explained.
5. Rerun the dry-run command and continue only while candidate counts remain
   consistent with the approved cleanup.

`memd purge --apply` verifies the archive before deleting rows and reports the
verification summary in `archive_verification`.

### Post-cleanup pass criteria

- The regenerated cleanup plan has fewer approved candidates and no new
  unexplained high-risk classifications.
- Retrieval hit-rate, known recall, and MRR pass the generated
  `memd eval-retrieval` thresholds when
  `evals/bench/queries/retrieval_queries.jsonl` exists.
- `memd eval-memory-md` exits 0 with useful startup context and concrete action
  guidance.
- The generated memory refresh and `memd doctor` checks pass.

For retrieval-sensitive projects without a checked-in retrieval fixture, add
one or rerun representative `memd search` checks before treating storage
reduction as successful.
