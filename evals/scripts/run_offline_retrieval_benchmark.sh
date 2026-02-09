#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESULTS_DIR="$PROJECT_ROOT/evals/results"
TIMESTAMP="$(date +%Y-%m-%d_%H-%M-%S)"

MODEL="all-minilm"
OUTPUT_DIR=""
MAX_QUERIES=""
MAX_DOCUMENTS=""
BOOTSTRAP_ITERATIONS="1000"
SEED="42"

usage() {
  cat <<'EOF'
Usage: run_offline_retrieval_benchmark.sh [options]

Options:
  --model <all-minilm|qwen3>       Embedding model (default: all-minilm)
  --output-dir <path>              Output directory (default: evals/results/offline-<timestamp>)
  --max-queries <n>                Optional query cap per dataset
  --max-documents <n>              Optional document cap per dataset
  --bootstrap-iterations <n>       Bootstrap iterations (default: 1000)
  --seed <n>                       Random seed for deterministic bootstrap (default: 42)
  --help                           Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)
      MODEL="$2"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --max-queries)
      MAX_QUERIES="$2"
      shift 2
      ;;
    --max-documents)
      MAX_DOCUMENTS="$2"
      shift 2
      ;;
    --bootstrap-iterations)
      BOOTSTRAP_ITERATIONS="$2"
      shift 2
      ;;
    --seed)
      SEED="$2"
      shift 2
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$RESULTS_DIR/offline-${TIMESTAMP}"
fi
mkdir -p "$OUTPUT_DIR"

cd "$PROJECT_ROOT"

declare -a DATASETS=(
  "evals/datasets/retrieval/beir_fiqa.json"
  "evals/datasets/retrieval/beir_scidocs.json"
  "evals/datasets/retrieval/beir_trec-covid.json"
)

echo "== Offline Retrieval Benchmark Protocol =="
echo "Model: $MODEL"
echo "Output: $OUTPUT_DIR"
echo "Bootstrap iterations: $BOOTSTRAP_ITERATIONS"
echo "Seed: $SEED"
[[ -n "$MAX_QUERIES" ]] && echo "Max queries: $MAX_QUERIES"
[[ -n "$MAX_DOCUMENTS" ]] && echo "Max documents: $MAX_DOCUMENTS"
echo

cargo build -p memd-evals >/dev/null
cargo build -p memd >/dev/null

report_path="$OUTPUT_DIR/cross_corpus_${MODEL}.json"
cmd=(
  cargo run -p memd-evals -- --suite benchmark --skip-build
  --memd-path target/debug/memd
  --embedding-model "$MODEL"
  --bootstrap-iterations "$BOOTSTRAP_ITERATIONS"
  --seed "$SEED"
  --report-json "$report_path"
)

for dataset in "${DATASETS[@]}"; do
  cmd+=(--dataset-path "$dataset")
done

if [[ -n "$MAX_QUERIES" ]]; then
  cmd+=(--max-queries "$MAX_QUERIES")
fi
if [[ -n "$MAX_DOCUMENTS" ]]; then
  cmd+=(--max-documents "$MAX_DOCUMENTS")
fi

echo "Running multi-dataset benchmark with shared deterministic options..."
"${cmd[@]}"
echo
echo "Cross-corpus report ready: $report_path"
