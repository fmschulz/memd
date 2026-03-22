#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
DATASET_DIR="$PROJECT_ROOT/evals/bench/datasets/retrieval"

BASE_COMMIT="7e1702284a382160ba5ef0493e741bdba95fccf2"
BASE_URL="https://raw.githubusercontent.com/fmschulz/memd/${BASE_COMMIT}/evals/bench/datasets/retrieval"

usage() {
  cat <<'EOF'
Usage: fetch_offline_benchmark_datasets.sh

Download the mirrored large offline benchmark datasets that are intentionally
not tracked in git at branch tip.

Currently fetched:
  - beir_fiqa.json
  - beir_scidocs.json

Not mirrored by this script:
  - beir_trec-covid.json

Place a local converted copy of beir_trec-covid.json at:
  evals/bench/datasets/retrieval/beir_trec-covid.json
if you need it for a custom benchmark run.
EOF
}

if [[ "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

require_cmd curl

mkdir -p "$DATASET_DIR"

fetch_one() {
  local name="$1"
  local target="$DATASET_DIR/$name"
  local url="$BASE_URL/$name"

  echo "Downloading $name"
  curl -fL --retry 3 --retry-delay 2 -o "$target" "$url"
}

fetch_one "beir_fiqa.json"
fetch_one "beir_scidocs.json"

cat <<'EOF'

Downloaded mirrored offline benchmark datasets.

Optional dataset not mirrored:
  evals/bench/datasets/retrieval/beir_trec-covid.json
EOF
