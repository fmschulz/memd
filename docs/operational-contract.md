# Operational Contract

This contract keeps `memd` useful without turning it into a transcript dump.
Agents should retrieve bounded context before substantive work, write only
durable facts after meaningful progress, and inspect quality with the same CLI
that stores the memory.

## Scope First

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

## Write Budget

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

## Durable Writes

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

## Low-Value Writes

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
include a concrete `Agent action:` sentence. It should tell the next agent what
to do, check, prefer, avoid, verify, reuse, or resolve. `memory.md` renders
this action guidance for each displayed takeaway, and `memd eval-memory-md`
fails when displayed project takeaways lack concrete action guidance.

## Inspect Quality

Use these commands before rolling out a memory workflow or after a noisy
session:

```bash
memd eval-memory-md --project-dir . --min-useful-ratio 0.8 --max-generated-wrappers 0
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

## Cleanup Safety

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

`cleanup-plan` is non-destructive. It classifies tenants and projects for
archive/delete review, high generated-digest noise, missing scope, legacy
routine-progress rows without expiry, and hidden-row purge readiness, then
emits command previews for the approved list.
Each approval item has a stable `approval_id`, command kind, destructive flag,
scope counts, generated-noise counts and ratios, payload-integrity counts, and
legacy progress-retention counts. Treat
`unreadable_active_chunks > 0` as a dry-run item first: normal retrieval and
export could not load every active metadata row, so run the generated
`memd purge --include-unreadable-active` preview and inspect the candidate
counts before approving destructive cleanup for that scope. Applying that
cleanup still requires `--apply --archive <path>`; the archive records metadata,
canonical text, candidate reason, and whether the segment payload was available.
The `approval_summary` block rolls up command kinds, destructive-command
preview coverage, archive-verifier coverage, estimated batch count, and the
number of concrete batch command previews generated for those estimates. It
also reports the unreadable active rows covered by purge previews and includes
action rollups so reviewers can see how many items are tenant archive/delete
reviews, project archive/delete reviews, high-noise reviews, scope/noise
reviews, legacy progress-retention reviews, or unreadable-active purge reviews
before inspecting every item.
`review_legacy_progress_retention` items are export-review prompts only. They
mark active legacy progress summaries that predate the current default TTL and
need consolidation, expiry, or deletion decisions before any destructive
cleanup is considered.
When cleanup-plan can derive the next step, it also prints
`destructive_command`, `verify_archive`, and `estimated_batches` fields. These
are approval aids only: run the dry-run command first, approve the exact
destructive command and archive path, then run `memd purge-archive` against the
written archive before considering the batch complete. Large unreadable
metadata cleanups also include `batch_command_previews` with unique archive
paths for each estimated batch. These are an ordered sequence over the current
candidate set, not offset-based pages: execute only the approved batch, verify
its archive with the generated `--min-records` count, rerun the dry-run command,
and continue with the next batch only while candidate counts remain consistent
with the approved cleanup.
Every cleanup plan also includes a `post_cleanup_verification` block with
non-destructive commands and pass criteria. After any approved purge/archive
cleanup, rerun the generated audit, cleanup-plan, startup-memory evaluation,
memory refresh, and doctor commands. When the project directory has
`evals/bench/queries/retrieval_queries.jsonl`, the block also includes a fixed
`memd eval-retrieval` gate. Treat cleanup as incomplete unless those checks
show reduced approved candidates without new unexplained high-risk
classifications, retrieval hit-rate/known-recall/MRR pass the generated
thresholds, `memd eval-memory-md` exits successfully with useful startup
context that includes concrete action guidance, and host/project wiring remains
valid.
`memd purge --apply` verifies the archive before deleting rows and reports the
verification summary in `archive_verification`. You can also run
`memd purge-archive` against the exact archive path. Treat a verification
failure, tenant/project mismatch, record-count mismatch, or payload flag
mismatch as a failed cleanup run until explained.

For retrieval-sensitive projects without a checked-in retrieval fixture, add
one or rerun representative `memd search` checks before treating storage
reduction as successful.
