#!/usr/bin/env bash
set -euo pipefail
export RUST_LOG="${RUST_LOG:-error}"

TMP_DIR="$(mktemp -d)"
DATA_DIR="${TMP_DIR}/data"
TENANT="memd_cli_enforcement_verify"
PROJECT="verify"
MARKER="verify memd cli enforcement 20260509"

cleanup() {
  if command -v memd >/dev/null 2>&1; then
    memd --data-dir "${DATA_DIR}" warm stop >/dev/null 2>&1 || true
  fi
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

if [[ ! -f "${HOME}/.cursor/rules/memd.mdc" ]]; then
  echo "missing Cursor user rule at ~/.cursor/rules/memd.mdc" >&2
  exit 1
fi

if ! grep -Fq "Mandatory \`memd\` CLI contract" "${HOME}/.cursor/rules/memd.mdc"; then
  echo "missing CLI memd contract in ~/.cursor/rules/memd.mdc" >&2
  exit 1
fi

# `memd doctor` should run and exit 0 on a wired host.
memd doctor --format json >"${TMP_DIR}/doctor.json"
grep -Fq '"binary"' "${TMP_DIR}/doctor.json"
grep -Fq '"global_rules"' "${TMP_DIR}/doctor.json"

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

memd --data-dir "${DATA_DIR}" search \
  --tenant-id "${TENANT}" \
  --project-id "${PROJECT}" \
  --query "${MARKER}" \
  --compact \
  --token-budget 1000 \
  --format markdown >"${TMP_DIR}/warm-search.md"

grep -Fq "${MARKER}" "${TMP_DIR}/warm-search.md"

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

memd --data-dir "${DATA_DIR}" warm stop >/dev/null || true

echo "Verified memd skill + CLI enforcement and CLI memory workflow"
