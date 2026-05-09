# memd Multi-turn Token-savings Benchmark Design

## Purpose

This benchmark is designed to answer a narrower question than `memd-stress-v4`:

Does correct retrieval from prior experience reduce the total tokens required to
solve later tasks, after including the token cost of memd retrieval?

The existing stress benchmark proves that memd can recover hidden facts. This
benchmark should prove a different claim: both conditions can solve the target
task, but the with-memd condition solves it faster because it learns from prior
experience instead of rediscovering the same root cause.

Committed result artifacts are the `summary.md` and `summary.json` files under
each `results/<run_set>/` directory. Raw runs, diffs, tests, metrics, retrieval
logs, and copied worktrees are generated locally by the harness and can be
regenerated from the committed fixture and harness inputs.

## Pilot1 Result

`pilot1` ran one transfer fixture across both requested agents and both memory
conditions:

```bash
python3 evals/bench/memd-multiturn-token-savings/seed/seed_experiences.py
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh pilot1
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set pilot1 --agents codex,claude
```

Result artifacts:

- `results/pilot1/summary.md`
- `results/pilot1/summary.json`
- `results/pilot1/runs/`
- `results/pilot1/final/`
- `results/pilot1/diffs/`
- `results/pilot1/tests/`

Outcome:

| agent | tests with/without | retrieval correct | memd payload | net savings |
|---|---:|---:|---:|---:|
| Codex CLI | 1/1 | 1 | 1,759 | -16,214 |
| Claude Code CLI | 1/1 | 1 | 2,531 | -22,890 |

This first executable pilot validates the harness and shows correct retrieval,
but it does not show token savings. The fixture is too small: both no-memd
agents solved it directly, so memd added one search call and payload without
avoiding enough diagnosis work. The next pilot should use larger, more
ambiguous fixtures with repeated failed-attempt traps before treating
token-savings as a supported claim.

## Harder v2 Episode

`timezone_boundary_transfer_v2` is a second transfer fixture designed to address
the `pilot1` weakness. It keeps the same broad timezone-boundary lesson, but
puts the bug in a larger dispatch export pipeline:

- `time_math.py` contains the shared UTC-normalization and reminder arithmetic
  bug.
- `policy.py`, `audit.py`, and `formatting.py` are plausible distractions that
  should not be rewritten unless current evidence proves them causal.
- `schedule_builder.py` combines the helpers so failures surface as contract
  UTC shifts, export ordering mistakes, and reminder-boundary errors.
- `test_dispatch_scheduler.py` has six contract tests; the intentionally broken
  fixture currently fails with three assertion failures and one reminder
  exception.

The expected prior is `mt-timezone-boundary-v2`, seeded by
`seed/seed_experiences.py`. Its retrieval record names the false leads
explicitly: blackout policy, audit keys, technician sorting, formatting, cache,
and database ordering were not causal. The intended repair is to normalize
offset-bearing ISO timestamps once with `astimezone(timezone.utc)` and to use
`timedelta` for reminder offsets.

Run only the harder episode manually:

```bash
python3 evals/bench/memd-multiturn-token-savings/seed/seed_experiences.py
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  without timezone_boundary_transfer_v2 pilot2 codex
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  with timezone_boundary_transfer_v2 pilot2 codex
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set pilot2 --agents codex
```

`harness/run_pilot.sh pilot2` runs all configured episodes for both Codex CLI
and Claude Code CLI.

## Pilot2 v2 Result

`pilot2_v2` ran only the harder `timezone_boundary_transfer_v2` episode across
both requested agents and both memory conditions. The seed script was not rerun
for this measurement, to avoid adding more duplicate prior records to the
benchmark tenant.

```bash
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  without timezone_boundary_transfer_v2 pilot2_v2 codex
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  with timezone_boundary_transfer_v2 pilot2_v2 codex
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  without timezone_boundary_transfer_v2 pilot2_v2 claude
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  with timezone_boundary_transfer_v2 pilot2_v2 claude
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set pilot2_v2 --agents codex,claude
```

Result artifacts:

