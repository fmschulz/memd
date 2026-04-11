# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - 2026-04-10

### Added
- Explicit trust-boundary metadata on search and digest-style MCP responses.
- `artifact.verify` for grounding claims against canonical artifacts.
- Structural runtime wiring so `code.find_*` tools are initialized in normal server startup.
- A checked-in structural benchmark fixture for the eval harness.

### Changed
- The compiled wiki prototype now renders trust tiers and grounding links when current memd metadata is available.
- The packaged memd skill binary has been rebuilt from the current release tree.

### Fixed
- Structural benchmark runs no longer depend on a missing fixture or an uninitialized structural index.
- The local shared HTTP daemon can now be restarted from the current installed binary to expose the latest shipped behavior.

## [0.2.0] - 2026-04-01

### Added
- Shared local HTTP daemon support for multi-session MCP access.
- Structured task and artifact workflows for progress tracking, evidence capture, review, and thread-level collaboration.
- Summary-first project briefs, task resumes, and failure, decision, evidence, and highlight digests.
- Additional MCP tools for context retrieval, structural code queries, and debug inspection.

### Changed
- Retrieval can widen by `project_id` across older local tenant histories when needed.
- The release surface and bundled skill assets are aligned with the current `main` branch.

### Fixed
- Search-style retrieval now skips unreadable finalized chunks instead of aborting on CRC-related storage errors.
- The packaged Linux skill binary has been refreshed to the current release build.
