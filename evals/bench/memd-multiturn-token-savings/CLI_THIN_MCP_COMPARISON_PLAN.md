# CLI and Thin-MCP Interface Comparison Plan

## Purpose

Suite5 showed that correct memd retrieval worked, but Claude Code used many
more provider tokens with the full memd MCP server. The measured memd payload
was small; the overhead was mostly provider-side tool/context cache accounting
from exposing the full MCP tool surface.

This plan defines a dev-branch implementation that compares full MCP, thin MCP,
and direct CLI retrieval side by side on the same benchmark episodes.

## Proposed Branch

Use a dedicated branch:

```bash
git switch -c dev/memd-cli-thin-interface-benchmark
```

Do not create the branch from a dirty worktree unless the current untracked
benchmark artifacts are intentionally part of the branch. If starting from a
clean base, first preserve or commit the existing benchmark artifacts.

## Current Starting Point

- Existing full MCP condition: `run_one.sh` with `condition=with`.
- Existing no-memd condition: `run_one.sh` with `condition=without`.
- Existing Rust CLI entry point: `crates/memd/src/cli.rs`.
- Existing CLI search command:

```bash
memd --mode cli search --tenant-id <tenant> --query <query> --k <n>
```

Current CLI gaps for this benchmark:

- no `--project-id` filter
- no `--compact` output
- no `--token-budget`
- no retrieval `--mode`
- no MCP-parity trust/citation fields
- no direct benchmark condition that tells Codex/Claude to use CLI retrieval

## Goals

1. Compare four conditions on the same suite5 episodes:
   - `without`: no memd, current baseline
   - `full_mcp`: current full memd MCP tool surface
   - `thin_mcp`: one-tool retrieval-only MCP surface
   - `cli_search`: direct compact CLI retrieval

2. Keep task difficulty and scoring unchanged:
   - same five hard episodes
   - same seeded prior experiences
   - same target tests
   - same retrieval markers

3. Make token accounting explicit:
   - provider/CLI tokens from Codex and Claude
   - Claude `modelUsage` input/output/cache-create/cache-read split
   - memd payload tokens for MCP conditions
   - direct CLI output size for CLI conditions
   - net savings relative to `without`

4. Preserve auditability:
   - record every command/tool call transcript
   - store retrieval outputs per cell
   - keep final answer, diff, tests, metadata, and metrics
   - record benchmark summaries in memd task artifacts

5. Decide with evidence whether CLI or thin MCP should become the default
   agent-facing retrieval interface for Claude Code.

## Interface Conditions

### Condition A: `without`

Use the current no-memd harness. No external memory tools are configured.

### Condition B: `full_mcp`

Use the current with-memd harness. This exposes the full memd MCP server.

Expected purpose:

- baseline for current memd integration
- preserves current structured task/artifact surface
- likely high Claude tool-schema/cache overhead

### Condition C: `thin_mcp`

Expose only a single retrieval tool to the agent, preferably named
`memory.search` or `memd_search`.

Candidate implementation options:

- add a memd server mode or config flag that filters `tools/list`
- add a tiny MCP facade process that forwards only `memory.search` to the
  existing memd HTTP daemon

Recommended first implementation:

- tiny MCP facade, because it is isolated and does not risk changing the full
  MCP server behavior

Required behavior:

- one tool visible in Claude init events
- accepts `tenant_id`, `project_id`, `query`, `k`, `compact`,
  `token_budget`, and optional `mode`
- forwards to existing `memory.search`
- returns bounded compact JSON

### Condition D: `cli_search`

Use direct CLI retrieval from the copied fixture workspace.

Target command shape:

```bash
memd --mode cli search \
  --tenant-id bench_mt_tokens \
  --project-id schema_defaults \
  --query "schema default migration failed" \
  --k 5 \
  --compact \
  --token-budget 1200 \
  --format json
```

Required behavior:

- search must be project scoped
- output must be compact and bounded
- output must include enough text and identifiers to score retrieval correctness
- output must not require the agent to parse full MCP protocol envelopes

Open design choice:

- direct store access (`--mode cli`) is fastest but may diverge from the HTTP
  daemon's exact retrieval behavior
- an HTTP-backed CLI wrapper preserves daemon behavior but still avoids MCP tool
  schema injection

Recommended first implementation:

- add an HTTP-backed wrapper subcommand or script for benchmark use:

```bash
memd-search --tenant-id bench_mt_tokens --project-id schema_defaults \
  --query "schema default migration failed" --k 5 --token-budget 1200
```

This keeps the first CLI comparison focused on agent-facing interface overhead,
not on differences between direct-store and daemon retrieval.

## Implementation Plan

### Phase 1: Add Benchmark Condition Plumbing

Files:

- `evals/bench/memd-multiturn-token-savings/harness/run_one.sh`
- `evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh`
- `evals/bench/memd-multiturn-token-savings/analyze.py`

Tasks:

1. Rename current conditions internally:
   - `without` remains no memory
   - current `with` becomes alias for `full_mcp`
2. Add accepted condition names:
   - `without`
   - `full_mcp`
   - `thin_mcp`
   - `cli_search`
3. Preserve backward compatibility:
   - `with` should continue to map to `full_mcp`
4. Add condition-specific prompt blocks:
   - `full_mcp`: use `memory.search`
   - `thin_mcp`: use the one exposed retrieval tool
   - `cli_search`: use the exact `memd-search` command before broad exploration
5. Store condition in metadata and result filenames.

Validation:

```bash
bash -n evals/bench/memd-multiturn-token-savings/harness/run_one.sh
bash -n evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh
python3 -m py_compile evals/bench/memd-multiturn-token-savings/analyze.py
```

