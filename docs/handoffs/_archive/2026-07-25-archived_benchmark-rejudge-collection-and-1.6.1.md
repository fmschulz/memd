# Handoff: Benchmark - re-judge, collection rebuild, and memd 1.6.1

**Date:** 2026-07-25
**Branch:** memd `main` (HEAD `4bc8904`), memd-bench `feat/reproducible-benchmark-evidence` (HEAD `dcc4812`)

## Context & Status

A critical review of the benchmark suite and manuscript found that LongMemEval
judge failures were being scored as wrong answers for the system under test.
That is now fixed end to end: the judge fails closed, all nine systems were
re-judged cleanly, the evidence collection was rebuilt, and the numbers are
bound into the manuscript. Two releases went out, 1.6.0 and then 1.6.1 fixing
defects an adversarial review found in 1.6.0.

Everything is committed and pushed. Both repositories are clean.

Review: `tasks/reviews/2026-07-24_benchmark-manuscript-consistency-review.md`.
Plan: `tasks/2026-07-24-longmemeval-rejudge-plan.md`.
Prior handoff: `docs/handoffs/2026-07-24_longmemeval-rejudge-clean-collection-blocked.md`.

## Technical Implementation

### Work completed

**Re-judge.** Nine phases, zero judge failures, zero truncations across 4,500
rows, on a container started from the pinned image
`ghcr.io/ggml-org/llama.cpp@sha256:54421a9c…` at `--parallel 4 --ctx-size 16384`
(4,096 per slot), GPU 2, host port 9911.

| System | accuracy | 95% CI | was | Δ | rec@20 |
|---|---:|---|---:|---:|---:|
| SuperLocalMemory | 36.4 | [33.0, 39.8] | 33.8 | +2.6 | 97.26 |
| BM25 control | 35.0 | [31.6, 38.4] | 33.4 | +1.6 | 97.69 |
| memd | 34.8 | [31.2, 38.4] | 32.2 | +2.6 | 98.87 |
| LightMem | 31.6 | [28.0, 35.4] | 26.6 | +5.0 | - |
| Dense control | 31.2 | [27.8, 34.6] | 29.4 | +1.8 | 97.88 |
| MemOS | 31.2 | [27.6, 34.6] | 29.2 | +2.0 | 97.65 |
| Mem0 | 18.8 | [15.6, 22.0] | 16.0 | +2.8 | 71.67 |
| Recency control | 7.8 | [5.6, 10.2] | 7.0 | +0.8 | 49.31 |
| Graphiti | 0.0 | - | - | - | 0.00 |

LightMem moved sixth to fourth, so the defect did change the ordering.

**Collection.** `1113a0d6561ceb92`, five workspaces, verifies recursively.
CAS `36e61c69a13086d3` verifies and packs to 466,124,308 bytes.
`check_assertions verify` passes for **14 claims**, up from 11.

**Manuscript.** Section 5.4 now binds `longmemeval_protocol`,
`longmemeval_results` and `longmemeval_judge_reliability`, states the full
ordering, and names the BM25, Dense RAG and Recency controls (review finding
B3). Notebook executes clean and its summary is byte-identical across two runs.

**memd 1.6.0** shipped `verifier_error` plus the consolidation slot ceiling.
**memd 1.6.1** fixed two defects adversarial review found in that shipped code.

### Outcomes

- **What worked:** the fail-closed gate caught its own inadequacy three times,
  first on a residual truncation, then on 19 timeout rows the truncation-only
  version let through, then on a pre-fix judge artifact the renderer would have
  published. Each catch was the same defect class one level further out.
- **What didn't:** several estimates in the plan were wrong, and one review
  finding was wrong. Both are recorded below.

### File map

