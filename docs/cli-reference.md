# CLI reference

Everything `memd` does is a subcommand on the same binary. Commands fall into
three groups: **agent-facing** (the retrieval and write commands agents run every session),
**administrative** (init, maintenance, stats, exports), and **structured
operations** (`memd call <operation>` and `memd batch --jsonl`).

## Agent-facing commands

The global `--embedding-model` flag is optional. It accepts `all-minilm` (384-d),
`bge-small` (384-d CLS), `bge-base` (768-d CLS), and `qwen3` (unsupported by
the Candle backend; uses MiniLM). Without the flag, a store uses the model it
records, and a new store uses `bge-small`. A flag that differs from the store's
model is an error. Both BGE models normalise vectors and add the
retrieval instruction to queries only. The default search variant is
`hybrid-feature` for `bge-small` and `all-minilm`, and `dense-only` for `bge-base`.
An explicit `--search-variant` overrides this default. To switch the model on
an existing store, follow the steps in
[Embedding models](configuration.md#embedding-models). The switch rebuilds the
dense indexes from segments, even when both models have 384 dimensions.

For write-quality expectations and cleanup safety, see the
[Operational contract](operational-contract.md).

| Command | Purpose |
| --- | --- |
| `memd agent-context` | Prefetch bounded context to a file with audit logs. |
| `memd search` | Direct compact search. |
| `memd add` | Store summaries, traces, evidence, decisions. Without `--tenant-id` and with no scope in reach, the add uses the scope it would create and, once it succeeded, publishes `.memd/project_scope.json` (at the repository root, else in the directory). |
| `memd warm start \| status \| stop` | Manage the private local warm worker used by `--warm auto\|required`. |
| `memd batch --jsonl` | Run structured operation calls from JSONL in one loaded process; `--stream` keeps stdin/stdout open for benchmark clients. |
| `memd get`, `memd delete`, `memd stats` | Inspect and maintain chunks. |
| `memd export`, `memd export-markdown`, `memd export-omf`, `memd import-omf` | Portable local memory operations. |
| `memd init` | Write `.memd/` scope files and CLI guardrail blocks. |
| `memd doctor` | Diagnose binary discovery (incl. PATH-binary version skew), the resolved `--data-dir`, warm-worker reachability and worker-vs-CLI version skew, global agent rules, Claude Code SessionStart hook, and current project scope; `--strict` exits 2 when any check fails. |
| `memd memory-md` | Refresh project-root `memory.md` with ranked fact libraries and up to 3 recent lessons (`--lesson-limit`) for session-start use. |
| `memd eval-memory-md` | Gate startup-memory quality with `--min-useful-ratio` and `--max-generated-wrappers`; evaluate into `memory-eval.md`. |
| `memd eval-retrieval` | Gate retrieval quality with precision, hit-rate, recall, and MRR thresholds. |
| `memd audit` | Report tenant/project storage shape, generated-wrapper noise, alias groups, unreadable active rows, and routine progress summaries that still lack an expiry; `--strict` exits 2 when `unreadable_active_chunks > 0`. |
| `memd report` | Usefulness and self-diagnosis report from the usage ledger and store metadata; `--strict` exits 2 on any `[warn]` line. |
| `memd cleanup-plan` | Generate a non-destructive cleanup approval report with archive/purge command previews and post-cleanup pass criteria. Throwaway name checks match whole tokens separated by punctuation, so `retrieval` and `latest` do not match `eval` or `test`. |
| `memd purge` | Dry-run or archive-first cleanup of hidden rows; `--apply` verifies the archive before mutation, and `--include-unreadable-active` includes active rows with confirmed missing or corrupt payloads. Transient read errors abort the purge. |
| `memd purge-archive` | Read-only verification for `memd purge --archive` files: validates format/counts/payload flags, emits SHA-256, and can enforce expected tenant/project. |
| `memd consolidate` | Call the configured LLM (Claude Sonnet or Codex Spark, selected by `MEMD_CONSOLIDATOR`) and stage validated output under a journaled run ID. Candidate summaries remain hidden until review. `--promote` requests promotion in the same run; `--legacy-immediate` is a deprecated alias for one compatibility release. Exact source-set reruns reuse the existing run. |
| `memd consolidate-review` | List staged runs, inspect a proposal and its sources, accept it for atomic promotion, or reject it while leaving every source active. |
| `memd session-start` | Resolve an existing project or legacy scope, or the nearest scope of the enclosing git repository (reported as `scope_dir`), or auto-create a minimal `.memd/project_scope.json` when none exists and the project has stored content (a visible fact or an experience case): at the repository root for a subdirectory of a repository, otherwise in the directory itself. A project without content gets no scope and no `memory.md`; the result is `{"skipped": "no_stored_facts", "tenant_id", "project_id"}`. Recover consolidation runs idle for at least 30 seconds, refresh `memory.md` synchronously, then attempt background consolidation when at least 10 dirty chunks have accumulated. Recovery promotes only runs with durable promotion intent. A writer-lock failure is reported in `consolidation_recovery` without suppressing context refresh. See [startup scope and output behavior](agent-skill.md#install). |
| `memd eval-counterfactual` | Replay a JSONL benchmark file; write an overlap@k / rank-shift report under `evals/bench/reports/`. Monitors whether `kind:consolidated` lessons are load-bearing in retrieval. |
| `memd eval-outcome-ranking` | Compare the served order with the source-deduplicated `outcome-v1` shadow order against JSONL relevant/harmful judgments. Writes JSON and Markdown counterfactual reports without activating the policy. |
| `memd maintenance` | Disk hygiene: sweep orphan HNSW snapshots; `--aggressive` also force-merges the Tantivy sparse index and reports before/after segment counts. |

`memd add` returns the primary `chunk_id` and `stored_chunk_ids`, an ordered
list of every physical chunk created by document splitting.

- `memory-md --explain-output <path>` writes a JSON candidate audit with scan
  source, score components, tags, display/filter decisions, scope and memory
  health state, and agent-usefulness metrics. Its `scan` object counts, for the
  project and machine-wide sections, the visible, read and unreadable rows, the
  excluded experience projections and generated digests, the rows excluded by status, and whether a
  cap truncated the scan. Its `lessons` array lists each lesson candidate with
  the problem ID, lesson artifact ID, event time and skip reason (`superseded`,
  `conflict`, `over_limit`, `too_large`, `unresolvable`; null when shown).
- `memory-md --lesson-limit <n>` (default 3, maximum 10, 0 turns the block off)
  sets the `Recent Lessons` block, which follows the project facts. It lists
  the newest (by creation time) current lessons of the active project (and its aliases), written as inert single-line text with fixed field lengths: a
  superseded lesson, a case with conflicting lessons, a case with more than
  200 stored artifacts and lessons of other
  projects are left out. It does not use `--project-limit` places and skips the
  repository-coverage and dedupe gates. The command result reports
  `project_lessons`. Recall lessons of other projects with
  `memd experience find`.
- `memory-md` renders a generation date and scope before the fact libraries.
  `Memory health` prints `facts: <P> visible in project <id>, <T> visible in
  tenant <tenant>` (`<P>` counts the facts the scan admitted, so it skips
  superseded, deleted and other non-visible rows, experience projections and
  generated digests, including continuation rows of long ones written before
  part links existed; `<T>` is a SQL row count that can include those old
  continuation rows) and
  warnings for scope drift, a truncated project scan (oldest facts cut first),
  or unreadable payloads. The `[warn]` self-diagnosis lines of `memd report`
  no longer appear. Task, handoff, and
  git state belong in repository files. Takeaways covered by indexed repository
  documents are suppressed using a token-overlap heuristic; lessons tagged
  `priority:8+` or `importance:8+` are exempt. Missing or unreadable repository files
  are skipped by that coverage check.
- Every visible row of the active project is a candidate. Machine-wide
  candidates are the newest 5,000 visible rows outside it. Experience-case
  projections are never candidates; use `memd experience find` for lessons.
- Project and machine-wide candidates are ranked, then deduplicated as one
  pool within the best 200 candidates per section, before display limits are
  applied. Exact IDs, consolidation
  lineage, and high-confidence topic matches appear in one section only, with
  the active project's section taking precedence.
- `eval-memory-md` writes to `memory-eval.md` by default and rejects an output
  that resolves to the project's `memory.md`. `--gold-file` evaluates local
  multi-project fixtures with required or forbidden text and fragment or
  unrelated-item limits when specified. Scope, duplicate, and degraded-memory
  metrics remain available in the explanation output.
- `eval-retrieval` gates with `--min-precision-at-k`,
  `--min-hit-rate-at-k`, `--min-known-recall-at-k`, and `--min-mrr`.
- `eval-outcome-ranking --queries <jsonl> --report-json <path>` records normal
  privacy-safe episodes but does not serve the shadow order. Each query row
  contains `id`, `query`, `relevant_chunk_ids`, and `harmful_chunk_ids`.
  Existing report paths are never overwritten. Its served baseline is the
  current production order, including any exact-query relevance feedback in
  the store; the shadow order adds outcome-attributed priors.
- A cold `search` or `agent-context` run opens the store read-only and
  records no episode and no exposure line: `retrieval_episode_id` is `null`.
  A search the warm worker serves records its episode as described below.
- `search` episodes retain the expanded candidate pool and counterfactual
  shadow ranks. Multi-query `agent-context` episodes retain only the final
  merged result set and use ranking mode `off`; they support later outcome
  attribution but are not valid counterfactual ranking records.
- `task_id`, `thread_id`, and outcome evidence references are plaintext
  linkage fields. Keep them opaque and non-sensitive. Retrieval episode tables
  store query hashes rather than raw queries, but an explicit
  `agent-context --log-dir` audit log includes the raw query summaries.

`purge --older-than-days` counts a deleted row's grace period from deletion.
A deleted row whose deletion time is unknown receives a full grace period at
its first writable open. An applied purge also removes expired retrieval
episodes and their items and outcome events in the selected tenant and project.
Episode expiry is normally 90 days after creation. A dry run retains these rows.

## Experience

The `experience` commands require memd 1.8.0 or later.
Shared options are `--tenant-id`, `--project-id`, `--output PATH`, and
`--warm auto|off|required`. Scope follows the normal
project configuration rules. See the [worked procedure](experience-how-to.md).

| Command | Input and behavior |
| --- | --- |
| `experience note FILE` | Records a solved problem from a case file (`-` reads stdin): a problem, an attempt for each `failed` item and for the `fix`, a check, and a lesson. `--emit` validates the file and prints the canonical JSON without opening a store or needing a scope. `--shared-id ID` stores `agentdash:ID` in provenance. `--compact` stores the full case but makes only the lesson searchable, in one row unless the lesson is long enough to be split (see `rows_written`). A call that stopped half way is finished by the same call again, not repeated as a second case. A repeat call returns the stored case (`replay`) only for the same ID, or, without an ID, the same problem, symptoms, fix, check, and `applies_when`. Prints `problem_id`, `attempt_ids`, `check_id`, `lesson_id`, `evidence`, `replay`, and `rows_written` (chunk rows the store wrote; 0 on a replay). The format is in [Experience memory](experience-memory.md#a-case-file-records-a-solved-problem-in-one-call). |
| `experience record --json JSON` | Accepts `event`, optional `observed_at_ms`, and optional `provenance` with evidence fields such as `path`, `uri`, `repo`, and `commit`. `--input FILE` reads the same object from a file. |
| `experience get PROBLEM_ID` | Returns the problem, events, current lessons, conflicts, and `resolution_state`. Reads a case from another project when it has a current shared lesson, and then returns only its shared lessons and their checks. Read-only. |
| `experience check ATTEMPT_ID -- PROGRAM ARGS` | Runs the explicit program locally and records a receipt for that attempt. Repeated `--evidence PATH` selects source files. `--timeout-seconds` defaults to 60 and accepts 1–3600. |
| `experience find QUERY` | Returns applicable current lessons or abstains. Each lesson carries `evidence` (`executed` or `observed`). Includes lessons that other projects share; options: `--machine`, `--tool`, `--version`, `--project-only` (exclude shared lessons from other projects), and `--limit` (default 5). Read-only. |
| `experience export PROBLEM_ID...` | Writes a `memd-experience` version 1 JSON bundle to stdout. Read-only. |
| `experience import FILE` | Validates and imports a bundle; returns inserted and replayed artifact IDs. Exact replay inserts nothing. |

Record event fields:

| `event.kind` | Fields |
| --- | --- |
| `problem` | `description`, `symptom_terms` |
| `attempt` | `problem_id`, `action`, optional `previous_attempt_id`; the preceding attempt is required after the first attempt |
| `lesson` | `problem_id`, `supporting_check_id`, `guidance`, `symptom_terms`, `applicability`, optional `visibility` and `conflicts_with` |
| `correction` | `problem_id`, `supersedes_id`, `replacement` (the lesson fields above) |

`applicability` contains optional exact-match `machine`, `tool`, and `version`
strings. `visibility` defaults to `project`; `shared` also permits recall by
other projects in the same tenant, and `experience find` includes such lessons
unless `--project-only` is given. A lesson also takes `evidence` (`executed`,
the default, or `observed`), which must match its supporting check, plus
optional `context` and `links`.

Check statuses are `passed`, `failed`, `timed_out`, and `source_unverified`.
The last means the program succeeded but source fingerprints were missing or
changed. Only `passed` supports a lesson. A successful memd exit means that
the receipt was stored; inspect `status` to determine the verifier's result.
Interrupted checks return an error when no complete receipt is available.

The corresponding structured operations are `experience.note`,
`experience.record`, `experience.record_check`, `experience.get`,
`experience.find`, `experience.export`, and `experience.import`. They accept tenant/project scope
in their arguments. `experience.record_check` accepts a caller-reported receipt
and does not execute a command. The CLI `experience check` is the execution
entry point. See [Trust boundary](trust-boundary.md#experience-checks).

### Caller execution context

Search and structured-operation requests capture available execution context
before warm routing. The allowlisted fields include native session and thread
IDs, harness, reported model, host, directory, and Git state.
Absent model or identity values remain absent. Explicit semantic task IDs
remain separate from native conversation IDs.

`session-start --native-hook codex|claude` accepts a bounded JSON hook payload
on stdin and returns its allowlisted execution context, including agent and
parent IDs when present. These hook-only IDs are not automatically attached
to later CLI writes. The adapter does not ingest
the transcript or create a machine-global current-session identity file.
Native hook configuration and trust are managed by the harness; the adapter's
presence does not establish that a hook is installed or active.

## Consolidation review

The default command creates a hidden, validated proposal and returns its
`run_id`, candidate IDs, consolidator command, model, and version:

```bash
memd consolidate --project-dir .
memd consolidate-review --list
memd consolidate-review <run_id>
memd consolidate-review <run_id> --accept
```

With only a run ID, the command reads candidate text, sources, lineage,
validation state and consolidator provenance. It does not accept or reject
the run. List and inspection open the store read-only, including while a
warm worker is active. Candidate text remains excluded from ordinary recall.
The run's `validated` state describes consolidation validation, not a check
that the proposed advice worked.

The scan watermark records the time selection began, with a one-millisecond
overlap. Writes made during the model call stay eligible for the next scan.

`--tenant-id` and `--project-id` restrict listing, inspection and decisions.
Without an explicit tenant, the command uses the repository's scope when one
exists. Without either flags or a scope file, it is a store-wide administrator
view. An explicit tenant without a project covers that tenant's projects.
List filters apply before `--limit`, whose default is 100 runs.

Accept and reject support `--warm auto|off|required`, with `auto` as the default.
This routes the decision through the warm worker when available. List and
inspection remain read-only regardless of `--warm`.

Use `--reject` instead of `--accept` to close the run without changing its
sources. Acceptance records durable promotion intent before the atomic
transaction. A restart can therefore finish an accepted run, but cannot
promote a proposal that was only staged. Project-scoped acceptance promotes
the candidates and marks same-project sources `Superseded`; tenant-wide
acceptance records `derives_from` lineage and leaves project sources active.

For an explicitly automated workflow, use:

```bash
memd consolidate --project-dir . --promote
```

The deprecated `--legacy-immediate` flag has the same promotion semantics and
prints a warning. It exists only for migration from the former default.

Each proposed entry must name a concrete agent action, cite exactly the source
IDs it supersedes or derives from, and provide a confidence in `[0, 1]`.
Malformed, conflicting, or prompt-like output is rejected before candidate
creation. The journal records the consolidator identity and points to a
permission-restricted, size-capped raw-response audit artifact inside the
store data directory. Candidate contents and audit artifacts are never part
of retrieval or export surfaces.

## Structured operations (`memd call`)

`memd call <operation> --json ...` exposes the historical operation surface
through the executable without starting a separate integration process. This is
the compatibility path for advanced scripts that need structured task,
artifact, context, code, or debug operations before every operation gets a
dedicated first-class subcommand.

When the current directory is inside a repository with
`.memd/project_scope.json`, `memd call` and `memd batch` add the scope's
`tenant_id` and `project_id` to requests that omit `tenant_id`. A request with
an explicit JSON `tenant_id` does not inherit the repository project; include
`project_id` in that request when project filtering is intended. Scope is
resolved before warm-worker routing. A malformed or unreadable scope file
fails closed for requests without a tenant. In batch mode,
`--continue-on-error` reports that failure on the affected line while
explicitly scoped lines continue.

### `memory.*` — raw searchable content

- `memory.add` — single chunk. The response contains the primary `chunk_id`
  and `stored_chunk_ids`, an ordered list of every stored split child with the
  primary ID first. Code chunks retain their text and source metadata for
  ordinary memory retrieval.
- `memory.add_batch` — many chunks in one call. Its `chunk_ids` array contains
  one primary ID per logical input, preserving input order. It does not expose
  split-child IDs; use individual `memory.add` calls when physical write
  identity is required for retrieval or outcome attribution.
- `memory.search` — hybrid retrieval with optional `mode`, `project_id`,
  compact/token-budgeted output, and event sibling expansion.
  A query term with a digit, `-` or `_` (`cve-2026-111497`, `job_2807`) also
  gets an exact lookup in the keyword index, for the first 8 such terms of a
  query. The term must match whole tokens: `873` does not match `18735`, and
  punctuation around an identifier (backticks, `#`, `@`, quotes) is ignored.
  When at most 8 visible chunks in the search scope hold the term, all of them
  get a +20 score boost; this rescue does not depend on `k`. When more chunks
  hold it, or the index read 1,000 sentences before the count was certain, the
  term is common (`read-only`) and gets no boost. Only chunk text is matched,
  not tags. Superseded, expired, deleted and candidate chunks do not count.
  Rows still waiting for keyword indexing (up to 200 per scope) are tested too
  and counted with the indexed ones; more waiting rows than that, or a waiting
  row that cannot be read, mean no term is boosted. A term that the sentence splitter cuts, such as `api?job_id=2807`
  as one term, cannot match: each sentence is its own index document, and a fix
  needs a chunk-level field. The keyword index is rebuilt once, from stored
  payloads, by the first writer that opens a store after a tokenizer change. A generic search never rebuilds
  digests; `mode` values such as `brief_project` or `find_failures` still
  refresh theirs.
- `memory.get`, `memory.delete`, `memory.stats`, `memory.health`.
- `memory.compact` — explicit digest refresh; supports `digest_modes` and `force_digest_rebuild`.
- `memory.dream` — dry-run-first retention and compaction planning; safely
  retires duplicate digest projections on apply and writes a traceable report.
  Exact duplicate raw chunks are reported by health but not auto-retired by
  the safe profile.

Conversation-style chunks can carry caller-supplied `event:<id>` tags along
with `entry:factual` or `entry:relational`. Passing
`expand_event_siblings: true` to `memory.search` keeps the ranked result list
unchanged and attaches bounded same-tenant/same-project chunks that share the
matched event tag under each result's `expanded_siblings` field.

### `task.*` — structured work

- `task.start` (only `goal` required), `task.progress`, `task.finish`.
- `task.run_start` / `task.run_finish` for substantive runs.
- `task.add_evidence` for concrete evidence against a task.
- `task.get`, `task.search`, `task.resume`.

`task.resume` looks up the task ID directly without a recent-task limit and
checks tenant scope. Project briefs classify a task with `finished_at_ms`
as completed, including failed or cancelled tasks.

### `artifact.*` — focused collaboration tools

Artifact collaboration uses four specialized tools with tight schemas:

- `artifact.review` — request a review; attach summary and requested action.
- `artifact.revision` — supersede a prior artifact with `superseded_by` lineage.
- `artifact.decision` — choose between alternatives with `why_chosen`.
- `artifact.verification` — distinct-writer countersignature; with a different
  `agent_id` than the parent's and `supports_claim = true` it promotes the
  underlying claim to `VerifiedRecord` trust. The IDs are caller supplied;
  this records claimed review independence, not authenticated identity.

Inspection and retrieval:

- `artifact.get`, `artifact.search`, `artifact.list_thread`.
- `artifact.find_related` (retrieval helper; former `artifact.verify` alias
  is deprecated but still works).
- `artifact.find_failures`, `artifact.find_decisions`, `artifact.find_evidence`,
  `artifact.find_highlights`.

Claim support requires at least one whole-word match from the claim's
non-stopword terms of four or more characters. Recency, a positive marker,
or unrelated validation text alone does not establish support.

`artifact.create` remains available for backwards compatibility with a
deprecation warning. Digest artifacts are system-generated and cannot be
forged through `artifact.create`.

`artifact.search` defaults to the full legacy response. Passing `compact: true`
adds `budget_info`; `include_artifact: false` and `include_matched_text: false`
return only identifiers, summaries, ranking, and trust/grounding metadata so a
caller can fetch selected records with `artifact.get`.

### `context.*` — summary-first retrieval

`context.brief_project`, `context.find_relevant_context`,
`context.get_hot_context`, `context.get_files_for_subsystem`,
`context.list_subsystems`, `context.suggest_agent`.

`context.find_relevant_context` can prepend hot-context chunks when
`include_hot` is true. That legacy hot pre-scan is bounded by a short
wall-clock budget so large tenants still fall through to normal retrieval
instead of blocking the whole lookup on a full payload scan.
Hot chunks use the same visibility filter as search; superseded, expired
and history-tier content stays hidden.

## Warm and batch execution

For sustained local use, the warm worker keeps the store and indexes hot
across CLI calls. Warm-routable write commands route through the worker by
default, so the worker is the normal single-writer path:

```bash
memd warm start
memd agent-context --warm required --tenant-id quickstart --query "..."
memd warm stop
```

Flags:

- `--warm auto` (default) — use the local worker, starting it if needed; fall
  back to the current CLI process if startup or connection fails.
- `--warm off` — always run in the current CLI process.
- `--warm required` — require a local worker; fail if it cannot be started or
  reached, and hard-error on cold-only variants.

Routable commands: see [Shared topology](shared-topology.md).

`warm status` and `warm stop` report an absent or refused socket as stopped.
Timeouts, protocol errors and permission failures return errors. Unix PID
liveness uses signal-zero checks, including on macOS. Worker caps also count
live PIDs on macOS; Linux additionally checks the process name.

For scripts that need many structured operations in one loaded process:

```bash
memd batch --jsonl requests.jsonl
memd batch --jsonl - --stream
```

Each JSONL line should contain `{"tool":"memory.search","arguments":{...}}`;
the command emits one JSON result row per input line.

See [Quick start](quickstart.md) for end-to-end examples and
[Configuration](configuration.md) for the environment variables that change
defaults.