- `results/pilot2_v2/summary.md`
- `results/pilot2_v2/summary.json`
- `results/pilot2_v2/runs/`
- `results/pilot2_v2/final/`
- `results/pilot2_v2/diffs/`
- `results/pilot2_v2/tests/`

Outcome:

| agent | tests with/without | retrieval correct | without tokens | with total incl. memd | net savings |
|---|---:|---:|---:|---:|---:|
| Codex CLI | 1/1 | 1 | 33,570 | 30,646 | +2,924 |
| Claude Code CLI | 1/1 | 1 | 121,844 | 171,349 | -49,505 |

This harder fixture produced the first positive net token-savings result for
Codex CLI: retrieval saved `5,723` provider tokens and remained positive
after the `2,799` token memd payload. Claude Code still used more total tokens
with memd despite correct retrieval. The result supports continuing with
harder fixtures, but it is not yet a general claim across agents.

## Suite5 Expansion

The benchmark now has four additional transfer episodes so the harder run can
check retrieval across five distinct bug families rather than one timezone
case:

| episode | project_id | expected prior | intended repair |
|---|---|---|---|
| `pagination_cursor_transfer` | `pagination_cursor` | `mt-pagination-cursor-v1` | Move cursor advancement after the page write succeeds; leave the cursor unchanged on transient write failure. |
| `cache_key_scope_transfer` | `cache_key_scope` | `mt-cache-key-scope-v1` | Include `tenant_id`, `project_id`, and `flag_name` in the cache key. |
| `schema_defaults_transfer` | `schema_defaults` | `mt-schema-defaults-v1` | Backfill existing rows before enforcing the new required `tier` column. |
| `stream_backpressure_transfer` | `stream_backpressure` | `mt-stream-backpressure-v1` | Drain pending chunks before final flush and on backpressure. |

Each added fixture has four unit tests, plausible distractor modules, and a
seeded prior that records false leads. This creates a five-fixture transfer
suite when combined with `timezone_boundary_transfer_v2`.

Run the five harder transfer episodes after seeding:

```bash
python3 evals/bench/memd-multiturn-token-savings/seed/seed_experiences.py
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh \
  suite5 \
  timezone_boundary_transfer_v2,pagination_cursor_transfer,cache_key_scope_transfer,schema_defaults_transfer,stream_backpressure_transfer \
  codex,claude
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set suite5 --agents codex,claude
```

When the optional episode list is omitted, `run_pilot.sh` runs all episodes in
`episodes.json`, including the small `timezone_boundary_transfer` pilot.

## Suite5 Result

`suite5` ran the five harder transfer episodes across both requested agents and
both memory conditions:

```bash
python3 evals/bench/memd-multiturn-token-savings/seed/seed_experiences.py
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh \
  suite5 \
  timezone_boundary_transfer_v2,pagination_cursor_transfer,cache_key_scope_transfer,schema_defaults_transfer,stream_backpressure_transfer \
  codex,claude
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set suite5 --agents codex,claude
```

Result artifacts:

- `results/suite5/summary.md`
- `results/suite5/summary.json`
- `results/suite5/runs/`
- `results/suite5/final/`
- `results/suite5/diffs/`
- `results/suite5/tests/`
- `results/suite5/metrics/`

All 20 cells exited with `cli_rc=0`, passed their target tests, and all 10
with-memd cells retrieved the expected prior experience.

Aggregate outcome:

| agent | solved cells | retrieval correct | positive net pairs | without tokens | with provider tokens | memd payload | with total incl. memd | net savings |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Codex CLI | 10/10 | 5/5 | 2/5 | 252,383 | 210,291 | 12,743 | 223,034 | +29,349 |
| Claude Code CLI | 10/10 | 5/5 | 1/5 | 745,192 | 947,290 | 12,684 | 959,974 | -214,782 |

Pair-level median net savings was `-4,248` tokens across the 10 with/without
pairs. Codex was positive in aggregate because `schema_defaults_transfer`
saved `37,997` net tokens and `timezone_boundary_transfer_v2` saved `2,164`.
Claude was positive only on `cache_key_scope_transfer` (`+976`) and negative on
the other four episodes.

