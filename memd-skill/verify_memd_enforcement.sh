#!/usr/bin/env bash
set -euo pipefail

TMP_DIR="$(mktemp -d)"
TENANT="memd_cli_enforcement_verify"
PROJECT="verify"
MARKER="verify memd cli enforcement 20260509"

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

require_cmd memd

if ! grep -Fq "<!-- memd-enforcement:start -->" "${HOME}/.codex/AGENTS.md"; then
  echo "missing memd enforcement block in ~/.codex/AGENTS.md" >&2
  exit 1
fi

if ! grep -Fq "Mandatory \`memd\` CLI contract" "${HOME}/.codex/AGENTS.md"; then
  echo "missing CLI memd contract in ~/.codex/AGENTS.md" >&2
  exit 1
fi

if ! grep -Fq "<!-- memd-enforcement:start -->" "${HOME}/.claude/CLAUDE.md"; then
  echo "missing memd enforcement block in ~/.claude/CLAUDE.md" >&2
  exit 1
fi

if ! grep -Fq "Mandatory \`memd\` CLI contract" "${HOME}/.claude/CLAUDE.md"; then
  echo "missing CLI memd contract in ~/.claude/CLAUDE.md" >&2
  exit 1
fi

DATA_DIR="${TMP_DIR}/data"
CONTEXT_FILE="${TMP_DIR}/context.md"
LOG_DIR="${TMP_DIR}/logs"

memd --data-dir "${DATA_DIR}" add \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --chunk-type summary \
  --tags kind:verify,source:skill \
  --text "${MARKER}" >/dev/null

memd --data-dir "${DATA_DIR}" search \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --query "${MARKER}" \
  --warm off \
  --compact \
  --token-budget 1000 \
  --format markdown >"${TMP_DIR}/search.md"

grep -Fq "${MARKER}" "${TMP_DIR}/search.md"

memd --data-dir "${DATA_DIR}" agent-context \
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

echo "Verified memd skill + CLI enforcement and CLI memory workflow"
