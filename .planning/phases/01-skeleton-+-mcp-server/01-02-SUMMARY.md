---
phase: 01-skeleton-+-mcp-server
plan: 02
subsystem: mcp
tags: [mcp, json-rpc, stdio, protocol, tools]
dependency-graph:
  requires: [01-01]
  provides: [mcp-server, mcp-protocol, tool-schemas, tool-dispatch]
  affects: [01-03, 02-*]
tech-stack:
  added: []
  patterns: [json-rpc-2.0, stdio-transport, lazy-static]
key-files:
  created:
    - crates/memd/src/mcp/mod.rs
    - crates/memd/src/mcp/protocol.rs
    - crates/memd/src/mcp/error.rs
    - crates/memd/src/mcp/server.rs
    - crates/memd/src/mcp/tools.rs
  modified:
    - crates/memd/src/lib.rs
    - crates/memd/src/main.rs
decisions:
  - id: D01-02-01
    decision: "Protocol version 2024-11-05 for MCP compatibility"
    rationale: "Match current MCP specification version"
  - id: D01-02-02
    decision: "Logs to stderr in MCP mode, responses to stdout"
    rationale: "Keep protocol messages clean, allow debug without interference"
  - id: D01-02-03
    decision: "Tool responses use MCP content format with type=text"
    rationale: "Standard MCP tool response structure for agent compatibility"
metrics:
  duration: 8m
  completed: 2026-01-29
---

# Phase 01 Plan 02: MCP Server Implementation Summary

**One-liner:** JSON-RPC 2.0 MCP server with stdio transport, 6 memory tools with JSON Schema definitions, and proper error handling.

## What Was Built

### MCP Protocol Layer (`protocol.rs`)
- JSON-RPC 2.0 Request/Response types with serde serialization
- RequestId supporting both number and string IDs
- RpcError with standard error codes:
  - PARSE_ERROR (-32700): Invalid JSON
  - INVALID_REQUEST (-32600): Malformed request
  - METHOD_NOT_FOUND (-32601): Unknown method
  - INVALID_PARAMS (-32602): Bad parameters
  - INTERNAL_ERROR (-32603): Server error
- Request::parse() for line-based JSON parsing

### MCP Error Types (`error.rs`)
- McpError enum mapping to JSON-RPC error codes
- Tool-specific error variant (-32000 range)
- Automatic conversion to RpcError

### MCP Server (`server.rs`)
- McpServer struct with stdio event loop
- Handler methods:
  - `initialize` - Returns protocol version 2024-11-05, capabilities, serverInfo
  - `initialized` - Client ready notification
  - `tools/list` - Returns all tool definitions
  - `tools/call` - Dispatches to tool handlers
  - `shutdown` - Graceful shutdown
- Stub tool handlers validating required parameters:
  - memory.search (query, tenant_id)
  - memory.add (tenant_id, text, type)
  - memory.add_batch (tenant_id, chunks)
  - memory.get (tenant_id, chunk_id)
  - memory.delete (tenant_id, chunk_id)
  - memory.stats (tenant_id)

### Tool Definitions (`tools.rs`)
- ToolDefinition struct with name, description, JSON Schema
- LazyLock static MEMORY_TOOLS array
- 6 tools with complete JSON Schema input specifications:
  - memory.search: query, tenant_id, project_id, k, filters (types, time_range)
  - memory.add: tenant_id, text, type, project_id, source, tags
  - memory.add_batch: tenant_id, chunks[]
  - memory.get: tenant_id, chunk_id
  - memory.delete: tenant_id, chunk_id
  - memory.stats: tenant_id

### CLI Updates (`main.rs`)
- Clap argument parsing with --config, --mode, --verbose
- Mode enum: mcp (default) and cli (placeholder)
- MCP mode: JSON logs to stderr, protocol to stdout
- CLI mode: Pretty logs (not yet implemented)

## Commits

| Commit | Description |
|--------|-------------|
| 0ca369f | Implement MCP protocol types (JSON-RPC 2.0) |
| ea9aee8 | Implement MCP server with stdio transport |
| 92e30db | Define tool schemas and implement tools/list |

## Decisions Made

### D01-02-01: Protocol Version 2024-11-05
- **Context:** Need to specify MCP protocol version for capability negotiation
- **Decision:** Use "2024-11-05" as the protocol version
- **Alternatives:** Older versions, custom versioning
- **Rationale:** Match current MCP specification, future-compatible

### D01-02-02: Logs to stderr in MCP Mode
- **Context:** MCP uses stdout for protocol messages
- **Decision:** Redirect tracing logs to stderr in MCP mode
- **Alternatives:** Disable logging, file-based logging
- **Rationale:** Debug capability without protocol interference

### D01-02-03: MCP Content Format for Tool Responses
- **Context:** Tool responses need standard structure
- **Decision:** Use `{"content": [{"type": "text", "text": "..."}]}`
- **Alternatives:** Direct JSON values, custom format
- **Rationale:** Standard MCP tool response structure for agent compatibility

## Deviations from Plan

None - plan executed exactly as written.

## Test Coverage

31 new tests added (52 total), covering:
- JSON-RPC request parsing (valid, invalid, notifications)
- Response serialization (success, error)
- Error code mapping
- Server handlers (initialize, tools/list, unknown method)
- Tool call validation (missing params, missing name)
- Stub tool handlers (search, add, stats)
- Tool definitions (count, names, descriptions, schemas)
- Schema required fields validation

## Verification Results

All MCP protocol tests pass:
- `echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | memd` returns `{"serverInfo":{"name":"memd"}}`
- `tools/list` returns exactly 6 tools
- Invalid method returns error code -32601
- Invalid JSON returns error code -32700
- `cargo test`: 52 tests pass

## Next Phase Readiness

**Ready for 01-03: In-Memory Store Implementation**

Prerequisites satisfied:
- MCP server accepts requests and dispatches to tool handlers
- Tool schemas define input/output contracts
- Stub handlers ready to be replaced with actual storage calls
- Error handling infrastructure in place
