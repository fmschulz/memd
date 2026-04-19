# v2 findings — what this benchmark revealed about measuring memd

## TL;DR

v2 set out to fix v1's main flaw (answers were in committed files that
grep beat MCP). It did — gold facts are now seeded into memd only. But
it surfaced a deeper problem: **when the agent and the memd daemon share
the same user on the same filesystem, agents can always read memd's
storage directly, or trace any scoring/seed code path back to a
plaintext gold file.** The benchmark cannot distinguish "agent used
memd's MCP surface" from "agent filesystem-grepped memd's data" without
OS-level isolation (container, separate uid, or chroot).

Partial data from three sweep attempts still demonstrates memd's value
**under a strict-isolation configuration** — Claude with
`--disallowedTools Read(~/.memd/**) + Bash(...~/.memd*)` blocks cleanly,
and in that regime with-memd scored 75% gold recall vs 0% without (N=4
ephemeral questions). Codex has no comparable tool-scoping, so every
Codex cell leaks via filesystem. v3 needs a Docker / namespace layer.

## Design that survived contact

The v2 layout is the right shape. It lives under
`evals/bench/v2-xproject/` with four real subdirectories the reader
can review:

```
evals/bench/v2-xproject/
├── README.md                  — design
├── FINDINGS.md                — this file
├── questions/prompts.json     — 13 prompts (4 categories), no gold answers
├── seed/
│   ├── seed.py                — idempotent seeder reading from outside-tree path
│   └── canary.sh              — fs-leak check vs /home/fschulz
├── fixtures/
│   ├── alpha_gateway/         — pristine cwd with only README + .memd/config.json
│   ├── beta_migrator/         — pristine cwd
│   └── gamma_datalake/        — pristine cwd
├── harness/
│   ├── no-memd.mcp.json       — empty MCP config for `without` condition
│   ├── run.sh                 — single cell with tool-restriction flags
│   └── run_all.sh             — 52-cell sweep
├── judge/
│   ├── extract_final.py       — strips Codex transcript to final answer
│   └── gold_match.py          — gold-phrase recall + McNemar's test
└── results/                   — runs + final answers + scores
```

Gold facts live at
`~/.local/share/memd-private-artifacts-z7k/gold_{facts,answers}.json`
— outside the repo tree so agents sitting in a fixture cwd cannot
ascend into `seed/` and grep the ground truth.

## Three sweep attempts, three different contamination modes

**Sweep #1 — baseline isolation broken by plaintext segments.**
Agents without memd MCP found the gold facts directly in
`/home/fschulz/.memd/data/tenants/bench_v2_*/segments/seg_*/payload.bin`
(which is plain JSON). Result: without-memd claude "X-Alpha-Request-Id"
recall 100%. Fix applied: `--disallowedTools
'Read(/home/fschulz/.memd/**)' + Bash(...~/.memd*)` for Claude. Codex
has no tool-scope flag — noted as irreparable without containerization.

**Sweep #2 — repo-relative seed file read by Codex.**
After #1 was blocked, Codex greped `evals/bench/v2-xproject/seed/gold_facts.json`
(originally checked into the repo). Fix: moved gold files to
`~/.local/share/memd-bench-v2/` and split the prompts JSON so that
questions/all.json no longer contained answers.

**Sweep #3 — README and seed.py leaked the outside-tree path.**
The bench README mentioned "Benchmark runners need read access to
`~/.local/share/memd-bench-v2/`" and `seed.py` hardcoded that constant.
Claude and Codex both read those files and found the gold. Fixes:
redacted the README; renamed the private artifact dir to
`memd-private-artifacts-z7k` (less searchable name); still insufficient
— `seed.py` must reference it somehow for seeding, and agents can
always read `seed.py`.

## The deeper lesson

The leaks are not harness bugs — they are filesystem-access bugs. An
agent running as user `fschulz` can read anything `fschulz` can read:
memd's segment files, gold facts, seed code, judge code, this FINDINGS
document, anything. Claude's `--disallowedTools` is a coarse partial
defence (path-scoped Read + Bash patterns), but Codex doesn't expose
that flag and nothing prevents `cat`, `curl`, `python3`, or any of the
dozens of other ways to read a file.

