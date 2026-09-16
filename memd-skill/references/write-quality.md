# Write Quality Contract

Most repository tasks need no memory write. Keep their decisions, test results,
and finish summaries in repository files.

Use a raw memory for a concise operational fact that no readable repository
file can answer. Record an experience problem and its attempts as observed.
Add a conditional lesson only after a source-stable check passes. Keep the
source, progress, exact commands, and complete logs in the repository; the case
stores the causal summary and links to that evidence.

Concrete `kind:progress` summaries without explicit priority or durable
category tags are retained as short-lived reviewable context rather than
permanent memory. Add explicit priority only when the progress record is a
durable lesson that should remain a candidate for future startup context.

For an operational fact unavailable in repository files, record:

- decision plus rationale
- validated fix or result
- root cause of a failure
- a machine-specific command, path, parameter, or version
- evidence that supports or contradicts a claim
- enough scope and freshness information to verify the fact again

For high-priority records with `priority:8+` or `importance:8+`, include a
concrete `Agent action:` sentence; the write-quality gate requires it. The
sentence should tell the next agent what to do, check, prefer, avoid, verify,
reuse, or resolve. Avoid vague labels such as "benchmark state" unless they are
followed by the action rule and evidence that make them useful.

Avoid transcript-like memory:

- no full chat logs or play-by-play tool transcripts
- no "starting to inspect files" or "made progress" notes without outcomes
- no broad claims without validation or uncertainty
- no secrets, credentials, private account data, or sensitive log values
- no duplicate summaries unless they add new evidence, tags, or provenance

Use `priority:8` or `priority:9` only for lessons that should plausibly appear
in future `memory.md` refreshes. If startup context looks noisy or displayed
facts are stale or duplicate repository content, inspect the selection with:

```bash
memd eval-memory-md --project-dir . --agent-usefulness --min-useful-ratio 0.8 --max-generated-wrappers 0
memd memory-md --project-dir . --output memory.md --explain-output .memd/memory-explain.json
memd audit --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID" --format markdown
memd report --strict
```

`audit` and `cleanup-plan` report routine progress summaries that still lack an
expiry, including the subset older than 30 days. Treat those as legacy handoff
records that need consolidation, expiry, or deletion review; the generated
`review_legacy_progress_retention` cleanup-plan item is non-destructive and
exports the scope for inspection.
