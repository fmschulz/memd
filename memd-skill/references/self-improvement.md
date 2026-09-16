# Evidence-bound self-improvement

memd supports three separate learning loops. Keep them inspectable and gated:

1. **Content improvement:** stage deduplicated lessons with `memd consolidate`,
   inspect them with `memd consolidate-review --list`, and accept or reject the
   run. Candidate text stays hidden until an accepted run promotes atomically.
2. **Retrieval improvement:** capture a `retrieval_episode_id` from normal
   search or agent context, then attach a verified task outcome only after an
   external result exists.
3. **Experience reuse:** record a nontrivial problem, attempts, a client-run
   source-stable check, and a conditional lesson. At reuse, query the explicit
   target conditions and inspect the complete case before acting.

Experience check receipts and retrieval outcomes serve different purposes. A
check receipt establishes whether one recorded attempt passed against stable
source. An outcome evaluates a completed task after retrieved raw memory was
used. Do not substitute either record for the other.

Example outcome attribution:

```bash
memd outcome "$EPISODE_ID" \
  --outcome passed \
  --verifier automated_test \
  --used "$CHUNK_ID" \
  --evidence "artifact:test-report"
```

Pass multiple rendered IDs as comma-separated values to `--used` or
`--harmful`. Use `--harmful` only for chunks that caused a verified correction
or failure. When the verifier itself produced no verdict, because it crashed,
timed out, or returned something unparseable, report
`--outcome verifier_error` rather than `failed`. `failed` asserts the task was
verified and did not succeed, so reporting it for a broken verifier penalises
whatever happened to be retrieved. A `verifier_error` credits nothing either
way and keeps the broken run visible. Only `user`, `automated_test`, `external_tool`, and `task_system`
verifiers can affect the bounded, time-decayed prior; `agent_self_report` is
audit-only. Unattributed rendered chunks receive no credit. Episode storage
hashes raw queries, but `task_id`, `thread_id`, evidence references, and an
explicit `agent-context --log-dir` audit remain plaintext. Keep those values
short, opaque, and non-sensitive.

Outcome-aware ranking is shadow-only in v1.5. Evaluate it before considering
any serving change:

```bash
memd eval-outcome-ranking \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --queries evals/bench/queries/outcome-ranking.jsonl \
  --report-json evals/bench/reports/outcome-ranking.run-id.json
```

The report compares served and source-deduplicated shadow top-k lists for
explicit relevant and harmful chunk judgments. It does not activate the
shadow policy. A successful task is not, by itself, evidence that every
rendered memory helped; attribution must name only the chunks actually used.
Likewise, `observed_used` records that a lesson was reused. It is not a success
verdict and gives no ranking credit.
