#!/usr/bin/env bash
# Fetch upstream LoCoMo dataset from snap-stanford/locomo.
#
# Pins to a commit so the workload is stable. Skip the download if the file
# already exists and is non-empty.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATASET_DIR="${SCRIPT_DIR}/datasets"
DATASET="${DATASET_DIR}/locomo10.json"

# Pinned to a known-good commit on snap-stanford/locomo main.
LOCOMO_REPO="snap-stanford/locomo"
LOCOMO_COMMIT="main"   # TODO: replace with a pinned commit SHA when one is reviewed
LOCOMO_PATH="data/locomo10.json"

mkdir -p "${DATASET_DIR}"

if [[ -s "${DATASET}" ]]; then
  echo "LoCoMo already present at ${DATASET} ($(wc -c < "${DATASET}") bytes); skipping fetch."
  exit 0
fi

URL="https://raw.githubusercontent.com/${LOCOMO_REPO}/${LOCOMO_COMMIT}/${LOCOMO_PATH}"
echo "Fetching ${URL}"
if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 --retry-delay 2 -o "${DATASET}" "${URL}"
elif command -v wget >/dev/null 2>&1; then
  wget -O "${DATASET}" "${URL}"
else
  echo "neither curl nor wget is available" >&2
  exit 1
fi

# Light sanity: must be a JSON array with at least one conversation.
python3 - <<PY
import json, sys
data = json.loads(open("${DATASET}").read())
if not isinstance(data, list) or not data:
    print("fetched file is not a non-empty LoCoMo array", file=sys.stderr)
    sys.exit(1)
print(f"OK: {len(data)} conversations fetched to ${DATASET}")
PY