Interpretation: suite5 supports a narrower claim than the benchmark's target
claim. Correct memd retrieval can save tokens on some harder repair tasks and
was aggregate-positive for Codex CLI in this run, but it did not consistently
save tokens across episodes or across Claude Code CLI. The benchmark therefore
needs harder or more search-heavy tasks, more replicates, and additional
controls before making a broad cross-agent token-savings claim.

Token caveat: Codex uses its CLI footer, while Claude uses `modelUsage` totals.
These measurements are useful within each agent but are not billing-equivalent
across agents. The memd payload column is the estimated serialized MCP
request/response payload from `memory.metrics`.

### Claude Code Overhead Diagnosis

The suite5 Claude result should not be read as "memd returned 214,782 extra
tokens." The measured memd MCP payload was small:

| Claude Code aggregate | tokens |
|---|---:|
| no-memd provider/modelUsage total | 745,192 |
| with-memd provider/modelUsage total | 947,290 |
| extra provider/modelUsage tokens before memd payload | 202,098 |
| measured memd MCP payload | 12,684 |
| net extra with-memd tokens including payload | 214,782 |

The measured memd payload accounts for only about 5.9% of Claude's negative net
delta. The rest is Claude Code provider-side accounting. In the suite5 stream
JSON, with-memd Claude runs exposed 79 tools at initialization versus 24 tools
without memd; 55 of those were memd MCP tools. Even though the harness allowed
only `mcp__memd__memory_search`, the full memd MCP tool surface was visible in
the init event. Claude also used an extra `ToolSearch` step before the memd
call. The provider totals are dominated by cache accounting:

| Claude Code aggregate | no memd | with memd | with - no |
|---|---:|---:|---:|
| input tokens | 3,419 | 3,068 | -351 |
| output tokens | 6,922 | 6,259 | -663 |
| cache-creation input tokens | 63,587 | 82,555 | +18,968 |
| cache-read input tokens | 671,264 | 855,408 | +184,144 |

Interpretation: in this run, Claude Code's overhead came mainly from the larger
tool/context surface being cached and reread across turns, plus the extra
tool-selection/retrieval turn. It was not primarily caused by the serialized
memd search response, which averaged about 2.5k MCP payload tokens per
with-memd Claude cell.

This suggests a concrete follow-up control: compare full memd MCP against a
thin retrieval interface for Claude Code. The thin condition could expose only
one `memory.search` tool, or use a small direct HTTP/CLI wrapper that returns a
bounded compact result. Direct access is not automatically better, because the
agent still has to read the returned memory text and direct shell output loses
some MCP audit structure. It is likely worth testing because it can avoid
injecting the full 55-tool memd schema into Claude Code's tool context.

## Suite5 Interface Result

`suite5_interface` implemented the follow-up control from
`CLI_THIN_MCP_COMPARISON_PLAN.md` and reran the five hard transfer episodes
across Codex CLI and Claude Code with four interface conditions:

```bash
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh \
  suite5_interface \
  timezone_boundary_transfer_v2,pagination_cursor_transfer,cache_key_scope_transfer,schema_defaults_transfer,stream_backpressure_transfer \
  codex,claude \
  without,full_mcp,thin_mcp,cli_search
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set suite5_interface --agents codex,claude \
  --conditions without,full_mcp,thin_mcp,cli_search
```

Result artifacts:

- `results/suite5_interface/summary.md`
- `results/suite5_interface/summary.json`
- `results/suite5_interface/runs/`
- `results/suite5_interface/final/`
- `results/suite5_interface/diffs/`
- `results/suite5_interface/tests/`
- `results/suite5_interface/retrieval/`
- `results/suite5_interface/metrics/`

All 40 cells exited with `cli_rc=0`, passed target tests, and produced
analyzable token/speed metadata. Retrieval correctness was 29/30 across memory
conditions. The miss was Codex `thin_mcp` on
`timezone_boundary_transfer_v2`, where search returned the older timezone v1
prior rather than the expected v2 prior.

Aggregate outcome:

