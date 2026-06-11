#!/usr/bin/env bash
# Execute the benchmark figure notebooks in place, writing PNG/SVG figures
# to docs/figures/. Reproducible: reads only the checked-in JSON snapshots
# under evals/notebooks/data/. Requires uv.
#
#   bash evals/notebooks/run_notebooks.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Regenerate the .ipynb from cell sources, then execute in place.
uv run --with nbformat python build_notebooks.py

for nb in locomo_cross_system.ipynb beir_retrieval_gate.ipynb; do
  echo "== executing $nb =="
  uv run --with matplotlib,numpy,nbconvert,nbformat,ipykernel \
    jupyter nbconvert --to notebook --execute --inplace \
    --ExecutePreprocessor.timeout=120 "$nb"
done

echo "Figures written to docs/figures/"
