# memd cross-project benchmark — v2

## What v1 got wrong

v1 (`evals/bench/memd-xproject-pilot/`) asked questions whose answers were
already in the memd repo's `docs/handoffs/`, `tasks/lessons.md`, and
`CHANGELOG.md`. Any agent with filesystem access could grep them, so the
`without-memd` condition tied or beat `with-memd` on aggregate scores. The
measurement captured documentation quality, not memory quality.

## v2 design

Every question in v2 has an answer that is **inside memd and not in any
reachable file** on the test machine. Three mechanisms enforce this:

1. **Seeded gold facts.** `seed/seed.py` writes specific text into
   dedicated bench tenants (`bench_v2_alpha`, `bench_v2_beta`,
   `bench_v2_gamma`) before the run. The gold content is composed of
   invented identifiers (e.g. `X-Alpha-Request-Id`, `fault_budget:0.037`)
   that do not appear in any repo, config, or doc on disk.
2. **Pristine fixture cwds.** `fixtures/{alpha_gateway,beta_migrator,gamma_datalake}/`
   each contain only `README.md` and a `.memd/config.json` pointing at
   the matching bench tenant. No handoffs, no lessons, no code that
   would grep-hit the gold facts.
3. **Grep canary.** Before scoring, we verify that
   `rg <gold_phrase> /home/fschulz` returns zero matches outside of
   `evals/bench/v2-xproject/` — if a fact leaks into the filesystem,
   the question is removed from the run before scoring.

## Question categories (4)

| Category | What memd is being tested on | N |
|---|---|---|
| `ephemeral` | Canonical decisions stored as memd chunks that never made it into any file | 8 |
| `cross_project` | From cwd of project A, answer requires searching project B's tenant | 6 |
| `quantitative` | Aggregate over historical `task_run_*` artifacts (count, max, latest) | 4 |
| `trust` | Only verified/ grounded artifacts should be cited (`verification_status=verified`, `artifact.verify`) | 3 |

Total: **21 questions × 2 agents × 2 conditions = 84 runs.**

## Directory layout

```
evals/bench/v2-xproject/
├── README.md                     — this file
├── FINDINGS.md                   — written after the run
├── questions/
│   ├── ephemeral.json            — 8 questions keyed to seeded chunk IDs
│   ├── cross_project.json        — 6 questions spanning tenants
│   ├── quantitative.json         — 4 questions over task-run aggregates
│   └── trust.json                — 3 questions exercising artifact.verify
├── seed/
│   ├── gold_facts.json           — ground truth (fact_id → tenant, chunk payload, gold_phrase)
│   ├── seed.py                   — populates memd with the gold facts
│   └── canary.sh                 — fs leak check; fails closed if gold_phrase leaks
├── fixtures/
│   ├── alpha_gateway/            — pristine cwd with .memd/config.json for tenant bench_v2_alpha
│   ├── beta_migrator/            — pristine cwd for tenant bench_v2_beta
│   └── gamma_datalake/           — pristine cwd for tenant bench_v2_gamma
├── harness/
│   ├── no-memd.mcp.json          — empty MCP config for the `without` condition
│   ├── run.sh                    — one cell: <agent> <condition> <cwd> <question_id>
│   └── run_all.sh                — sweeps every cell
├── judge/
│   ├── gold_match.py             — objective: does the response contain the gold_phrase?
│   └── judge_pairs.py            — blind LLM pairwise (backup signal)
└── results/
    ├── runs/                     — raw agent stdout
    ├── final/                    — codex transcripts stripped to final-answer
    ├── judged/                   — per-pair verdicts
    ├── gold_scores.json          — objective scores
    └── summary.md                — aggregate report
```

## Scoring

**Primary metric — gold-phrase precision/recall:**

- Did the response contain the literal `gold_phrase`?
- For questions that expect multiple facts, Recall@top-response.
- Hallucination rate: count of response-claimed identifiers (via regex
  patterns) that are NOT in memd and NOT in any file on disk.

**Secondary metric — blind LLM pairwise judge** on grounding,
correctness, specificity, actionability, as in v1.

**Statistical test — McNemar's test on paired gold-hit binary outcomes**
per (question, agent). Significance threshold: p < 0.05.

## Expected outcome

If memd is doing what it claims, `with-memd` gold-phrase recall should
be ≥ 0.8 while `without-memd` should be ≤ 0.1 (because the gold facts
literally are not in any file). A tight, publishable result.

## Reproducibility

1. `python3 seed/seed.py` — writes gold facts to memd (idempotent by
   chunk_id).
2. `bash seed/canary.sh` — verifies no gold_phrase leaks into the
   filesystem outside this bench dir.
3. `bash harness/run_all.sh` — sweeps 84 runs (~90 min wall clock).
4. `python3 judge/gold_match.py` — objective gold-fact scoring.
5. `python3 judge/judge_pairs.py` — LLM pairwise (optional, ~30 min).
6. `python3 judge/summary.py` — writes `results/summary.md` and
   `FINDINGS.md`.