| agent | condition | solved cells | retrieval correct | provider tokens | added MCP payload | CLI output estimate | total incl. retrieval | elapsed seconds | net vs without |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Codex CLI | `without` | 5/5 | 0/0 | 180,095 | 0 | 0 | 180,095 | 188 | baseline |
| Codex CLI | `full_mcp` | 5/5 | 5/5 | 178,669 | 12,807 | 0 | 191,476 | 197 | -11,381 |
| Codex CLI | `thin_mcp` | 5/5 | 4/5 | 218,422 | 8,950 | 10,410 | 227,372 | 185 | -47,277 |
| Codex CLI | `cli_search` | 5/5 | 5/5 | 134,977 | 0 | 14,901 | 134,977 | 621 | +45,118 |
| Claude Code | `without` | 5/5 | 0/0 | 681,312 | 0 | 0 | 681,312 | 134 | baseline |
| Claude Code | `full_mcp` | 5/5 | 5/5 | 929,068 | 13,873 | 0 | 942,941 | 130 | -261,629 |
| Claude Code | `thin_mcp` | 5/5 | 5/5 | 874,685 | 10,960 | 12,762 | 885,645 | 118 | -204,333 |
| Claude Code | `cli_search` | 5/5 | 5/5 | 789,458 | 0 | 14,919 | 789,458 | 122 | -108,146 |

Interpretation:

- Thin MCP reduced Claude's visible memd tool surface from 55 tools to 1 tool,
  and reduced Claude's negative net token delta by 57,296 tokens relative to
  full MCP, but it did not make Claude net-positive on this suite.
- CLI search was the best token condition for both agents. It made Codex
  aggregate-positive by 45,118 tokens and cut Claude's negative delta by
  153,483 tokens relative to full MCP.
- CLI search was much slower for Codex: 621 seconds total versus 188 seconds
  without memd, 197 seconds for full MCP, and 185 seconds for thin MCP.
- For Claude, interface conditions were slightly faster than no-memd in this
  run: 130 seconds for full MCP, 118 for thin MCP, and 122 for CLI search,
  versus 134 seconds without memd.
- The recommended default is not full MCP for Claude retrieval-only workflows.
  If auditability and typed calls matter, thin MCP is the better MCP shape. If
  token use matters most and shell access is acceptable, CLI search is the best
  tested retrieval interface, but Codex speed tradeoffs need attention.

## CLI-only Prefetch Orchestration

The next speed control is `cli_prefetch`. It keeps memd out of the agent tool
surface entirely and avoids asking the agent to run a retrieval command during
the solve:

1. The benchmark controller runs native `memd agent-context` before launching
   Codex or Claude.
2. For speed, the controller can call the long-lived local memd daemon through
   `memd agent-context --url ...`; no MCP server or MCP tool schema is exposed
   to the agent.
3. The controller writes `.bench/memd-context.md` plus audit JSON logs under
   `.bench/memd-search-logs/`.
4. The agent receives the bounded context in the initial prompt and patches the
   workspace normally.

Run it side by side with the previous conditions:

```bash
cargo build -p memd
python3 evals/bench/memd-multiturn-token-savings/seed/seed_experiences.py
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh \
  suite5_cli_prefetch_full \
  timezone_boundary_transfer_v2,pagination_cursor_transfer,cache_key_scope_transfer,schema_defaults_transfer,stream_backpressure_transfer \
  codex,claude \
  without,cli_search,cli_prefetch
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set suite5_cli_prefetch_full --agents codex,claude \
  --conditions without,cli_search,cli_prefetch
```

`cli_prefetch` is the recommended agent-facing CLI-only orchestration shape for
speed: retrieve once before the agent starts, dedupe and budget the context,
log the exact retrieval payload, and keep agent configuration free of MCP
servers. The tuned default profile is `k=2` with a `700` token budget per
query. Use `--url http://127.0.0.1:8787/mcp` when the shared daemon is running;
omit `--url` only when a fully direct one-shot store read is required.

Controller-side command shape:

