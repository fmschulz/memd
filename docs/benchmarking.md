# Benchmarking

memd separates correctness gates, development experiments, and public
cross-system evidence. A benchmark number is current only when its source,
binary, dataset, configuration, row-level outputs, and analysis inputs are
bound by an immutable manifest and a verified artifact bundle.

## Evidence status

The present development branch has a frozen longitudinal result for staged
consolidation and outcome-attributed retrieval. Outcome-only shadow replay
improved success and MRR without lowering recall. The combined consolidation
and outcome loop failed its prespecified recall non-regression gate because a
synthesized lesson dropped terminology needed by a later query variant.
Outcome-aware ranking therefore remains shadow-only.

No LoCoMo, CodeIR, MemoryData, or LongMemEval comparison for the current source
revision is claim-bearing yet. Those runs require a clean committed memd
revision, pinned open-weight answer and judge models, and a validated bundle.
The sibling `memd-bench` repository owns those public protocols.

## In-repository release gates

### Offline retrieval

The fast retrieval gate evaluates the configured hybrid lane on tracked
BEIR-style fixtures and compares it with versioned baselines.

```bash
./evals/bench/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

CI uses this workload as a regression tripwire. It is deliberately small and
is not a leaderboard result. An older figure workflow is preserved under
`evals/legacy/beir-2026-06/`; its missing candidate-source identity prevents
use as current evidence.

### Task-memory behavior

The task-memory harness checks structured retrieval behavior and CLI execution
modes on an internal corpus:

```bash
./evals/bench/scripts/run_task_memory_benchmark.sh
```

Its tracked report and corpus are under
[`docs/scientific-task-memory/benchmark-results/`](scientific-task-memory/benchmark-results/README.md).
This is an internal behavior check, not a cross-system result.

### Longitudinal adaptive memory

The versioned protocol and fixtures live in `evals/bench/longitudinal/`. It
compares no memory, raw memory, admission, staged consolidation, the historical
exposure heuristic, outcome-only ranking, and the full loop. The protocol
records task success, harmful-memory rate, correction recurrence, recall, MRR,
latency, memory growth, scope violations, and crash-recovery violations.

```bash
./evals/bench/longitudinal/run.sh
```

Protocol v1 is frozen. A failed gate must change the serving decision, not the
threshold. The current result keeps `outcome-v1` disabled for serving and
available only as shadow telemetry.

### Memory-quality CLI gates

The CLI also exposes deterministic local checks:

- `memd eval-memory-md --agent-usefulness` checks startup-context structure,
  task-source state, scope health, duplicate suppression, and bounded machine
  context.
- `memd eval-retrieval` evaluates known-useful retrieval queries against sparse
  judgments.
- `memd eval-write-quality` checks admission, deduplication, retention,
  lifecycle hiding, retrieval durability, and bounded store growth.

## Public benchmark contract

The sibling `memd-bench` repository defines the current LoCoMo, CodeIR,
MemoryData, superlocalmemory, and untouched LongMemEval workflows. Each phase
writes its own immutable manifest, such as `seed.<run-id>.json`,
`retrieve.<run-id>.json`, `qa.<run-id>.json`, or `judge.<run-id>.json`.

The manifests record:

- the clean memd source commit, build command, compiler, lockfile, and binary
  digest;
- dataset repository revisions, file hashes, selections, exclusions, and row
  counts;
- exact invocation, isolated HOME/XDG paths, hardware, OS, and allowlisted
  environment;
- answer and judge model repositories, revisions, tokenizer revisions,
  serving runtime, container digest, prompt digest, and inference settings;
- parent manifests and hashes for every input and output.

LoCoMo uses same-store event-time invariance checks, paired answer generation,
conversation-cluster bootstrap, tokenizer-counted context budgets, and
dense/sparse/hybrid ablations. CodeIR stores every emitted chunk ID and
compares equally budgeted lanes, including a query-shape adaptive policy.
MemoryData uses matched `k` and token budgets. LongMemEval is untouched
confirmatory evidence whose retrieval policy must be frozen from development
benchmarks before its results are opened.

## Historical material

The former in-repository cross-system LoCoMo harness, figures, notebooks, and
snapshots are archived under `evals/legacy/locomo-2026-05/`. They mixed old and
new retrieval runs, lacked complete answer-model identity, used unmatched
budgets, and did not bind results to exact binary bytes. They remain useful for
historical inspection but must not support current product or manuscript
claims.

The complete active and retired surface map is in the repository's
`evals/bench/BENCHMARK_INVENTORY.md`.
