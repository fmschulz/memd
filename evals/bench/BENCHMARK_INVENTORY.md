# Benchmark inventory

This inventory separates release gates, development evidence, experiments, and
retired harnesses. Public cross-system evidence is built in the sibling
`memd-bench` repository, where phase manifests and artifact bundles pin the
source, binary, dataset, answer model, and runtime.

## Active release gates

| Surface | Entrypoint | Evidence role |
| --- | --- | --- |
| Offline retrieval | `scripts/run_offline_retrieval_benchmark.sh` | Fast BEIR-style regression gate used by CI |
| Task memory | `scripts/run_task_memory_benchmark.sh` | Internal structured-memory behavior and latency |
| Longitudinal memory | `longitudinal/run.sh` | Frozen repeated-task development evaluation; protocol v1 is immutable |

The benchmark scripts use the tracked baselines and fixtures under `bench/`.
Generated stores and large datasets remain local and are not sources of record.

## Experimental surfaces

- `bright-pro-memd/`: scoped biology retrieval adapter, not a full benchmark.
- `memd-multiturn-token-savings/`: interface and token-overhead experiment.
- `v2-xproject/`: cross-project usefulness pilot.
- `tools/`: analysis utilities shared by the active internal gates.

## Retired surfaces

- `../legacy/locomo-2026-05/`: the former in-repository cross-system LoCoMo
  harness, notebooks, snapshots, and figures. Its results are exploratory
  history and do not meet the current artifact contract.
- `benchmarks/locomo/` at the repository root: deleted 2026-07-24. It was a
  fork of the `memd-bench` LoCoMo harness that had diverged in every file and
  appeared in no inventory. `memd-bench` owns that protocol; recover the
  deleted copy from git history if a historical invocation must be inspected.
- The MCP conformance suite was retired when the public executable became
  CLI-first. Current coverage is in `evals/harness/src/suites/cli_contract.rs`;
  `evals/harness/src/mcp_client.rs` remains only as a compatibility wrapper for
  older behavior suites.
- The deleted harness symlinks `REPRO_BENCHMARK.md`, `BASELINES_SETUP.md`, and
  `manifests` pointed to paths that never existed in the tracked tree.

Do not restore a retired result to product documentation without rerunning it
through the current `memd-bench` protocol and freezing a verified bundle.