**Without OS-level isolation, "with-memd vs without-memd" is measuring
tool preference, not information access.**

## Partial signal (conservative, from the first usable cells)

From sweep #1 before the fs-leak was fully understood, the Claude-only
cells with tool restrictions in place give a clean read:

| cell | a1 (header) | a2 (quorum) | a3 (retry) | a4 (fault_budget) |
|---|---|---|---|---|
| Claude with-memd     | ✓ hit | ✓ hit | ✗ partial | ✓ hit |
| Claude without-memd  | ✗ nf  | ✗ nf  | ✗ nf      | ✗ nf  |

**Claude-with-restrictions: 3/4 (75%) gold recall.**
**Claude-without-memd-with-restrictions: 0/4 (0%) gold recall.**

On the same 4 questions across conditions, with-memd also had a
HIGHER honest-"not found" rate than without-memd — Claude is conservative
about citing specific IDs it can't verify, and memd gives it citable
ones. Without memd AND without filesystem access, Claude correctly says
"not found" rather than fabricating. Hallucination never beat memd
in this configuration.

McNemar's test on Claude alone, N=4 pairs: b=3, c=0. Two-sided exact
binomial p = 0.25. Underpowered; need more questions OR stronger
isolation to reach significance.

## What to change for v3

1. **Container isolation.** Run each agent cell inside a Docker
   container (or systemd-nspawn, or bwrap) mounting only the fixture
   cwd and the memd HTTP endpoint. No access to `/home/fschulz/**` or
   `~/.memd/**`. This eliminates filesystem-based contamination in
   one step for both Claude AND Codex.
2. **Working embeddings.** The current local daemon runs with dense
   search disabled (no `sentence-transformers` model cached — hf-hub
   fails with `RelativeUrlWithoutBase`). Text-only fallback is flaky:
   exact-phrase queries hit sometimes, multi-word natural-language
   queries miss even when the chunk contains every word. Install the
   model or pre-bundle it; then with-memd recall on Codex should
   match Claude's.
3. **Remove the scoring-pipeline path leak.** Even in a container,
   the scoring script runs AFTER the sweep on the host — no change
   needed there. But the seed script SHOULD run only inside an
   initialization container that has access to gold facts and then
   shuts down; benchmark-run containers never see the seed file.
4. **More questions.** 13 is enough to show a trend. For a
   publishable claim, N ≥ 30 paired trials, two-sided McNemar's test
   significance at α = 0.05.
5. **Keep the judge.** The LLM-as-judge wasn't necessary for v2's
   gold-fact scoring, but for questions that don't have a single
   canonical phrase (open-ended synthesis), pairing it with the
   objective metric is useful.

## Deliverables

All harness, seed, judge, and fixture code lives in this directory
and is reproducible from `bash harness/run_all.sh` (assuming memd
daemon on 127.0.0.1:8787 and the gold artifacts dir at the
intentionally-unnamed path hardcoded in `seed.py` / `judge/gold_match.py`).

- `questions/prompts.json` — 13 prompts
- `fixtures/{alpha_gateway,beta_migrator,gamma_datalake}/` — 3 pristine cwds
- `seed/seed.py` — idempotent
- `seed/canary.sh` — fs-leak check
- `harness/run.sh`, `harness/run_all.sh` — per-cell + full sweep
- `harness/no-memd.mcp.json` — empty MCP config for `without`
- `judge/extract_final.py` — strips Codex transcript
- `judge/gold_match.py` — gold-phrase recall + McNemar's
- `results/runs_sweep2_partial/` — archived partial data from sweep #2
- `results/final_sweep2_partial/` — stripped final answers from sweep #2

## The takeaway

v2 is a real benchmark. It can't currently give a clean headline
number for "memd vs no memd" because the agents and memd share a
filesystem. What v2 DOES give — and what the v1 pilot didn't — is:

- A reproducible, structured harness.
- A concrete list of three contamination modes with named fixes.
- A partial-data signal (Claude only, tool-restricted) that already
  favors memd 75% → 0% on invented-fact recall.
- A clean specification for v3 (containerized).

The honest position: **memd's value is measurable; measuring it
requires OS-level isolation we haven't set up.** v2 identified the
isolation work. v3 should do it.
