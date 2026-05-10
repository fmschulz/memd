#!/usr/bin/env bash
set -euo pipefail

TMP_DIR="$(mktemp -d)"
TENANT="cli_workflow_verify"
PROJECT="verification"
MARKER="marker_from_cli_20260509"

cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

if [[ -x "./target/release/memd" ]]; then
  MEMD_BIN="./target/release/memd"
else
  require_cmd cargo
  cargo build --release -p memd >/dev/null
  MEMD_BIN="./target/release/memd"
fi

DATA_DIR="${TMP_DIR}/data"
CONTEXT_FILE="${TMP_DIR}/context.md"
LOG_DIR="${TMP_DIR}/logs"

"${MEMD_BIN}" --data-dir "${DATA_DIR}" add \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --chunk-type summary \
  --tags kind:verify,source:script \
  --text "${MARKER}" >/dev/null

"${MEMD_BIN}" --data-dir "${DATA_DIR}" search \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --query "${MARKER}" \
  --warm off \
  --compact \
  --token-budget 1000 \
  --format markdown >"${TMP_DIR}/search.md"

grep -Fq "${MARKER}" "${TMP_DIR}/search.md"

"${MEMD_BIN}" --data-dir "${DATA_DIR}" agent-context \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --query "${MARKER}" \
  --warm off \
  --k 2 \
  --token-budget 700 \
  --format markdown \
  --output "${CONTEXT_FILE}" \
  --log-dir "${LOG_DIR}"

grep -Fq 'interface: `cli_only`' "${CONTEXT_FILE}"
grep -Fq "${MARKER}" "${CONTEXT_FILE}"
test -s "${LOG_DIR}/memd_search_log.jsonl"

echo "Verified skill + CLI memd workflow: add, search, agent-context, and audit logs"
