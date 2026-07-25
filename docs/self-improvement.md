# Self-improvement loop

`memd` keeps the working set of takeaways durable and useful across sessions
through five cooperating mechanisms. Each is independent and can be inspected
in isolation.

## 1. Heuristic priority at write time

All public write paths pass through the same preparation service. It
normalizes tags, applies write admission, assigns retention defaults, and
stamps an inferred `priority:N` tag (3..=7) from the chunk type, `kind:*`
tags, and validation or finish signals. Explicit `priority:` and
`importance:` tags still take precedence. CLI, structured-operation, batch,
OMF-import, supersession, and consolidation writes therefore share the same
policy.

## 2. LLM consolidation

`memd consolidate` builds a working region from chunks written or retrieved
since the last run, then asks the configured backend
(`MEMD_CONSOLIDATOR=claude|codex|auto|mock`) to rewrite them into
deduplicated `kind:consolidated` proposals with source lineage. Each proposal
must contain a concrete agent action, exact source evidence, and a confidence
value. Output that omits those fields, cites an unknown source, claims a
source twice, or resembles a prompt override is rejected as a whole.

Project-scoped runs use `supersedes:<csv>` and soft-tombstone their sources.
Tenant-wide runs use `derives_from:<csv>` and retain every project-scoped
source because the machine-wide lesson is not directly visible to a
project-scoped search.

Safety:

- The prompt frames untrusted chunk text as a JSON array so chunks cannot
  forge instructions.
- A single timeout reaps zombie subprocesses on expiry.
- Source claims are globally deduplicated: the same source can never
  be claimed twice.
- Each run is recorded in `consolidation_runs`, `consolidation_entries`, and
  `memory_lineage` before its payloads are written. The input hash makes an
  exact tenant, project, relation, and source set idempotent.
- The journal records the backend command, model, and CLI version. A
  permission-restricted audit artifact stores a size-capped raw-response
  prefix plus hashes that detect tampering.
- New summaries use the internal `Candidate` lifecycle state. Candidate text
  is excluded from search, direct retrieval payloads, agent context,
  `memory.md`, exports, and reports until promotion commits.
- The default command stops after validation. Inspect staged runs with
  `memd consolidate-review --list`, then use
  `memd consolidate-review <run_id> --accept` or `--reject`.
- Acceptance records durable promotion intent, then changes every candidate
  to `Final` and every same-project source to `Superseded` in one immediate
  SQLite transaction. A failed transaction leaves candidates hidden and
  sources active. `memd consolidate --promote` selects the same path for an
  explicitly automated run.
- Session start retries stale nonterminal work, but promotes only a run whose
  durable intent was already recorded. Runs updated within the last 30
  seconds are treated as in flight; malformed runs are terminally rejected so
  they cannot block recovery of later runs. Transient I/O and storage errors
  remain recoverable.

## 3. Retrieval exposure

Every CLI search appends one compatibility record per rendered chunk to the
rotating `.memd/data/retrieval_exposures.jsonl` log. Exposure is observability,
not evidence that a memory helped. It does not increase ranking priority.
`memory.md` uses only the absence of exposure as one signal when diagnosing an
old, apparently unused chunk. Reports prefer structured SQLite episode counts
and fall back to the JSONL log for older stores.

`memd eval-counterfactual` separately measures whether consolidated chunks
change ranks versus a same-pass filtered baseline.

## 4. Verified task outcomes

Search and agent context return a `retrieval_episode_id`. A search episode
stores a SHA-256 query hash, requester scope, expanded candidate pool, served
order, and `outcome-v1` shadow order. Agent context merges several separately
budgeted searches, so its combined episode stores only the final deduplicated
set and records ranking mode as `off`; it remains valid for explicit outcome
attribution but is not a served-versus-shadow comparison.

Retrieval episode tables never persist raw queries. `agent-context --log-dir`
is an explicit exception: its optional audit log contains the raw per-query
summaries shown in the command payload. Optional task IDs, thread IDs, and
outcome evidence references are also stored as plaintext linkage fields. Use
short, opaque, non-sensitive identifiers and never put prompts, credentials,
or other secrets in them.

After a task, an agent or task system can attach an explicit outcome:

```bash
memd outcome "$EPISODE_ID" \
  --tenant-id "$TENANT_ID" \
  --outcome passed \
  --verifier automated_test \
  --used "$CHUNK_ID" \
  --evidence "artifact:test-report"
```

Long documents may be stored as several chunks. `memory.add` returns the
primary ID in `chunk_id` and the complete ordered set in `stored_chunk_ids`.
Agents that retain write identities should store `stored_chunk_ids`; retrieval
and outcome attribution use the physical child IDs.
`memory.add_batch` returns one primary ID per logical input and does not expose
split-child IDs. Agents that need complete physical attribution for long
documents should use individual `memory.add` calls.

Only explicitly used chunks on a verified pass or acceptance receive positive
credit. Only explicitly harmful chunks on a verified correction or failure
receive negative credit. Rendered-but-unattributed chunks receive no credit,
and agent self-reports never affect ranking.

Report `verifier_error` when the verifier itself produced no verdict, because
it crashed, timed out, or returned something unparseable. `failed` asserts that
the task was verified and did not succeed, so using it for a broken verifier
penalises whatever happened to be retrieved and turns verifier flakiness into
permanent negative priors. A `verifier_error` credits nothing in either
direction; it is recorded so the failure stays visible and attributable. The
evidence requirement still applies to the verifier types that carry it, so an
automated test reporting a broken run should cite the run or log. Priors are scoped to the
requesting tenant and project, time-decayed, and bounded. Outcomes on an
aliased chunk therefore cannot alter the origin tenant's ranking prior.

The policy currently runs in shadow mode. Compare it with the served order
before considering activation:

```bash
memd eval-outcome-ranking \
  --tenant-id "$TENANT_ID" \
  --project-id "$PROJECT_ID" \
  --queries evals/bench/queries/outcome-ranking.jsonl \
  --report-json evals/bench/reports/outcome-ranking.run-id.json
```

This command reports recall, reciprocal rank, and harmful hits for both
policies. “Served” means the current production order, including exact-query
relevance feedback when that store already contains it. “Shadow” adds the
outcome prior to that order. It does not enable `serve` mode.

## 5. Cross-project transfer

Tenant-wide consolidation (`memd consolidate --tenant-id <t>`, no
`--project-id`) rewrites lessons that recur across a tenant's projects
into `kind:consolidated` chunks, which surface in every project's
`memory.md` through the capped `Machine-Wide Fact Library` (default
`--global-limit 2`; set `--global-limit 0` to disable). Project-scoped source
chunks remain active and searchable. The tenant-wide lesson records
`derives_from:<csv>` lineage without replacing those sources.

## Session-start hook

The session-start hook ties everything together:

```bash
memd session-start --project-dir "$CLAUDE_PROJECT_DIR"
```

It recovers stale consolidation runs, refreshes `memory.md` synchronously,
then stages a background consolidation when **≥ 10 dirty chunks** have
accumulated. Background proposals remain hidden and can be found with
`memd consolidate-review --list`. The
[skill installer](agent-skill.md) wires this into Claude Code by default.
