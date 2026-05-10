# memd token overhead measurement

## Measurement boundary

`memd` can directly observe serialized local operation payloads. It cannot directly observe
the agent's full prompt, hidden reasoning, provider cache accounting, or
non-`memd` tool transcripts. Therefore token monitoring has two layers:

1. `memd` payload estimates from `memory.metrics.token_usage`.
2. Whole-agent token deltas from paired with/without runs that capture provider
   API usage or an agent CLI `tokens used` footer.

The estimator is `ceil(serialized_payload_bytes / 4)`. It is
useful for comparing tools, compact modes, tenants, and response sizes, but it
is not provider billing data.

## Runtime monitoring

`memory.metrics` now returns a `token_usage` block with:

- `estimator`
- `total.calls`, `total.errors`, byte totals, and estimated token totals
- `by_tool` aggregates keyed by operation name
- `recent_tool_calls` when `include_recent` is true

Set `include_recent: false` to keep the output compact while retaining aggregate
token counters.

## Benchmark parser

Use:

```bash
python3 evals/bench/tools/token_overhead.py
```

The script reports exact paired deltas when transcripts contain a `tokens used`
footer, and separately reports a rough transcript-size estimate for all paired
runs. Exact footer rows are the preferred whole-agent signal.

## Existing transcript results

Checked-in exact footer pairs currently cover the `memd` pilot project and the
`alpha_gateway` v2 fixture:

| suite | project | qid | exact with | exact without | delta |
|---|---|---:|---:|---:|---:|
| memd-xproject-pilot | home_fschulz_dev_software_memd | q1_review_capture_rule | 229245 | 127609 | +101636 |
| memd-xproject-pilot | home_fschulz_dev_software_memd | q3_review_protocol | 96256 | 50799 | +45457 |
| v2-xproject | alpha_gateway | a1_echo_id | 43477 | 45983 | -2506 |
| v2-xproject sweep2 | alpha_gateway | a1_echo_id | 85453 | 132029 | -46576 |

Mean exact delta in this small checked-in set is `+24503` tokens; median is
`+21476`. Treat this as a pilot sanity check, not a headline. The v2 checked-in
data only has exact paired Codex footer rows for `alpha_gateway`, and the older
pilot has known filesystem-contamination issues.

## Historical payload probe

Compact `memory.search` probes were run on three existing `memd` projects with
`limit: 3`, `compact: true`, and `token_budget: 1000`. These historical
client-side probe numbers included the old wrapper bytes; current CLI-only runs
should be remeasured through `memd call`.

| project | elapsed_ms | request_bytes | response_bytes | estimated_mcp_tokens |
|---|---:|---:|---:|---:|
| bench_v2_alpha/alpha_gateway | 190 | 246 | 8392 | 2160 |
| memd/memd | 223 | 243 | 11518 | 2941 |
| default/bester-hosting | 2724 | 257 | 13567 | 3456 |

For these compact lookups, using `memd` added about `2160-3456` observable
payload tokens per lookup versus `0` `memd` payload tokens without `memd`. Whole
agent deltas can still be lower or negative when `memd` prevents broad
filesystem scans, and higher when the agent records many task/evidence calls.

## Recommended benchmark protocol

For publishable overhead numbers:

1. Use fixed prompts, agent, model, reasoning effort, cwd, and timeout.
2. Run paired with/without conditions; omit `memd` CLI retrieval from the
   without condition.
3. Capture exact provider usage or CLI `tokens used` footer.
4. Query `memory.metrics` before and after the with run and subtract
   `token_usage` counters to get the `memd`-observable component.
5. Report correctness alongside token delta, since a cheaper failed answer is
   not a useful memory-system win.
