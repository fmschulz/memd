#!/usr/bin/env python3
"""Objective gold-phrase matching for the v2 benchmark.

Each (agent, condition, question) cell is scored on:
  - gold_hit: 1 if the final answer contains the question's gold_phrase
    (case-insensitive substring), else 0.
  - secondary_hits: count of `gold_secondary` phrases present
    (out of len(gold_secondary)).
  - nf_honest: 1 if the response explicitly says "not found" without
    claiming the gold_phrase, else 0. Used to measure honesty on the
    hard questions.

Writes `results/gold_scores.json` and prints a markdown summary
including per-category breakdown and McNemar's test on the paired
binary gold_hit outcomes.
"""
import json
import math
import pathlib
from collections import defaultdict

BENCH = pathlib.Path(__file__).resolve().parent.parent
FINAL = BENCH / "results" / "final"
PROMPTS = json.load(open(BENCH / "questions" / "prompts.json"))["questions"]
GOLD = json.load(open(pathlib.Path.home() / ".local/share/memd-private-artifacts-z7k/gold_answers.json"))["answers"]
QUESTIONS = []
for q in PROMPTS:
    g = GOLD.get(q["id"], {})
    QUESTIONS.append({**q, "gold_phrase": g.get("gold_phrase", ""), "gold_secondary": g.get("gold_secondary", [])})


def score_one(answer: str, gold_phrase: str, secondary: list[str]) -> dict:
    low = answer.lower()
    gp = gold_phrase.lower()
    hit = int(gp in low)
    shits = sum(1 for s in secondary if s.lower() in low)
    nf = int("not found" in low and not hit)
    return {"gold_hit": hit, "secondary_hits": shits,
            "secondary_total": len(secondary), "nf_honest": nf,
            "chars": len(answer)}


def mcnemar(b: int, c: int) -> float:
    """Exact McNemar's test (two-sided) p-value for paired binary data.

    b = pairs where with-memd correct, without-memd wrong
    c = pairs where without-memd correct, with-memd wrong
    Uses exact binomial if b+c small, otherwise continuity-corrected chi-squared.
    """
    n = b + c
    if n == 0:
        return 1.0
    if n < 25:
        # Two-sided exact: sum of P(X=k) for k <= min(b,c) on Binomial(n, 0.5),
        # doubled, capped at 1.0.
        k = min(b, c)
        total = 0.0
        for i in range(k + 1):
            total += math.comb(n, i) * (0.5 ** n)
        return min(1.0, 2 * total)
    stat = (abs(b - c) - 1) ** 2 / n
    # chi-sq 1 dof survival = erfc(sqrt(stat/2))
    return math.erfc(math.sqrt(stat / 2))


rows = []
for q in QUESTIONS:
    for agent in ("claude", "codex"):
        for cond in ("with", "without"):
            path = FINAL / f"{agent}__{cond}__{q['id']}.txt"
            if not path.exists():
                continue
            answer = path.read_text(errors="replace")
            s = score_one(answer, q["gold_phrase"], q.get("gold_secondary", []))
            rows.append({"qid": q["id"], "category": q["category"],
                         "agent": agent, "cond": cond, **s})

by_cond = defaultdict(lambda: defaultdict(list))
for r in rows:
    for k in ("gold_hit", "secondary_hits", "secondary_total",
              "nf_honest", "chars"):
        by_cond[r["cond"]][k].append(r[k])

by_cat = defaultdict(lambda: defaultdict(lambda: [0, 0]))  # [hits, total]
for r in rows:
    by_cat[r["category"]][r["cond"]][0] += r["gold_hit"]
    by_cat[r["category"]][r["cond"]][1] += 1