```bash
memd agent-context \
  --tenant-id bench_mt_tokens \
  --project-id schema_defaults \
  --query "mt-schema-defaults-v1 repair rules" \
  --output .bench/memd-context.md \
  --log-dir .bench/memd-search-logs \
  --url http://127.0.0.1:8787/mcp
```

Full `suite5_cli_prefetch_full` result after iterative tuning:

| agent | condition | tests | retrieval | provider tokens | CLI output est. | elapsed total | median elapsed |
|---|---|---:|---:|---:|---:|---:|---:|
| Codex CLI | `without` | 5/5 | 0/5 | 175,871 | 0 | 171s | 34s |
| Codex CLI | `cli_search` | 5/5 | 5/5 | 138,806 | 14,843 | 565s | 113s |
| Codex CLI | `cli_prefetch` | 5/5 | 5/5 | 165,580 | 6,648 | 178s | 31s |
| Claude Code | `without` | 5/5 | 0/5 | 630,858 | 0 | 119s | 20s |
| Claude Code | `cli_search` | 5/5 | 5/5 | 882,319 | 14,861 | 151s | 26s |
| Claude Code | `cli_prefetch` | 5/5 | 5/5 | 675,413 | 6,679 | 141s | 24s |

All 30 cells passed tests. Both memory conditions retrieved the expected prior
in every scored cell. The tuning path matters: the initial `k=5` prefetch was
correct but too verbose, while `k=1` improved token totals but missed retrieval
markers on three cells. The final `k=2`, tag-free Markdown context restored
10/10 `cli_prefetch` retrieval correctness and kept the agent-facing context to
about 1.3 KiB per cell.

Interpretation: `cli_prefetch` is the best tested default when the agent should
not see MCP tools. It preserves correct retrieval, removes the slow in-agent
search turn, and cuts the visible retrieval output roughly in half relative to
`cli_search`. It is not a universal token win against no-memd; it is a fast,
auditable retrieval orchestration path for tasks where prior experience is
expected to matter.

## Assumptions

- "Multi-turn" means a sequence of fresh agent invocations in one benchmark
  episode. The chat transcript is not carried across turns; memd is the only
  cross-turn memory channel in the with-memd condition.
- The no-memd condition must still be able to solve the target from files,
  tests, and logs. Hidden-fact recall alone is not enough for this benchmark.
- Token savings must be computed after adding memd MCP request and response
  payload tokens to the with-memd condition.

## Episode Shape

Each episode has three turns:

| Turn | Name | What happens | Why it exists |
|---|---|---|---|
| 0 | Experience write | A prior task is solved and recorded as structured memd task/artifact history. The first implementation can seed this deterministically; a later variant can make an agent solve and write it live. | Creates the prior experience that later retrieval should use. |
| 1 | Transfer solve | A fresh agent solves a related task in a pristine fixture. The with-memd condition can retrieve the prior experience; the without-memd condition cannot. | Measures whether retrieval reduces diagnosis and repair tokens on a solvable task. |
| 2 | Distractor control | A fresh agent solves a similar-looking task where the prior fix is wrong or incomplete. | Measures retrieval precision and whether memd causes harmful over-transfer. |

Turn 1 and Turn 2 are the scored turns. Turn 0 is setup and is reported
separately so the benchmark does not hide memory-writing cost.

## Fixture Families

Start with synthetic but realistic repolets. Each family should include tests,
logs, and a small codebase so a no-memd agent can solve by normal debugging.

| Family | Prior experience | Transfer task | How retrieval should save tokens |
|---|---|---|---|
| `timezone_boundary` | A date parser failed around DST because naive local time was converted twice. Failed attempts ruled out database ordering. | A scheduler has a one-hour offset only on DST boundary tests. | The retrieved repair rule points directly to timezone normalization and avoids broad database/test-run exploration. |
| `pagination_cursor` | A sync worker repeated the last page because the cursor advanced only after a successful write. | A webhook backfill duplicates records after a retry. | Retrieval points to cursor advancement ordering and idempotency checks. |
| `sqlite_locking` | A flaky test came from sharing one SQLite connection across async tasks without WAL settings. | A queue worker intermittently fails with database locked. | Retrieval narrows the search to connection isolation and WAL timeout settings. |
| `schema_defaults` | A migration failed because new non-null columns lacked backfill defaults for old rows. | A report pipeline crashes only on pre-migration fixtures. | Retrieval points to migration backfill rather than formatter or API code. |
| `cache_key_scope` | Tenant-scoped cache entries used a project-only key and leaked results. | A permissions test returns another tenant's cached value. | Retrieval identifies key composition and the required regression test. |
| `stream_backpressure` | A stream reader dropped final chunks because the flush happened before awaiting the writer drain. | A log exporter truncates output under high concurrency. | Retrieval points to drain/flush ordering and avoids parser rewrites. |

