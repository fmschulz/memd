#!/usr/bin/env bash
set -euo pipefail

TMP_DIR="$(mktemp -d)"
PORT="${MEMD_VERIFY_PORT:-8787}"
TENANT="cross_cli_demo"
URL="http://127.0.0.1:${PORT}/mcp"
WORK_DIR="${TMP_DIR}/workspace"
CLAUDE_CONFIG="${TMP_DIR}/claude-mcp.json"
MEMD_LOG="${TMP_DIR}/memd.log"
CODEX_WRITE_OUT="${TMP_DIR}/codex-write.txt"
CODEX_READ_OUT="${TMP_DIR}/codex-read.txt"

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

require_cmd cargo
require_cmd codex
require_cmd claude
require_cmd curl

mkdir -p "${WORK_DIR}"

cat >"${CLAUDE_CONFIG}" <<EOF
{"mcpServers":{"memd":{"type":"http","url":"${URL}"}}}
EOF

if ! claude -p --permission-mode bypassPermissions --output-format text -- 'ok' >/dev/null 2>&1; then
  echo "Claude Code is installed but not authenticated. Fix Claude auth before running this verification." >&2
  exit 1
fi

cargo run -p memd -- --mode mcp --transport http --http-bind "127.0.0.1:${PORT}" --data-dir "${TMP_DIR}/data" >"${MEMD_LOG}" 2>&1 &
MEMD_PID=$!

for _ in $(seq 1 30); do
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
  -C "${WORK_DIR}" \
  -c "mcp_servers.memd.url=\"${URL}\"" \
  -o "${CODEX_WRITE_OUT}" \
  "Use the memd MCP server only. Do not use shell commands. Call memory.add with tenant_id \"${TENANT}\", project_id \"verification\", type \"summary\", text \"marker_from_codex_20260322\". Then answer with exactly: stored" \
  >/dev/null

grep -qx 'stored' "${CODEX_WRITE_OUT}"

CLAUDE_READ_PROMPT="Use the memd MCP server only. Do not use Bash. Search tenant_id \"${TENANT}\" for \"marker_from_codex_20260322\" with memory.search and return only the matching text if found."
CLAUDE_READ_OUTPUT="$(
  printf '%s' "${CLAUDE_READ_PROMPT}" | \
    claude -p \
      --permission-mode bypassPermissions \
      --strict-mcp-config \
      --mcp-config "${CLAUDE_CONFIG}" \
      --output-format text
)"

grep -Fqx 'marker_from_codex_20260322' <<<"${CLAUDE_READ_OUTPUT}"

CLAUDE_WRITE_PROMPT="Use the memd MCP server only. Do not use Bash. Call memory.add with tenant_id \"${TENANT}\", project_id \"verification\", type \"summary\", text \"marker_from_claude_20260322\". Then answer with exactly: stored"
CLAUDE_WRITE_OUTPUT="$(
  printf '%s' "${CLAUDE_WRITE_PROMPT}" | \
    claude -p \
      --permission-mode bypassPermissions \
      --strict-mcp-config \
      --mcp-config "${CLAUDE_CONFIG}" \
      --output-format text
)"

grep -Fqx 'stored' <<<"${CLAUDE_WRITE_OUTPUT}"

codex exec \
  --skip-git-repo-check \
  -C "${WORK_DIR}" \
  -c "mcp_servers.memd.url=\"${URL}\"" \
  -o "${CODEX_READ_OUT}" \
  "Use the memd MCP server only. Do not use shell commands. Search tenant_id \"${TENANT}\" for \"marker_from_claude_20260322\" with memory.search and then answer with exactly the matching text." \
  >/dev/null

grep -Fqx 'marker_from_claude_20260322' "${CODEX_READ_OUT}"

echo "Verified Codex -> memd -> Claude and Claude -> memd -> Codex over ${URL}"
