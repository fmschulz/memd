# Benchmark inventory

Public cross-system evidence is built in the sibling `memd-bench` repository,
where artifact bundles pin the source, binary, dataset, model, and runtime.

## Active checks

| Surface | Entrypoint | Evidence role |
| --- | --- | --- |
| BEIR retrieval gate | `.github/workflows/retrieval-gate.yml` runs `memd-evals --suite benchmark` and `--suite benchmark-regression` | CI comparison against `baselines/beir_v1.json`, using fetched fiqa and scidocs datasets |
| Local retrieval | `scripts/run_offline_retrieval_benchmark.sh` | Tracked code_pairs fixture plus available BEIR datasets |
| Task memory | `scripts/run_task_memory_benchmark.sh` | Internal structured-memory behavior and latency |
| Longitudinal memory | `longitudinal/run.sh` | Frozen repeated-task development evaluation |

## Supporting files

- `baselines/` and `datasets/`: tracked baselines and small fixtures. Large
  datasets are fetched or supplied locally.
- `queries/`: query fixtures for the retained CLI retrieval evaluations.
- `tools/task_memory_benchmark.py`: implementation of the task-memory script.
- `experience-memory/` and `reflection/`: development validation notebooks.
- `results/`: internal development snapshots.

Generated stores and large datasets remain local. Public results require a
validated `memd-bench` artifact bundle.
