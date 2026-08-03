# Handoff: Release - v1.5.0 Published
**Date:** 2026-07-22
**Branch:** feat/reliable-adaptive-memory
**HEAD:** e6f88a89180ef362faa585fea47cdd057e0ef1ce

## Context & Status

memd v1.5.0 is published. The public product, private benchmark source, and
canonical controlcenter skill are on their respective `main` branches. All
release workflows and live-artifact checks passed. The local benchmark evidence
archive remains ignored and was not uploaded to GitHub.

## Technical Implementation

### Work Completed

- Updated public memd main to `e6f88a89180ef362faa585fea47cdd057e0ef1ce`.
- Updated private memd-bench main to
  `09b68d7a71010c8448728b3908881d7673e922ca`.
- Merged the five-file skill update into current controlcenter main as
  `1d0d6382b0bf6f71581e863ab1b59bfb0eb37715`.
- Published the annotated `v1.5.0` tag, GitHub Release, crates.io package, and
  documentation site.
- Verified the GitHub x86_64 Linux checksum and ran binaries installed from
  both the release archive and crates.io.

### Outcomes

- **CI:** all seven workflows passed: version consistency, retrieval gate,
  tests, docs, auto-release, crate publish, and release.
- **Registry:** a clean locked `cargo install` completed and the executable
  reported `memd 1.5.0`.
- **Release assets:** 13 files total 31,460,867 bytes. No benchmark evidence
  archive or generated run output was uploaded.
- **Documentation:** the deployed self-improvement page contains the v1.5
  consolidation, retrieval-episode, shadow-ranking, and outcome workflow.

### File Map

| Repository | Main head | Result |
|---|---|---|
| `memd` | `e6f88a89180ef362faa585fea47cdd057e0ef1ce` | v1.5.0 published |
| private `memd-bench` | `09b68d7a71010c8448728b3908881d7673e922ca` | source and manuscript published privately |
| `controlcenter` | `1d0d6382b0bf6f71581e863ab1b59bfb0eb37715` | canonical skill merged |

## Key Decisions

| Decision | Rationale |
|---|---|
| Keep benchmark evidence local and ignored | GitHub is not the stable archive for the 4.9 GB evidence closure. |
| Keep outcome ranking in shadow mode | The untouched longitudinal gate must validate serving behavior before activation. |
| Preserve fail-closed competitor results | Compatibility diagnostics do not replace frozen primary benchmark outcomes. |

## Knowledge Capture

### Lessons Learned

- Treat the public main update as the release event because it triggers tags,
  registry publication, release assets, and documentation deployment.
- Verify the product at the registry, archive, and executable levels; green CI
  alone does not establish that users can install the release.

### Gotchas

- The release archive extracts into a target-named subdirectory; run the binary
  from that directory.
- The final benchmark collection and its compact archive remain local. Preserve
  their content hashes until they are deposited in a stable external archive.

## Moving Forward

### Next Steps

1. Deposit the immutable benchmark archive and source metadata in a stable
   public repository before manuscript submission.
2. Run the untouched adaptive-policy and attribution-noise evaluation before
   enabling outcome-aware serving.

### Blockers

- None for the v1.5.0 software release.
