# Handoff: LongMemEval - re-judge clean, collection rebuild blocked

**Date:** 2026-07-24
**Branch:** memd `feat/reliable-adaptive-memory` (HEAD `e6f88a8`), memd-bench `feat/reproducible-benchmark-evidence` (HEAD `ca1696c`)

## Context & Status

A critical review of the benchmark suite and manuscript found that LongMemEval
judge failures were being scored as wrong answers for the system under test, at
24 to 47 rows per system across nine systems. The re-judge is **complete and
clean**: nine phases, zero judge failures, zero truncations across 4,500 rows.

The numbers exist and are recorded. Binding them into the manuscript is blocked
on rebuilding the evidence collection, which is a larger job than the plan
assumed. See Blockers.

Review: `tasks/reviews/2026-07-24_benchmark-manuscript-consistency-review.md`.
Plan: `tasks/2026-07-24-longmemeval-rejudge-plan.md`.
Progress log: `tasks/todo.md`.

## Technical Implementation

### Root cause, reproduced

The pinned Gemma 4 31B judge runs on llama.cpp and returns reasoning in
`reasoning_content`, the verdict in `content`. `answering.call_model` reads only
`content`. Two separate caps produced empty verdicts:

1. `max_output_tokens: 256` in the v1 judge identity.
2. The server's per-slot context. `finish_reason: "length"` fires when
   prompt plus completion reaches `n_ctx`, not `max_output_tokens`. The judge
   container ran `--ctx-size 8192 --parallel 4`, giving 2,048 per slot. One memd
   row reached 208 + 1840 = 2048 exactly, and requesting 3072, 4096 or 6144
   changed nothing.

### Work completed

- `benchmarks/longmemeval/judge_client.py` (new): fail-closed judge client.
  Raises `JudgeTruncated` on `finish_reason == "length"` and on an empty verdict
  with populated reasoning. Reports the endpoint's per-slot context best-effort.
  Additive because `answering.py` is pinned by digest in every frozen QA
  manifest.
- `benchmarks/longmemeval/judge.py`: uses the new client; aborts on **any**
  judge-side failure; counts answer-side and judge-side failures separately;
  records the serving context and any inherited-subject divergence.
- `benchmarks/config/judge-model.gemma4-31b.v3.json` (new): 4,096 output tokens,
  600 s timeout.
- `benchmarks/longmemeval/judge-protocol.v2.json` (new): supersedes the v1 judge
  protocol and records the new failure contract.
- `benchmarks/restore_run_dirs.py` (new): restores a frozen phase closure out of
  a verified bundle without touching tracked files.
- `manuscript/render_claims.py` (new): shared region substitution, verified-claim
  guard and staged audit. `render_memorydata.py` delegates to it.
- `manuscript/render_longmemeval.py` (new): three bound evidence regions.
- `manuscript/check_assertions.py`, `claims.v1.json`, `MANUSCRIPT.md`: registered
  the `internal_validation` evidence class, relabelled the three longitudinal
  claims, renamed `failed_promotion_gate` to `promotion_gate`, removed a
  note-to-self from Data and code availability.
- memd repo: `README.md` and `docs/comparison.md` stale LoCoMo table and lead
  claims replaced with bundle-bound lane numbers; `docs/benchmarking.md`
  evidence status rewritten (all seven longitudinal gates pass); stale tracked
  `benchmarks/locomo/` fork deleted and recorded as retired in
  `evals/bench/BENCHMARK_INVENTORY.md`.

### Results

Judged on a new container from the pinned image
`ghcr.io/ggml-org/llama.cpp@sha256:54421a9c76f8ab7c7a8aa8f8c13fec764e30a574edc4b6b11213bd1fb0ccfb65`,
`--parallel 4 --ctx-size 16384` (4,096 per slot), GPU 2, host port 9911.
39.6 rows/min, 1.9 h.

| System | v17 accuracy | 95% CI | v1 | delta | rec@20 |
|---|---:|---|---:|---:|---:|
| SuperLocalMemory | 36.4 | [33.0, 39.8] | 33.8 | +2.6 | 97.26 |
| BM25 control | 35.0 | [31.6, 38.4] | 33.4 | +1.6 | 97.69 |
| memd | 34.8 | [31.2, 38.4] | 32.2 | +2.6 | 98.87 |
| LightMem | 31.6 | [28.0, 35.4] | 26.6 | +5.0 | - |
| Dense control | 31.2 | [27.8, 34.6] | 29.4 | +1.8 | 97.88 |
| MemOS | 31.2 | [27.6, 34.6] | 29.2 | +2.0 | 97.65 |
| Mem0 | 18.8 | [15.6, 22.0] | 16.0 | +2.8 | 71.67 |
| Recency control | 7.8 | [5.6, 10.2] | 7.0 | +0.8 | 49.31 |
| Graphiti | 0.0 | - | - | n/a | 0.00 |

Ordering changed: LightMem sixth to fourth, Dense and MemOS now tied, and the
memd-to-BM25 gap narrowed from 1.2 to 0.2 points. The review's conclusion
sharpens rather than reverses: memd leads evidence recall at 98.87 and places
third on answer accuracy, indistinguishable from a plain BM25 control.

### Outcomes

