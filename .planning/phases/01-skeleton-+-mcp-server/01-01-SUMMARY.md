---
phase: 01-skeleton-+-mcp-server
plan: 01
subsystem: core
tags: [rust, cargo, types, config, workspace]
dependency-graph:
  requires: []
  provides: [workspace, core-types, config-loader, error-types]
  affects: [01-02, 01-03]
tech-stack:
  added: [serde, serde_json, toml, thiserror, uuid, tracing, tracing-subscriber, clap, tokio]
  patterns: [newtype-validation, builder-pattern, xdg-config]
key-files:
  created:
    - Cargo.toml
    - crates/memd/Cargo.toml
    - crates/memd/src/main.rs
    - crates/memd/src/lib.rs
    - crates/memd/src/config.rs
    - crates/memd/src/error.rs
    - crates/memd/src/types.rs
    - configs/default.toml
    - .gitignore
  modified: []
decisions:
  - id: D01-01-01
    decision: "Used UUIDv7 for ChunkId to enable time-sortable identifiers"
    rationale: "Natural chronological ordering without separate timestamp index"
  - id: D01-01-02
    decision: "TenantId validation restricts to alphanumeric + underscore"
    rationale: "Safe for file paths and database queries without escaping"
  - id: D01-01-03
    decision: "Config uses XDG convention (~/.config/memd/config.toml)"
    rationale: "Standard Linux config location, familiar to users"
metrics:
  duration: 4m4s
  completed: 2026-01-29
---

# Phase 01 Plan 01: Rust Foundation Summary

**One-liner:** Cargo workspace with validated types (TenantId, MemoryChunk), TOML config loader, and thiserror-based error handling.

## What Was Built

### Cargo Workspace Structure
- Root workspace with `crates/*` member pattern and resolver = "2"
- Shared dependency versions in workspace manifest for consistency
- memd crate with async tokio runtime and JSON tracing

### Core Types (`types.rs`)
- **TenantId**: Newtype with validation (non-empty, alphanumeric + underscore only)
- **ProjectId**: Optional wrapper for project-scoped data
- **ChunkId**: UUIDv7 wrapper for time-sortable identifiers
- **ChunkType**: 9 variants (Code, Doc, Trace, Decision, Plan, Research, Message, Summary, Other)
- **ChunkStatus**: 4 variants (Draft, Final, Error, Deleted)
- **Source**: Provenance struct with uri, repo, commit, path, tool_name, tool_call_id
- **MemoryChunk**: Complete struct with builder pattern, content hashing

### Error Handling (`error.rs`)
- MemdError enum with thiserror derivation
- Variants: ConfigError, ValidationError, StorageError, ProtocolError, IoError, JsonError, TomlError
- Result type alias for convenience

### Configuration (`config.rs`)
- Config struct with data_dir, log_level, log_format, server settings
- XDG config directory support (~/.config/memd/config.toml)
- Tilde expansion for paths
- Validation for all settings
- Default config file at configs/default.toml

## Commits

| Commit | Description |
|--------|-------------|
| 5667b2e | Initialize Cargo workspace and crate structure |
| 628ee36 | Implement core types and error handling |
| a58366f | Implement config loader with TOML support |

## Decisions Made

### D01-01-01: UUIDv7 for ChunkId
- **Context:** Need identifiers that can be sorted chronologically
- **Decision:** Use UUIDv7 which encodes timestamp in the UUID
- **Alternatives:** UUIDv4 + separate timestamp, snowflake IDs
- **Rationale:** Built-in time ordering, standard UUID format, no coordination needed

### D01-01-02: TenantId Validation
- **Context:** TenantId used in file paths and queries
- **Decision:** Restrict to alphanumeric + underscore
- **Alternatives:** Allow any UTF-8, URL-encode special chars
- **Rationale:** Simpler, safer, prevents path traversal issues

### D01-01-03: XDG Config Location
- **Context:** Need standard config file location
- **Decision:** Use ~/.config/memd/config.toml
- **Alternatives:** ~/.memd/config.toml, /etc/memd/config.toml
- **Rationale:** XDG Base Directory spec is the Linux standard

## Deviations from Plan

None - plan executed exactly as written.

## Test Coverage

22 unit tests covering:
- TenantId validation (valid, empty, invalid chars, serde roundtrip)
- ChunkId uniqueness
- ChunkType/ChunkStatus display
- MemoryChunk serialization and builder pattern
- Config loading (TOML string, partial, empty, defaults)
- Path tilde expansion
- Invalid config rejection
- Error type conversions

## Verification Results

- `cargo build --release`: Success, 0 warnings
- `cargo test`: 22 tests pass
- `cargo run`: Outputs JSON log "memd starting..."
- Project structure matches plan specification

## Next Phase Readiness

**Ready for 01-02: MCP Server Implementation**

Prerequisites satisfied:
- Core types available for tool schemas
- Error types ready for protocol error handling
- Config loader ready for server configuration
- Tracing infrastructure in place