| File | Change | Notes |
|------|--------|-------|
| `crates/memd/src/cli/consolidate.rs` | Modified | slot ceiling, `seal_inherited_descriptors`, EINVAL tolerance |
| `crates/memd/src/store/outcome.rs` | Modified | `VerifierError`, `#[non_exhaustive]` |
| `crates/memd/src/cli/session_start.rs` | Modified | spawn errors surfaced |
| `README.md`, `docs/comparison.md`, `docs/benchmarking.md` | Modified | superseded claims replaced |
| `benchmarks/locomo/` | Deleted | stale fork of the memd-bench harness |
| memd-bench `benchmarks/longmemeval/judge_client.py` | Added | fail-closed judge client |
| memd-bench `benchmarks/restore_run_dirs.py` | Added | restore a frozen closure from a bundle |
| memd-bench `manuscript/render_claims.py` | Added | shared claim binding |
| memd-bench `manuscript/render_longmemeval.py` | Added | three bound evidence regions |

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Fix the judge additively in a new client | `answering.py` is pinned by digest in every frozen QA manifest; editing it makes those phases un-rerunnable |
| Tolerate divergence only in inherited subject artifacts | A judge phase reads frozen answers and never executes the subject |
| Abort on any judge-side failure, not only truncation | Timeout and unparseable verdict charge the same infrastructure failure to the system under test |
| Start a new container from the pinned image rather than reuse port 8015 | 8015 is a different container; judging there while recording the pinned digest would be false provenance |
| Bound background consolidation at four slots, not one | Consolidation is mostly model-call time, so some cross-project parallelism is real throughput |
| Release 1.6.0 as minor, not 2.0.0 | Chef's call after the semver risk was surfaced. Codex maintains 2.0.0 was required; a downstream exhaustive `match` breaks on a patch upgrade within 1.6.x |
| No counter in `memd report` for `verifier_error` | That command reads a CLI usage ledger, a different concept, and does not read episodes |

## Knowledge Capture

### Lessons learned

Recorded in `tasks/lessons.md` and memd:

- A phase manifest is bound to the tree that produced it. Decide where a re-run
  must execute before starting it.
- Run the cross-vendor review on every change and never below the effort in
  `~/.codex/config.toml`. Four of about ten changes were reviewed, at medium
  effort against a configured max, and the defects were in the unreviewed half.
- Check whether a public Rust enum is reachable from the crate root before
  choosing a release version, and mark it `#[non_exhaustive]` in the same
  release so the break happens once.
- Verify a reviewer's claim before acting, and verify your own repro. My first
  attempt at the lock-leak repro used Python's default `close_fds=True`, which
  masked it; Rust's `Command` does not close descriptors.

### Gotchas

- **The pinned judge model is no longer downloadable.**
  `gemma-4-31B-it-Q4_K_M.gguf` is gone from `ggml-org/gemma-4-31B-it-GGUF`;
  upstream serves only Q4_0, Q8_0 and BF16. The local copy at revision
  `fb5801c7…` is the only source. Belongs in the manuscript limitations.
- **`--parallel N` divides `--ctx-size` by N.** A server advertising 8,192
  serves 2,048 per slot, and `finish_reason: "length"` fires on the context
  limit, not `max_output_tokens`.
- **Rebuilding the collection from ROOT needs two restores.**
  `benchmark-data/memd-source/target/release/memd` must be force-restored to the
  f959306 build, and `reference_adapter.py` plus `mem0_adapter.py` are pinned
  artifacts whose committed versions differ from the frozen ones. Restore with
  `--force` before building, `git checkout` after.
- **`auto-release.yml` fires on any push to main touching `Cargo.toml` or
  `CHANGELOG.md`.** It only tags when the version has no matching tag on origin.
  Check that before pushing, or a publish happens.
- **An unexplained flake.** Before the final `cargo fmt`, one 12-run batch of
  `cargo test -p memd --lib` showed 2 failures. Not reproduced in 65 runs since,
  and not explained.
- `pkill -f '<pattern>'` matches the shell running it. Use a bracketed pattern.

## Moving Forward

### Next steps

1. **Manuscript items C2, C3 and C4 from the review.** Move the
   content-deduplication limitation into Methods with its measured numbers
   (4,866,209,706 logical → 1,991,337,419 CAS → 466,594,126 packed, byte
   identical repack); fix the abstract's "recall@3 from 0.9766 to 0.9766"
   wording; report the LoCoMo retrieval ablation (hybrid 0.4762, BM25 0.3375,
   dense 0.3228) as bound claims.
2. Add the unfetchable judge model to the manuscript limitations.
3. Decide whether the seven orphaned figures become captioned supplementary
   figures or stop being rendered (review finding C4).
4. Verify references [5] and [8] before submission (review finding C5).
5. Consider whether the 1.6.x semver decision needs revisiting given Codex's
   position that it required a major.

### Blockers

- None. The collection rebuild that blocked the previous handoff is complete.
- Housekeeping: the judge container is still on GPU 2 holding about 21 GB.
  `docker rm -f memd-bench-gemma4-31b-judge-ctx16k` when it is no longer wanted.
- `benchmark-artifacts/rejudged-v2` is 3.6 GB plus a 1.99 GB CAS and a 466 MB
  archive, all gitignored.
