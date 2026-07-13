# Archived LoCoMo retrieval benchmark (May–June 2026)

This harness and its checked-in summaries are frozen exploratory history. They
do not satisfy the current binary, model, cache, matched-budget, or immutable
artifact requirements and must not support current product or manuscript
claims. The canonical public protocol lives in the separate `memd-bench`
repository under `benchmarks/locomo/`.

Direct retrieval benchmark on upstream
[`locomo10.json`](https://github.com/snap-stanford/locomo): each memory
system is seeded with the same conversation turns and scored against
LoCoMo `evidence` IDs (MRR@10 over categories 1–4).

Built systems: **memd**, **mem0**, **superlocalmemory**. Each adapter
lives under `tools/adapters/`. Cognee and Letta adapters are welcome
contributions but blocked on infrastructure-level fixes (see
**Known limitations** below).

## Historical entrypoint

```bash
./evals/legacy/locomo-2026-05/harness/run.sh
```

This:

1. Downloads `datasets/locomo10.json` from snap-stanford/locomo (skip if
   already present).
2. Builds `memd` release binary if needed.
3. Runs the memd adapter and writes `results/memd_full_locomo.{json,md}`.
4. Skips contender systems unless their venvs already exist (see below).
5. Renders the consolidated `results/comparison_full_locomo.md` table.

Per-system runs:

```bash
# memd only (always available — uses the local release binary)
python tools/bench_runner.py --system memd --memd-bin target/release/memd

# mem0 (requires `pip install mem0ai` + OpenAI-compatible LLM endpoint)
python tools/bench_runner.py --system mem0 \
  --mem0-llm-endpoint http://127.0.0.1:8010/v1 \
  --mem0-llm-model gemma4-31b

# superlocalmemory (requires `pip install superlocalmemory` or local clone)
python tools/bench_runner.py --system superlocalmemory
```

## What it measures

For each system the harness records:

- **MRR@10** — primary quality metric. Per-conversation aggregation
  (mean of per-query reciprocal ranks).
- **Hit@1, Hit@3, Hit@10** — hit-rate at each cutoff.
- **Avg / p95 search latency** — per-query end-to-end recall time.
- **Seed total** — wallclock to ingest 5,882 turns. LLM-extracting
  systems (Mem0) trade higher seed cost for higher Hit@1; chunk-native
  systems (memd) optimize for low seed and search latency.
- **Per-category breakdown** — categories 1 (multi-hop), 2 (specific
  facts), 3 (open-domain), 4 (long-form).

## Reproducibility

- Dataset: upstream LoCoMo (10 conversations, 5,882 turns, ~1,600 QA
  pairs). The harness fetches it on first run from a pinned commit so
  the workload is stable.
- LLM choice (when applicable): we recommend a self-hosted vLLM
  endpoint with a known model (e.g. `gemma4-31b`), passed through
  `--mem0-llm-endpoint` and `--mem0-llm-model`. The upstream Mem0
  leaderboard uses GPT-4-class — numbers are not directly comparable
  across LLM choices, so document yours.
- Embedder: each system uses its own default unless overridden.

The full prior results from the development workspace are mirrored
to `results/baseline_2026-05-22.{json,md}` so any reproducer can
diff their numbers against a checked-in baseline.

## Known limitations

- **Cognee adapter** — Cognee's `cognify()` pipeline uses forced
  `tool_choice` to a `KnowledgeGraph` function. Self-hosted vLLM
  requires `--enable-auto-tool-choice --tool-call-parser <name>`;
  hosted OpenAI / Anthropic endpoints work out of the box. Adapter
  stub welcome.
- **Letta adapter** — Letta v0.16+ became HTTP-client only. Adapter
  needs to launch `letta server` as a subprocess, create one agent per
  conversation, and route inserts/queries through the archival
  passages API. Heavier to maintain than the others. Stub welcome.
- **SuperLocalMemory embedded mode** — SLM's subprocess
  embedding-worker uses a PID-file singleton that does not always
  detect a dead worker. We could not get SLM with embeddings enabled
  to complete a full LoCoMo pass; lexical fallback is what's reported.
  The published Mode A 74.8% MRR@10 number is not currently
  reproducible from an external operator setup.

## Files

| Path | Purpose |
|---|---|
| `README.md` | this file |
| `run.sh` | one-command end-to-end pipeline |
| `fetch_locomo.sh` | downloads `datasets/locomo10.json` |
| `tools/bench_runner.py` | harness; selects an adapter per `--system` |
| `tools/adapters/memd_adapter.py` | memd adapter (default, always available) |
| `tools/adapters/mem0_adapter.py` | optional Mem0 adapter |
| `tools/adapters/slm_adapter.py` | optional SuperLocalMemory adapter |
| `datasets/` | downloaded LoCoMo JSON (gitignored) |
| `results/` | per-system + merged results (gitignored except baselines) |
