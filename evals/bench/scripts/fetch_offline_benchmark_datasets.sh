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

Currently fetched (required):
  - beir_fiqa.json       (~45 MB, Apache-2.0)
  - beir_scidocs.json    (~80 MB, CC-BY-4.0)

Optionally fetched (skipped silently if absent from the pinned commit mirror):
  - beir_trec-covid.json (~140 MB, TREC-COVID 2020 corpus)

Suite-specific datasets NOT fetched by this script (scifact / nfcorpus
suites expect them present locally when you run --suite scifact or
--suite nfcorpus):
  - beir_scifact_fixed.json
  - beir_nfcorpus.json

If you already have a local converted copy of any unfetched dataset,
place it under evals/bench/datasets/retrieval/ with the filename listed
above.
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

# Tolerant variant: warn on 404 instead of failing the whole script. Used for
# datasets that may not be present in the pinned mirror commit (e.g. a file
# too large to live in the commit tree). Users can either rely on this
# attempt or place the file at $DATASET_DIR/$name manually.
try_fetch_one() {
  local name="$1"
  local target="$DATASET_DIR/$name"
  local url="$BASE_URL/$name"

  echo "Attempting $name (optional)"
  if curl -fL --retry 1 --retry-delay 1 -o "$target" "$url" 2>/dev/null; then
    echo "  -> fetched"
  else
    rm -f "$target"  # curl may leave an empty file on 404
    echo "  -> not mirrored at the pinned commit; skipping"
    echo "     (place a local copy at $target if you need this dataset)"
  fi
}

fetch_one "beir_fiqa.json"
fetch_one "beir_scidocs.json"
try_fetch_one "beir_trec-covid.json"

cat <<'EOF'

Downloaded mirrored offline benchmark datasets.

Optional datasets NOT fetched by this script (place manually when needed):
  evals/bench/datasets/retrieval/beir_scifact_fixed.json  (suite: scifact)
  evals/bench/datasets/retrieval/beir_nfcorpus.json       (suite: nfcorpus)
EOF
