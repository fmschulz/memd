"""Retrieval evaluation: MRR@10, Hit@1/3/10, per-category, latency.

Protocol (mirrors docs/benchmarking.md): categories 1-4, search scoped to the
question's conversation project, k=10, default search mode. Gold = LoCoMo
evidence dia_ids. Questions whose evidence list is empty or references
dia_ids absent from the seeded mapping are excluded and logged.

All questions for all conversations run through ONE `memd batch` process per
conversation store; per-query latency comes from stdout row timestamp deltas
(row 1 of each conversation carries store-open cost and is excluded from
percentiles, reported separately).

Usage: python3 eval_retrieval.py <run_dir> [--k 10] [--label baseline]
"""

import json
import statistics
import sys
from pathlib import Path

import common
import metrics


def evaluate(run_dir: Path, k: int, label: str, scope: str = "project"):
    data = common.load_dataset()
    store_dir = run_dir / "store"
    mapping = json.loads((run_dir / "chunk_to_dia.json").read_text())

    per_question = []
    excluded = []
    latencies = []
    first_row_latencies = []

    for conv in data:
        sample_id = conv["sample_id"]
        project = f"conv-{sample_id}"
        # dia ids are unique per conversation only; build the per-conversation set
        conv_dias = set()
        for _key, _dt, turn in common.iter_turns(conv["conversation"]):
            conv_dias.add(turn["dia_id"])

        questions = []
        for qa in conv["qa"]:
            cat = qa.get("category")
            if cat not in (1, 2, 3, 4):
                continue
            evidence = [e for e in (qa.get("evidence") or []) if isinstance(e, str)]
            valid = [e for e in evidence if e in conv_dias]
            if not valid:
                excluded.append(
                    {
                        "conversation": sample_id,
                        "question": qa.get("question"),
                        "category": cat,
                        "evidence": evidence,
                        "reason": "no valid evidence dia_ids in conversation",
                    }
                )
                continue
            questions.append((qa, valid))

        requests = []
        for qa, _valid in questions:
            arguments = {
                "tenant_id": common.TENANT,
                "query": str(qa.get("question") or ""),
                "k": k,
            }
            if scope == "project":
                arguments["project_id"] = project
            requests.append({"tool": "memory.search", "arguments": arguments})
        if not requests:
            continue
        rows, row_times = common.run_batch(requests, store_dir)
        if len(rows) != len(requests):
            sys.exit(f"{project}: {len(rows)} responses for {len(requests)} searches")

        prev_t = None
        for idx, ((qa, valid), row, t) in enumerate(zip(questions, rows, row_times)):
            if not row.get("ok"):
                sys.exit(f"{project}: search failed: {json.dumps(row)[:400]}")
            result = row.get("result") or {}
            results = result.get("results") or []
            ranked_dias = []
            for r in results:
                cid = r.get("chunk_id")
                dia = mapping.get(cid)
                if dia is not None:
                    ranked_dias.append(dia)
            # Prefer memd's own per-request timing (batch rows report
            # elapsed_ms); stdout stream deltas are meaningless because the
            # batch runner buffers all output until the end.
            elapsed_ms = row.get("elapsed_ms")
            lat = (elapsed_ms / 1000.0) if elapsed_ms is not None else (
                (t - prev_t) if prev_t is not None else t
            )
            if idx == 0:
                first_row_latencies.append(lat)
            else:
                latencies.append(lat)
            prev_t = t
            per_question.append(
                {
                    "conversation": sample_id,
                    "category": qa.get("category"),
                    "question": qa.get("question"),
                    "gold": valid,
                    "ranked": ranked_dias,
                    "rr": metrics.reciprocal_rank(ranked_dias, valid, k),
                    "hit1": metrics.hit_at(ranked_dias, valid, 1),
                    "hit3": metrics.hit_at(ranked_dias, valid, 3),
                    "hit10": metrics.hit_at(ranked_dias, valid, 10),
                }
            )
        print(f"{project}: {len(questions)} questions evaluated")

    n = len(per_question)
    summary = {
        "label": label,
        "scope": scope,
        "k": k,
        "questions": n,
        "excluded": len(excluded),
        "mrr@10": round(sum(q["rr"] for q in per_question) / n, 4),
        "hit@1": round(sum(q["hit1"] for q in per_question) / n, 4),
        "hit@3": round(sum(q["hit3"] for q in per_question) / n, 4),
        "hit@10": round(sum(q["hit10"] for q in per_question) / n, 4),
        "per_category": {},
        "latency_ms": {},
    }
    for cat in (1, 2, 3, 4):
        qs = [q for q in per_question if q["category"] == cat]
        if qs:
            summary["per_category"][str(cat)] = {
                "n": len(qs),
                "mrr@10": round(sum(q["rr"] for q in qs) / len(qs), 4),
                "hit@10": round(sum(q["hit10"] for q in qs) / len(qs), 4),
            }
    lat_sorted = sorted(latencies)
    if lat_sorted:
        summary["latency_ms"] = {
            "n": len(lat_sorted),
            "mean": round(1000 * statistics.mean(lat_sorted), 2),
            "p50": round(1000 * metrics.percentile(lat_sorted, 50), 2),
            "p95": round(1000 * metrics.percentile(lat_sorted, 95), 2),
            "first_row_mean": round(
                1000 * statistics.mean(first_row_latencies), 2
            ),
        }

    out = run_dir / f"retrieval_{label}.json"
    out.write_text(
        json.dumps(
            {"summary": summary, "excluded": excluded, "per_question": per_question},
            indent=1,
        )
    )
    seed_meta = {}
    seed_meta_path = run_dir / "seed_meta.json"
    if seed_meta_path.exists():
        seed_meta = json.loads(seed_meta_path.read_text())
    common.write_manifest(
        run_dir,
        {
            "kind": "retrieval_eval",
            "label": label,
            "k": k,
            "seed": seed_meta,
            "memd_version": common.memd_version(store_dir),
        },
    )
    print(json.dumps(summary, indent=2))
    print(f"written: {out}")
    return summary


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    run_dir = Path(sys.argv[1]).resolve()
    k = 10
    label = "baseline"
    scope = "project"
    if "--k" in sys.argv:
        k = int(sys.argv[sys.argv.index("--k") + 1])
    if "--label" in sys.argv:
        label = sys.argv[sys.argv.index("--label") + 1]
    if "--scope" in sys.argv:
        scope = sys.argv[sys.argv.index("--scope") + 1]
    if scope not in ("project", "global"):
        sys.exit(f"unknown scope: {scope}")
    evaluate(run_dir, k, label, scope)
