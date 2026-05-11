# Retired MCP Conformance Suite

`evals/harness/src/suites/mcp_conformance.rs` was retired during the CLI-first
cleanup because the current `memd` executable no longer starts an MCP stdio
server with `--mode mcp`.

Current coverage moved to:

- `evals/harness/src/suites/cli_contract.rs` for supported executable commands
  and explicit rejection of `memd --mode mcp`.
- `evals/harness/src/mcp_client.rs` for a temporary compatibility wrapper used
  by older behavior and retrieval suites. It calls the current `memd call`
  operation surface instead of starting a protocol server.

The retired protocol-only assertions were not preserved as live tests because
they validated the removed transport rather than the supported executable
contract.