Each family should have at least three transfer variants and one distractor
variant. A publishable run should have at least 30 scored transfer cells.

## Condition Matrix

| Condition | memd access | Prior experience | Purpose |
|---|---|---|---|
| `without_memd` | No MCP server | None | Main baseline. Agent solves from files/tests/logs. |
| `with_memd_seeded` | memd MCP only | Correct prior plus distractors | Main treatment. Measures retrieval usefulness and net token savings. |
| `with_memd_unseeded` | memd MCP only | Empty tenant | Tool/schema overhead control. |
| `with_memd_distractor_only` | memd MCP only | Similar but wrong prior | Harmful-retrieval control. |
| `oracle_note` | No MCP server | One concise relevant hint in prompt | Upper-bound control for how much a perfect memory could save. |

The first pilot can run `without_memd` and `with_memd_seeded` only. The other
conditions should be added before making a manuscript-level claim.

## Memory Record Schema

Seed each prior experience through the real task lifecycle when possible:
`task.start`, `task.progress`, `task.run_start`, `task.run_finish`,
`task.add_evidence`, and `task.finish`.

The searchable content should include this shape:

```json
{
  "experience_id": "timezone_boundary_a",
  "symptom_signature": "DST boundary test is exactly one hour off",
  "failed_attempts": [
    "Database ordering was checked and was not causal",
    "Formatting changes did not affect the failing assertion"
  ],
  "root_cause": "Naive local time was converted to UTC twice",
  "repair_rule": "Normalize to timezone-aware UTC at input boundary only",
  "verification_command": "pytest tests/test_scheduler_dst.py",
  "non_transferable_boundaries": [
    "Do not apply this fix to monotonic duration calculations"
  ]
}
```

The transfer prompt must not include this text. It should only say that prior
experience may exist and that useful memories should be searched before broad
exploration.

## Retrieval Correctness

A with-memd transfer cell gets retrieval credit only if:

- the expected prior artifact appears in a successful memd search response, or
  the final answer cites the expected `artifact_id` or `experience_id`;
- the answer uses the prior `root_cause` or `repair_rule` in the patch plan;
- no distractor artifact is cited as the primary reason for the fix;
- the target tests pass.

Turn 2 distractor cells pass only when the agent avoids applying the wrong prior
fix and either retrieves a cautionary boundary or solves from current evidence.

## Token Accounting

Record token cost per agent, condition, episode, turn, and attempt.

### Provider or CLI Tokens

Prefer provider/API usage fields. If a CLI is used, record the exact source and
mark it as non-billing-equivalent across agents.

Required columns:

- `provider_input_tokens`
- `provider_output_tokens`
- `provider_cache_read_tokens`
- `provider_cache_write_tokens`
- `provider_total_tokens`
- `token_source` such as `openai_api_usage`, `codex_footer`, or
  `claude_modelUsage`

### memd Payload Tokens

Use a dedicated memd daemon/data dir per sweep, or add run-id scoped metrics
before publishable runs. Until run-id metrics exist, take pre/post
`memory.metrics.token_usage` snapshots around each cell.

Required columns:

- `memd_request_payload_tokens`
- `memd_response_payload_tokens`
- `memd_total_payload_tokens`
- `memd_tools`
- `memd_search_count`
- `expected_artifact_retrieved`

Use the same estimator as existing v4 docs:
`ceil(serialized_mcp_payload_bytes / 4)`.

### Derived Metrics

For each scored transfer cell:

