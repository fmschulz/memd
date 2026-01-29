# Phase 1: Skeleton + MCP Server - Context

**Gathered:** 2026-01-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Build a foundational MCP server that agents can connect to via stdio and invoke memory tools through proper protocol conformance. Includes stub tool implementations (memory.add, memory.search, memory.stats) with in-memory storage, proper MCP error handling, and structured logging. Agents should be able to successfully connect, call tools, and receive well-formed responses.

</domain>

<decisions>
## Implementation Decisions

### Invocation modes
- **Dual mode support:** Both MCP server (stdio) and minimal CLI wrapper
- MCP server mode: Primary interface for agent integration via stdio protocol
- CLI mode: Direct invocation (`memd add`, `memd search`, `memd stats`) for manual testing and debugging
- Same Rust binary exposes both interfaces — mode selected via command-line flags or subcommands

### Claude's Discretion
- Stub tool response formats (empty vs minimal realistic mock data)
- Error message structure and MCP error code mapping
- Logging approach (JSON structure, log levels, what operations to log)
- Tenant directory layout and conventions
- Debug/verbose mode implementation
- In-memory store data structures
- CLI argument parsing and help text formatting

</decisions>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches for MCP protocol conformance and Rust CLI patterns.

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 01-skeleton-+-mcp-server*
*Context gathered: 2026-01-29*