# Paired outcome for McNemar's, per (agent, question).
paired = defaultdict(lambda: [0, 0, 0, 0])  # [b=w-w0, c=w0-w, both, neither]
for q in QUESTIONS:
    for agent in ("claude", "codex"):
        w = next((r for r in rows if r["qid"] == q["id"] and r["agent"] == agent and r["cond"] == "with"), None)
        wo = next((r for r in rows if r["qid"] == q["id"] and r["agent"] == agent and r["cond"] == "without"), None)
        if not w or not wo:
            continue
        if w["gold_hit"] and not wo["gold_hit"]:
            paired["all"][0] += 1
            paired[agent][0] += 1
        elif wo["gold_hit"] and not w["gold_hit"]:
            paired["all"][1] += 1
            paired[agent][1] += 1
        elif w["gold_hit"] and wo["gold_hit"]:
            paired["all"][2] += 1
            paired[agent][2] += 1
        else:
            paired["all"][3] += 1
            paired[agent][3] += 1

# Write raw + summary
(BENCH / "results" / "gold_scores.json").write_text(json.dumps(rows, indent=2))

lines = ["# v2 benchmark — gold-fact scoring\n"]
lines.append(f"**Questions:** {len(QUESTIONS)}  •  **Runs:** {len(rows)}\n")
lines.append("## Gold-phrase recall (primary metric)\n")
lines.append("| condition | hits | total | recall |")
lines.append("|---|---|---|---|")
for cond in ("with", "without"):
    hits = sum(by_cond[cond]["gold_hit"])
    total = len(by_cond[cond]["gold_hit"])
    r = hits / total if total else 0.0
    lines.append(f"| {cond}-memd | {hits} | {total} | {r:.2%} |")
lines.append("")

lines.append("## Secondary-fact recall\n")
lines.append("| condition | secondary hits | max possible | recall |")
lines.append("|---|---|---|---|")
for cond in ("with", "without"):
    sh = sum(by_cond[cond]["secondary_hits"])
    st = sum(by_cond[cond]["secondary_total"])
    r = sh / st if st else 0.0
    lines.append(f"| {cond}-memd | {sh} | {st} | {r:.2%} |")
lines.append("")

lines.append("## Per-category recall (gold-phrase)\n")
lines.append("| category | with-memd | without-memd |")
lines.append("|---|---|---|")
for cat in sorted(by_cat):
    w = by_cat[cat]["with"]
    wo = by_cat[cat]["without"]
    lines.append(
        f"| {cat} | {w[0]}/{w[1]} ({(w[0]/w[1] if w[1] else 0):.0%}) | "
        f"{wo[0]}/{wo[1]} ({(wo[0]/wo[1] if wo[1] else 0):.0%}) |"
    )
lines.append("")

lines.append("## Paired outcomes + McNemar's test\n")
lines.append("b = with-memd correct AND without-memd wrong")
lines.append("c = without-memd correct AND with-memd wrong\n")
lines.append("| split | b | c | both | neither | McNemar p (two-sided) |")
lines.append("|---|---|---|---|---|---|")
for name in ("all", "claude", "codex"):
    b, c, both, neither = paired[name]
    p = mcnemar(b, c)
    lines.append(f"| {name} | {b} | {c} | {both} | {neither} | {p:.4f} |")
lines.append("")

lines.append("## Honest 'not found' rate (lower-bounds hallucination)\n")
lines.append("| condition | nf_honest | total | rate |")
lines.append("|---|---|---|---|")
for cond in ("with", "without"):
    nf = sum(by_cond[cond]["nf_honest"])
    t = len(by_cond[cond]["nf_honest"])
    lines.append(f"| {cond}-memd | {nf} | {t} | {nf/t if t else 0:.2%} |")
lines.append("")

lines.append("## Per-run detail\n")
lines.append("| qid | agent | cond | gold_hit | sec | chars | nf |")
lines.append("|---|---|---|---|---|---|---|")
for r in sorted(rows, key=lambda x: (x["qid"], x["agent"], x["cond"])):
    lines.append(
        f"| {r['qid']} | {r['agent']} | {r['cond']} | {r['gold_hit']} | "
        f"{r['secondary_hits']}/{r['secondary_total']} | {r['chars']} | {r['nf_honest']} |"
    )

(BENCH / "results" / "summary.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