- **What worked:** the fail-closed gate caught its own inadequacy twice. It
  aborted memd's phase on a residual truncation, then a generalized version
  caught 19 `TimeoutError` rows that the truncation-only version let through.
  Both would otherwise have been scored as wrong answers.
- **What didn't:** three planning estimates were wrong. Throughput was 16
  rows/min not 35; the restore closure was 40 artifacts across 13 run
  directories not 9; and `reference_adapter.py` is the pinned `subject.binary`
  for three lanes and had since been refactored, blocking their re-judge until
  inherited-subject divergence was tolerated and recorded.
- **Retracted:** review finding R1 was wrong. The suite runs from the locked
  environment with `uv run --locked python -m unittest discover -s
  benchmarks/tests` (238 tests, OK), which `reproduce.sh:96` already invokes.
  Do not add pytest to `uv.lock`.

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Fix additively in `judge_client.py` rather than editing `answering.py` | `answering.py` is pinned by digest in every frozen QA manifest; editing it makes those phases un-rerunnable. |
| Tolerate divergence only in inherited subject artifacts, and record it | A judge phase reads frozen answers and never executes the subject. A since-refactored adapter is a provenance fact, not a reason to refuse to score. |
| Abort on any judge-side failure, not only truncation | Timeout and unparseable verdict charge the same infrastructure failure to the system under test. |
| Raise the client timeout rather than reduce concurrency | `timeout_seconds` lives in `runtime`, not the model identity, so it preserves cross-system comparability while the config artifact still records it. |
| Start a new container from the pinned image rather than reuse port 8015 | Port 8015 is a different container. Judging there while recording `container_digest: sha256:54421a9c…` would have been false provenance. |
| Record the serving context in the phase manifest | Two endpoints with identical recorded identity produced different token sequences for the same request. |
| Do not hand-assemble a collection that holds two judge phases per system | `render_longmemeval` fails closed on that ambiguity, correctly. |

## Knowledge Capture

### Lessons learned

Recorded in `tasks/lessons.md`:

- A phase manifest is bound to the tree that produced it. Decide where a re-run
  must execute before starting it.
- `subject.binary` for a pure-Python competitor lane is a source file that keeps
  evolving; pinning it makes frozen phases un-rerunnable.
- A `memd add` that fails with "may still complete in the worker" can still have
  committed; verify with `memd search --warm off` before retrying.

### Gotchas

- The pinned judge model file `gemma-4-31B-it-Q4_K_M.gguf` is **no longer
  downloadable** from `ggml-org/gemma-4-31B-it-GGUF`; upstream now serves only
  Q4_0, Q8_0 and BF16. The local copy at revision
  `fb5801c702a472691c6eba168f28af79a076fbe9` is the only source. This belongs in
  the manuscript limitations and is a live reproducibility hazard.
- `--parallel N` divides `--ctx-size` by N. A server advertising 8,192 serves
  2,048 per slot.
- The memd working tree carries **someone else's staged v1.5.1 work**
  (`CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`,
  `crates/memd/src/cli/consolidate.rs`, `crates/memd/src/cli/session_start.rs`).
  It was staged before this session's doc edits and was deliberately left in the
  index. Do not sweep it into a commit.
- `pkill -f 'codex exec'` matches the shell running it. Use a bracketed pattern.

## Moving Forward

### Next steps

1. **Decide where the re-judge must execute, then rebuild the collection.** The
   v17 phases were produced at the memd-bench root. Collection
   `aa1ab03a4a82bb0f` draws six workspaces from separate nested clean-clones
   under `run-output/clean-clone-v9` through `v14`, each pinning a different memd
   binary at the same relative path and each still holding the superseded v1
   judge phases. Either re-run the judge inside those clones (they predate
   `judge_client.py`, so the fix must be applied there first), or add a supported
   way to replace a phase inside a verified collection. `bundle_artifacts.py`
   only builds or verifies today.
2. Bind `render_longmemeval.py write` against the new collection, then rewrite
   Section 5.4 to state the ordering and name the BM25, Dense and Recency
   controls (review finding B3). The renderer is tested against a synthetic
   bundle but has never seen real artifacts.
3. Re-point `manuscript/notebooks/02_longmemeval_analysis.ipynb` at the v17
   phases, add the answer-side and judge-side failure columns, and regenerate
   figures. Execute twice and diff the summaries.
4. Add the manuscript limitation about the unfetchable judge model file, and
   move the content-deduplication limitation into Methods with its measured
   numbers (review finding C2).
5. Fix the abstract's "recall@3 from 0.9766 to 0.9766" wording (C3) and report
   the LoCoMo retrieval ablation as bound claims (C4). Both need a renderer pass.

### Blockers

- **Evidence collection rebuild**, as described in step 1. Everything
  downstream of it (notebooks, claim binding, Section 5.4) waits on it.
- Nothing is pushed. memd-bench commits `b9af8ae`, `2ae7aa2`, `6842788`,
  `69b6a8b`, `ca1696c` are local. memd doc changes are unstaged and uncommitted.
  Both need explicit approval.
- The judge container `memd-bench-gemma4-31b-judge-ctx16k` is still running on
  GPU 2 and holds about 21 GB. Stop it with
  `docker rm -f memd-bench-gemma4-31b-judge-ctx16k` when the re-judge work is
  finished.
