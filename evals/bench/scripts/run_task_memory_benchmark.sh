#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

cd "$PROJECT_ROOT"

echo "== Phase 5 Task Memory Benchmark =="
echo "Building memd..."
cargo build -p memd >/dev/null

echo "Running task-memory benchmark..."
python3 evals/bench/tools/task_memory_benchmark.py "$@"
