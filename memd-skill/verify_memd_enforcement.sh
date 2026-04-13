#!/usr/bin/env bash
set -euo pipefail

PORT="${MEMD_VERIFY_PORT:-8791}"
URL="http://127.0.0.1:${PORT}/mcp"
TENANT="memd_enforcement_verify"
TMP_DIR="$(mktemp -d)"
WORK_DIR="${TMP_DIR}/workspace"
CLAUDE_CONFIG="${TMP_DIR}/claude-mcp.json"
MEMD_LOG="${TMP_DIR}/memd.log"
CODEX_OUT="${TMP_DIR}/codex-out.txt"
CLAUDE_OUT="${TMP_DIR}/claude-out.txt"
GUARD_OUT="${TMP_DIR}/guard-out.txt"
GUARD_ERR="${TMP_DIR}/guard-err.txt"

cleanup() {
  if [[ -n "${MEMD_PID:-}" ]]; then
    kill "${MEMD_PID}" >/dev/null 2>&1 || true
    wait "${MEMD_PID}" >/dev/null 2>&1 || true
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
require_cmd codex
require_cmd claude
require_cmd curl

if ! grep -Fq "<!-- memd-enforcement:start -->" "${HOME}/.codex/AGENTS.md"; then
  echo "missing memd enforcement block in ~/.codex/AGENTS.md" >&2
  exit 1
fi

if ! grep -Fq "Before saying the work is impossible, blocked, cannot be answered" "${HOME}/.codex/AGENTS.md"; then
  echo "missing pre-refusal memd check rule in ~/.codex/AGENTS.md" >&2
  exit 1
fi

if ! grep -Fq "<!-- memd-enforcement:start -->" "${HOME}/.claude/CLAUDE.md"; then
  echo "missing memd enforcement block in ~/.claude/CLAUDE.md" >&2
  exit 1
fi

if ! grep -Fq "Before saying the work is impossible, blocked, cannot be answered" "${HOME}/.claude/CLAUDE.md"; then
  echo "missing pre-refusal memd check rule in ~/.claude/CLAUDE.md" >&2
  exit 1
fi

mkdir -p "${WORK_DIR}"
cat >"${CLAUDE_CONFIG}" <<EOF
{"mcpServers":{"memd":{"type":"http","url":"${URL}"}}}
EOF

memd --mode mcp --transport http --http-bind "127.0.0.1:${PORT}" --data-dir "${TMP_DIR}/data" >"${MEMD_LOG}" 2>&1 &
MEMD_PID=$!

for _ in $(seq 1 40); do
  if curl -fsS -X POST "${URL}" \
    -H 'Accept: application/json, text/event-stream' \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"verify","version":"0.1.0"}}}' >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! curl -fsS -X POST "${URL}" \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"verify","version":"0.1.0"}}}' >/dev/null 2>&1; then
  echo "memd HTTP daemon did not become ready" >&2
  cat "${MEMD_LOG}" >&2
  exit 1
fi

codex exec \
  --skip-git-repo-check \
  --ephemeral \
  -C "${WORK_DIR}" \
  -c "mcp_servers.memd.url=\"${URL}\"" \
  -o "${CODEX_OUT}" \
  "Use the memd MCP server only. Do not use shell commands. Call task.start with tenant_id \"${TENANT}\", project_id \"verify\", goal \"verify memd enforcement\", motivation \"check enforced memory use\", hypothesis \"task logging works\", scientific_question \"does codex write memd task artifacts\", dataset_refs [{\"name\":\"verify_dataset\"}], expected_outputs [\"task_id\"] and then call task.finish for the created task. Answer only with the task_id." \
  >/dev/null

grep -Eq '^[0-9a-f-]{36}$' "${CODEX_OUT}"

CLAUDE_PROMPT="$(cat <<EOF
Use the memd MCP server only. Do not use Bash. Search tenant_id "${TENANT}" for "verify memd enforcement" with task.search and return only the matching goal text.
EOF
)"

printf '%s' "${CLAUDE_PROMPT}" | \
  claude -p \
    --permission-mode bypassPermissions \
    --strict-mcp-config \
    --mcp-config "${CLAUDE_CONFIG}" \
    --output-format text >"${CLAUDE_OUT}"

grep -Fqx 'Task task_start status in_progress for task' "${CLAUDE_OUT}" && {
  echo "Claude returned projection summary rather than exact goal text; memd access still verified." >&2
}

if ! grep -Fq 'verify memd enforcement' "${CLAUDE_OUT}"; then
  echo "Claude could not recover the Codex-written memd task artifact" >&2
  exit 1
fi

if command -v codex-memd-guard >/dev/null 2>&1; then
  set +e
  MEMD_URL="${URL}" MEMD_GUARD_TENANT_ID="${TENANT}" \
    codex-memd-guard \
      --skip-git-repo-check \
      --ephemeral \
      -C "${WORK_DIR}" \
      "Answer exactly: I cannot proceed. Do not use any tools." \
      >"${GUARD_OUT}" 2>"${GUARD_ERR}"
  GUARD_STATUS=$?
  set -e

  if [[ "${GUARD_STATUS}" -ne 3 ]]; then
    echo "codex-memd-guard did not block a refusal-style answer without memd retrieval" >&2
    cat "${GUARD_OUT}" >&2 || true
    cat "${GUARD_ERR}" >&2 || true
    exit 1
  fi

  if ! grep -Fq 'memd refusal guard blocked the answer' "${GUARD_ERR}"; then
    echo "codex-memd-guard exited 3 but did not emit the expected guard message" >&2
    cat "${GUARD_ERR}" >&2 || true
    exit 1
  fi
fi

echo "Verified memd enforcement snippets, MCP registration, and cross-client shared task access over ${URL}"
