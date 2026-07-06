# LoCoMo benchmark harness (repo-local, fetch-on-run)

Measures `memd` retrieval quality and answer-usefulness on the LoCoMo
long-conversation dataset. Self-contained, stdlib-only Python; all memd
processes run hermetically (repo-local HOME, cache, and store — see
`common.py`). Downloads and run outputs live under gitignored
`benchmark-data/` and `run-output/`.

## Dataset

- `locomo10.json` from the upstream LoCoMo repository
  (https://github.com/snap-research/locomo), pinned by SHA256 in `common.py`.
  Note: some older links point at `snap-stanford`, which 404s.
- 10 conversations, 5,882 turns, 1,986 QA pairs. Categories: 1 multi-hop,
  2 temporal, 3 open-domain, 4 single-hop, 5 adversarial (excluded from
  headline metrics, matching `docs/benchmarking.md`).

Fetch:

```bash
curl -fsSL -o benchmark-data/locomo10.json \
  https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json
```

## Protocol

Seeding (`seed.py`): one tenant `locomo`, one project per conversation
(`conv-<sample_id>`), one chunk per turn (`chunk_type=message`), via
`memd batch` (one process per conversation). Turn text formats:

- `plain`: `<speaker>: <text>` (image turns append `[shares a photo:
  <blip_caption>]`)
- `dated`: same, prefixed `[<session datetime>]`

The original (sanitized-away) harness's exact turn format is unknown; the
baseline run tests both formats and adopts the one that reproduces the
documented numbers (`docs/benchmarking.md`: MRR@10 0.412). A seed-time
`chunk_to_dia.json` mapping ties chunk ids back to LoCoMo `dia_id`s so
scoring never depends on search-payload metadata.

Retrieval eval (`eval_retrieval.py`): for each category 1-4 question, one
`memory.search` (k=10, default mode), all questions of a conversation
streamed through one `memd batch` process. `--scope project` (primary
protocol) scopes each search to the question's conversation; `--scope
global` searches tenant-wide across all 5,882 turns (closest reconstruction
of the documented protocol: reproduced MRR@10 0.429 global vs 0.4511
project vs 0.412 documented). Reports MRR@10, Hit@1/3/10, per-category
MRR@10/Hit@10, and latency (mean/p50/p95 preferring memd's per-request
`elapsed_ms` from batch rows; each conversation's first search carries
index-load warmup and is reported separately). Questions with no valid
evidence `dia_id` are excluded and logged in the results file.

## Metrics

Answer metrics mirror upstream
(`task_eval/evaluation.py` in snap-research/locomo): normalize (strip commas,
lowercase, strip punctuation, drop articles a/an/the/and, collapse
whitespace); F1 over Porter-stemmed tokens; EM as normalized token-set
equality. SubEM is not defined upstream; here: normalized gold is a
substring of the normalized prediction. The Porter stemmer is the classic
1980 algorithm (self-contained `porter.py`, self-tested); nltk's default
mode adds small extensions, so absolute F1 may differ marginally from other
harnesses — all comparisons inside this harness are internally consistent.

## Usage

```bash
# seed + retrieval eval (baseline)
python3 benchmarks/locomo/seed.py run-output/base-plain --fmt plain
python3 benchmarks/locomo/eval_retrieval.py run-output/base-plain --label baseline-plain
```

Every run directory gets a `manifest.json` (dataset URL + SHA256, git
commit, Cargo.lock SHA256, memd version, configuration, timings).
