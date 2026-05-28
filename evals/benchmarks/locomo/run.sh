#!/usr/bin/env bash
# One-command LoCoMo retrieval benchmark.
#
# Fetches the dataset if needed, builds memd, runs the memd adapter, and
# attempts the optional Mem0 and SuperLocalMemory adapters when their
# Python venvs are available (envs/mem0, envs/slm under this directory).
# Renders a consolidated comparison markdown.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
RESULTS="${SCRIPT_DIR}/results"
TOOLS="${SCRIPT_DIR}/tools"

mkdir -p "${RESULTS}"

# 1. Dataset
"${SCRIPT_DIR}/fetch_locomo.sh"

# 2. memd binary
if [[ ! -x "${REPO_ROOT}/target/release/memd" ]]; then
  echo "Building memd release binary..."
  (cd "${REPO_ROOT}" && cargo build --release -p memd)
fi

# 3. memd benchmark
echo "==> Running memd"
python3 "${TOOLS}/bench_runner.py" \
  --system memd \
  --memd-bin "${REPO_ROOT}/target/release/memd" \
  --dataset "${SCRIPT_DIR}/datasets/locomo10.json" \
  --out "${RESULTS}/memd_full_locomo.json" \
  --markdown-out "${RESULTS}/memd_full_locomo.md"

# 4. Optional contenders (only if their venvs exist)
results_to_merge=( "${RESULTS}/memd_full_locomo.json" )

if [[ -d "${SCRIPT_DIR}/envs/mem0" ]]; then
  echo "==> Running mem0 (using envs/mem0)"
  # shellcheck source=/dev/null
  source "${SCRIPT_DIR}/envs/mem0/bin/activate"
  python "${TOOLS}/bench_runner.py" \
    --system mem0 \
    --mem0-llm-endpoint "${MEM0_LLM_ENDPOINT:-http://127.0.0.1:8010/v1}" \
    --mem0-llm-model    "${MEM0_LLM_MODEL:-gemma4-31b}" \
    --mem0-data-dir "${RESULTS}/mem0_full_locomo_data" \
    --dataset "${SCRIPT_DIR}/datasets/locomo10.json" \
    --out "${RESULTS}/mem0_full_locomo.json" \
    --markdown-out "${RESULTS}/mem0_full_locomo.md"
  deactivate
  results_to_merge+=( "${RESULTS}/mem0_full_locomo.json" )
fi

if [[ -d "${SCRIPT_DIR}/envs/slm" ]]; then
  echo "==> Running superlocalmemory (using envs/slm)"
  # shellcheck source=/dev/null
  source "${SCRIPT_DIR}/envs/slm/bin/activate"
  python "${TOOLS}/bench_runner.py" \
    --system superlocalmemory \
    --slm-data-dir "${RESULTS}/slm_full_locomo_data" \
    --dataset "${SCRIPT_DIR}/datasets/locomo10.json" \
    --out "${RESULTS}/slm_full_locomo.json" \
    --markdown-out "${RESULTS}/slm_full_locomo.md"
  deactivate
  results_to_merge+=( "${RESULTS}/slm_full_locomo.json" )
fi

# 5. Merge
echo "==> Merging results"
python3 "${TOOLS}/bench_runner.py" \
  --merge "${results_to_merge[@]}" \
  --out "${RESULTS}/comparison_full_locomo.json" \
  --markdown-out "${RESULTS}/comparison_full_locomo.md"

echo
echo "Done. See:"
echo "  ${RESULTS}/comparison_full_locomo.md"