### Phase 2: Add CLI Retrieval Wrapper

Files:

- `scripts/memd-search` or `evals/bench/memd-multiturn-token-savings/tools/memd_search.py`
- optionally `crates/memd/src/cli.rs`

Tasks:

1. Implement a small read-only wrapper that calls the existing HTTP MCP daemon's
   `memory.search` tool.
2. Accept flags:
   - `--tenant-id`
   - `--project-id`
   - `--query`
   - `--k`
   - `--token-budget`
   - `--mode`
   - `--include-text`
3. Return compact JSON with:
   - `chunk_id`
   - `project_id`
   - `text` or `snippet`
   - `score`
   - `trust_tier`
   - `grounding_refs`
   - `artifact_id` when present
4. Make output deterministic enough for benchmark scoring.
5. Add a shell syntax or Python compile check depending on implementation.

Validation:

```bash
python3 evals/bench/memd-multiturn-token-savings/tools/memd_search.py \
  --tenant-id bench_mt_tokens \
  --project-id schema_defaults \
  --query "mt-schema-defaults-v1" \
  --k 3 \
  --token-budget 1200
```

Expected result:

- output includes `mt-schema-defaults-v1`
- JSON parses cleanly

### Phase 3: Add Thin MCP Facade

Files:

- `evals/bench/memd-multiturn-token-savings/tools/thin_mcp_search_server.py`
- `evals/bench/memd-multiturn-token-savings/harness/thin-memd.mcp.json`

Tasks:

1. Implement a minimal MCP server exposing one tool only.
2. Tool name: `memd_search` or `memory.search`.
3. Forward calls to the existing memd HTTP daemon.
4. Return compact bounded search results.
5. In Claude init events, verify only one thin retrieval tool is visible from
   this server.

Validation:

```bash
python3 -m py_compile \
  evals/bench/memd-multiturn-token-savings/tools/thin_mcp_search_server.py
```

Then run one Claude cell and inspect the init event tool count.

### Phase 4: Analyzer Support

Files:

- `evals/bench/memd-multiturn-token-savings/analyze.py`

Tasks:

1. Recognize all four conditions.
2. Pair every memory condition against `without`.
3. Add condition-level aggregate tables:
   - provider tokens
   - memd MCP payload tokens
   - CLI retrieval output bytes/tokens
   - Claude cache-read/cache-create tokens
   - tool count from init event
   - retrieval correctness
   - tests passed
4. Keep current summary fields for old runs.
5. Add `interface_condition` to rows and pairs.

Validation:

```bash
python3 evals/bench/memd-multiturn-token-savings/analyze.py \
  --run-set suite5_interface_probe --agents codex,claude
python3 -m json.tool \
  evals/bench/memd-multiturn-token-savings/results/suite5_interface_probe/summary.json
```

### Phase 5: Pilot Run

Run one episode first:

```bash
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  without schema_defaults_transfer suite5_interface_probe claude
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  full_mcp schema_defaults_transfer suite5_interface_probe claude
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  thin_mcp schema_defaults_transfer suite5_interface_probe claude
bash evals/bench/memd-multiturn-token-savings/harness/run_one.sh \
  cli_search schema_defaults_transfer suite5_interface_probe claude
```

Promotion criteria:

- all four cells pass target tests
- `full_mcp`, `thin_mcp`, and `cli_search` retrieve the expected prior
- `thin_mcp` and `cli_search` expose fewer tools / lower Claude cache tokens
  than `full_mcp`
- analyzer produces a clean comparison summary

### Phase 6: Full Side-by-Side Run

Run the five hard episodes for both agents:

```bash
bash evals/bench/memd-multiturn-token-savings/harness/run_pilot.sh \
  suite5_interface \
  timezone_boundary_transfer_v2,pagination_cursor_transfer,cache_key_scope_transfer,schema_defaults_transfer,stream_backpressure_transfer \
  codex,claude \
  without,full_mcp,thin_mcp,cli_search
```

If runtime is too high, run Claude first because suite5 showed the largest
interface overhead there.

## Success Metrics

Primary metrics:

- all cells pass tests
- retrieval correctness for each memory condition
- net tokens versus no-memd
- Claude cache-read and cache-creation deltas versus no-memd

Secondary metrics:

- elapsed seconds
- number of visible tools in Claude init event
- number of agent turns
- shell calls
- memd payload tokens
- CLI retrieval output token estimate
- final answer cites expected prior id

Decision rules:

- If `cli_search` saves tokens without losing retrieval correctness, add a
  documented CLI-first retrieval option for Claude Code.
- If `thin_mcp` performs similarly to CLI, prefer thin MCP because it preserves
  typed tool calls and auditability.
- If neither improves over full MCP, keep full MCP and focus on harder tasks or
  prompt/tool-selection changes.

## Security and Audit Requirements

For CLI retrieval:

- allow only read-only `memd-search` in the benchmark prompt
- no write commands in the CLI retrieval path
- output every retrieval command to transcript
- archive retrieval JSON per cell
- bound result count and token budget

For thin MCP:

- expose only the retrieval tool
- forward only read-only search requests
- reject write tools at the facade layer
- log forwarded request metadata

## Documentation Updates After Implementation

Update:

- `evals/bench/memd-multiturn-token-savings/README.md`
- `evals/bench/memd-multiturn-token-savings/results/<run_set>/summary.md`
- `tasks/todo.md`
- memd task artifacts for run start, run finish, and evidence

Add a final interpretation section answering:

- Did CLI/direct reduce Claude Code cache-read/cache-create overhead?
- Did thin MCP recover most of the benefit with better auditability?
- Did either condition change Codex behavior?
- Which interface should become the recommended default for agent retrieval?