```text
without_total = without_provider_total_tokens
with_total = with_provider_total_tokens + with_memd_total_payload_tokens
solver_savings = without_provider_total_tokens - with_provider_total_tokens
net_savings = without_total - with_total
memd_tax = with_memd_total_payload_tokens
net_savings_pct = net_savings / without_total
tokens_per_success = with_total / solved_binary
```

Positive `net_savings` means memd saved tokens after paying for retrieval.
Report cumulative episode cost separately:

```text
episode_total_with_memory_write =
  turn0_memory_write_provider_tokens
  + turn0_memd_write_payload_tokens
  + scored_turn_with_total_tokens
```

This prevents the benchmark from hiding the amortized cost of creating memory.

## Task-solving Metrics

Report these alongside tokens:

- `tests_passed`
- `patch_applied`
- `agent_turns`
- `wall_time_seconds`
- `shell_calls`
- `files_read`
- `commands_before_first_patch`
- `failed_attempts_repeated_from_prior`
- `retrieval_correct`
- `distractor_avoided`

The strongest token-savings evidence is not just lower total tokens. It is lower
tokens with the same or better test pass rate, fewer repeated failed attempts,
and correct retrieval of the relevant prior experience.

## Harness Layout

Proposed directory structure:

```text
evals/bench/memd-multiturn-token-savings/
|-- README.md
|-- episodes.json
|-- fixtures/
|   `-- <family>/<variant>/
|-- seed/
|   `-- seed_experiences.py
|-- harness/
|   |-- run_cell.sh
|   |-- run_episode.sh
|   `-- no-memd.mcp.json
|-- tools/
|   |-- analyze.py
|   |-- metrics_snapshot.py
|   `-- score_patch.py
`-- results/
    `-- <run_set>/
```

`episodes.json` should be the source of truth for:

- fixture path
- target tests
- expected prior artifact id
- distractor artifact ids
- allowed files
- prompt text
- maximum attempts
- expected success criteria

## Isolation Requirements

- Run each scored turn in a fresh process and fresh worktree copy.
- Mount only the fixture directory and the agent config required for that
  condition.
- Do not mount `~/.memd`, seed scripts, gold files, or benchmark internals into
  the agent workspace.
- Expose only the memd HTTP endpoint in with-memd conditions.
- Use identical prompts and configs across conditions except for memory access.
- Archive raw transcripts, final answers, diffs, test logs, and metrics
  snapshots for every attempt.

## Prompt Shape

With memd:

```text
You are debugging this fixture. Prior task experiences may exist in memd.
Before broad exploration, search memd for relevant prior failures or repair
rules. Use only memories that match the current evidence. Then patch the repo
and run the target tests. In the final answer, include the experience id you
used, or say no useful prior experience was found.
```

Without memd:

```text
You are debugging this fixture. External memory is unavailable. Solve from the
current files, tests, and logs only. Patch the repo and run the target tests.
```

Do not put root-cause words in the prompt that would give away the intended
memory.

## First Pilot

Run a small pilot before building the full suite:

- 6 fixture families
- 1 transfer variant per family
- 1 distractor control per two families
- Codex only
- conditions: `without_memd`, `with_memd_seeded`
- max 2 attempts per cell

Pilot success criteria:

- with-memd and without-memd both solve at least 5/6 transfer cells;
- with-memd retrieves the expected prior in at least 5/6 transfer cells;
- median `net_savings` is positive after memd payload cost;
- no distractor control applies the wrong prior fix;
- every summary reports provider tokens, memd payload tokens, turns, tests, and
  retrieval correctness.

After the pilot passes, scale to at least 30 transfer cells, add the unseeded and
distractor-only controls, and repeat across at least two agents.

## Why This Improves On Existing Benchmarks

- `memd-cost-v3` and `memd-stress-v4` show that memd improves recall when facts
  are absent from the fixture.
- This benchmark requires the no-memd condition to solve the task too.
- The measured outcome is net token savings on task solving, not just recall.
- Distractor turns test whether retrieval is precise enough to help rather than
  overfit to superficially similar prior experiences.
