"""Answer-usefulness evaluation: F1 / EM / SubEM + evidence metrics.

For each category 1-4 question: search the question's conversation store
(k=20 by default, matching the prior baseline variant's k), pass the
retrieved turn texts verbatim to a pinned answer model, score the short
answer against LoCoMo gold with upstream-style metrics, and report evidence
recall@10/@20, evidence precision@10, and packed context tokens.

Answer pathway (pinned per run, recorded in the manifest):
- codex: `codex exec` (default model from the environment's Codex config)
- claude: `claude -p --model <model>`

Every generated answer is cached in benchmark-data/qa_cache keyed by
(engine, prompt version, question id, context hash); unchanged retrievals
are never re-answered. Failed rows (model error / timeout / empty output)
are counted and reported, never dropped.

Usage:
  python3 run_qa.py <run_dir> --split smoke|dev|promoted [--k 20]
      [--engine codex|claude] [--label name] [--workers 4]
      [--date-render]

--date-render prefixes each retrieved turn with its session datetime at
CONTEXT-BUILD time (a harness-side join from the dataset), leaving the
store and retrieval untouched. This models a memory system that keeps
event time as structured metadata and renders it at recall; the plain
store keeps its retrieval quality while the answer model gets absolute
time anchors for relative expressions in turn text.

--external-contexts <file> skips memd retrieval and scores answers over a
reference system's retrieved contexts (JSON: question_id ->
{ranked_dias, context_lines}), so competing systems share the exact same
answer model, prompt, cache, and metrics. With --date-render, per-line
dates are joined by exact content match against the dataset.
"""

import concurrent.futures
import hashlib
import json
import random
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

import common
import metrics

PROMPT_VERSION = "v1"
PROMPT_TEMPLATE = """You answer questions about one long conversation using only the memory excerpts below.

Memory excerpts (most relevant first):
{context}

Question: {question}

Reply with only the answer as the shortest possible phrase (a few words; a date, name, or noun phrase). Do not explain. If the excerpts do not contain the answer, reply exactly: Not mentioned.
Answer:"""

CACHE_DIR = common.REPO_ROOT / "benchmark-data" / "qa_cache"
ANSWER_TIMEOUT_S = 120


def cache_key(engine, question_id, context_hash):
    raw = f"{engine}|{PROMPT_VERSION}|{question_id}|{context_hash}"
    return hashlib.sha256(raw.encode()).hexdigest()


def call_model(engine, prompt):
    if engine == "codex":
        cmd = ["codex", "exec", "-s", "read-only", "--skip-git-repo-check", prompt]
    elif engine.startswith("claude"):
        cmd = ["claude", "-p", "--model", "claude-sonnet-5", prompt]
    else:
        raise ValueError(engine)
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=ANSWER_TIMEOUT_S,
        cwd=str(common.REPO_ROOT / "benchmark-data"),
    )
    if proc.returncode != 0:
        raise RuntimeError(f"rc={proc.returncode}: {proc.stderr[-500:]}")
    return proc.stdout


def extract_answer(raw: str) -> str:
    """Last non-empty line of model output, stripped of common wrappers."""
    lines = [ln.strip() for ln in raw.strip().splitlines() if ln.strip()]
    if not lines:
        return ""
    ans = lines[-1]
    ans = re.sub(r"^(answer\s*[:\-]\s*)", "", ans, flags=re.I).strip()
    return ans.strip(" `\"'")


def answer_one(engine, question_id, question, context_lines):
    context = "\n".join(f"- {t}" for t in context_lines)
    ctx_hash = hashlib.sha256(context.encode()).hexdigest()
    key = cache_key(engine, question_id, ctx_hash)
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache_file = CACHE_DIR / f"{key}.json"
    if cache_file.exists():
        cached = json.loads(cache_file.read_text())
        return cached["answer"], True, None

    prompt = PROMPT_TEMPLATE.format(context=context, question=question)
    last_err = None
    for attempt in (1, 2):
        try:
            raw = call_model(engine, prompt)
            ans = extract_answer(raw)
            if ans:
                cache_file.write_text(
                    json.dumps(
                        {
                            "answer": ans,
                            "engine": engine,
                            "prompt_version": PROMPT_VERSION,
                            "question_id": question_id,
                            "context_hash": ctx_hash,
                        }
                    )
                )
                return ans, False, None
            last_err = "empty answer"
        except Exception as exc:  # noqa: BLE001 - report, never drop
            last_err = str(exc)[:300]
        time.sleep(1.5 * attempt)
    return None, False, last_err


def stratified_dev_sample(questions, fraction=0.2, seed=42):
    by_cat = {}
    for q in questions:
        by_cat.setdefault(q["category"], []).append(q)
    rng = random.Random(seed)
    picked = []
    for cat in sorted(by_cat):
        qs = sorted(by_cat[cat], key=lambda q: q["question_id"])
        n = max(1, round(len(qs) * fraction))
        picked.extend(rng.sample(qs, n))
    return sorted(picked, key=lambda q: q["question_id"])


def evaluate(
    run_dir: Path,
    split: str,
    k: int,
    engine: str,
    label: str,
    workers: int,
    date_render: bool = False,
    external_contexts: Path | None = None,
):
    data = common.load_dataset()
    store_dir = run_dir / "store"
    mapping = {}
    if external_contexts is None:
        mapping = json.loads((run_dir / "chunk_to_dia.json").read_text())
    external = (
        json.loads(Path(external_contexts).read_text()) if external_contexts else None
    )

    # (conversation, exact stored text) -> dia, for external date joins.
    content_dia = {}
    for conv in data:
        for _key, session_dt, turn in common.iter_turns(conv["conversation"]):
            text = common.turn_text(turn, session_dt, "plain")
            key = (conv["sample_id"], text)
            if text.strip() and key not in content_dia:
                content_dia[key] = turn["dia_id"]

    # dia_id -> session datetime, per conversation (dia ids repeat across
    # conversations, so key by (sample_id, dia_id)).
    dia_datetime = {}
    for conv in data:
        for _key, session_dt, turn in common.iter_turns(conv["conversation"]):
            dia_datetime[(conv["sample_id"], turn["dia_id"])] = session_dt

    # Collect questions (cats 1-4) with valid evidence.
    questions = []
    for conv in data:
        sample_id = conv["sample_id"]
        conv_dias = set()
        for _key, _dt, turn in common.iter_turns(conv["conversation"]):
            conv_dias.add(turn["dia_id"])
        for qi, qa in enumerate(conv["qa"]):
            cat = qa.get("category")
            if cat not in (1, 2, 3, 4):
                continue
            evidence = [e for e in (qa.get("evidence") or []) if e in conv_dias]
            if not evidence:
                continue
            questions.append(
                {
                    "question_id": f"{sample_id}:{qi}",
                    "conversation": sample_id,
                    "project": f"conv-{sample_id}",
                    "category": cat,
                    "question": str(qa.get("question") or ""),
                    "answer": qa.get("answer"),
                    "evidence": evidence,
                }
            )

    if split == "smoke":
        conv0 = questions[0]["conversation"]
        questions = [q for q in questions if q["conversation"] == conv0][:25]
    elif split == "dev":
        questions = stratified_dev_sample(questions)
    elif split != "promoted":
        sys.exit(f"unknown split: {split}")

    # Retrieval pass: one batch per conversation, k=20 contexts.
    if external is not None:
        missing = 0
        for q in questions:
            entry = external.get(q["question_id"])
            if entry is None:
                q["ranked_dias"] = []
                q["context_lines"] = []
                missing += 1
                continue
            q["ranked_dias"] = entry.get("ranked_dias") or []
            lines = (entry.get("context_lines") or [])[:k]
            if date_render:
                rendered = []
                for line in lines:
                    dia = content_dia.get((q["conversation"], line))
                    dt = dia_datetime.get((q["conversation"], dia)) if dia else None
                    rendered.append(f"[{dt}] {line}" if dt else line)
                lines = rendered
            q["context_lines"] = lines
        print(f"external contexts: {len(questions)-missing} matched, {missing} missing")
        by_conv = {}
    else:
        by_conv = {}
        for q in questions:
            by_conv.setdefault(q["project"], []).append(q)
    for project in sorted(by_conv):
        qs = by_conv[project]
        requests = [
            {
                "tool": "memory.search",
                "arguments": {
                    "tenant_id": common.TENANT,
                    "project_id": project,
                    "query": q["question"],
                    "k": k,
                },
            }
            for q in qs
        ]
        rows, _times = common.run_batch(requests, store_dir)
        for q, row in zip(qs, rows):
            if not row.get("ok"):
                sys.exit(f"{project}: search failed: {json.dumps(row)[:300]}")
            results = (row.get("result") or {}).get("results") or []
            q["ranked_dias"] = [
                mapping[r["chunk_id"]] for r in results if r.get("chunk_id") in mapping
            ]
            if date_render:
                lines = []
                for r in results:
                    text = r.get("text") or ""
                    dia = mapping.get(r.get("chunk_id"))
                    dt = dia_datetime.get((q["conversation"], dia)) if dia else None
                    lines.append(f"[{dt}] {text}" if dt else text)
                q["context_lines"] = lines
            else:
                q["context_lines"] = [r.get("text") or "" for r in results]
        print(f"retrieved {project}: {len(qs)} questions")

    # Answer pass (cached, parallel workers).
    failed = []
    cache_hits = 0
    t0 = time.monotonic()

    def _work(q):
        return q, answer_one(engine, q["question_id"], q["question"], q["context_lines"])

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        for q, (ans, was_cached, err) in pool.map(_work, questions):
            q["prediction"] = ans
            if err is not None:
                failed.append({"question_id": q["question_id"], "error": err})
            if ans is not None and was_cached:
                cache_hits += 1
    answer_wall = time.monotonic() - t0

    # Score.
    rows = []
    for q in questions:
        pred = q.get("prediction")
        gold = str(q["answer"]) if q["answer"] is not None else ""
        row = {
            "question_id": q["question_id"],
            "category": q["category"],
            "failed": pred is None,
            "f1": metrics.f1_score(pred, gold) if pred is not None else 0.0,
            "em": bool(pred is not None and metrics.exact_match_score(pred, gold)),
            "subem": bool(pred is not None and metrics.sub_em_score(pred, gold)),
            "evidence_r10": metrics.evidence_recall_at(q["ranked_dias"], q["evidence"], 10),
            "evidence_r20": metrics.evidence_recall_at(q["ranked_dias"], q["evidence"], 20),
            "evidence_p10": metrics.evidence_precision_at(q["ranked_dias"], q["evidence"], 10),
            "packed_chars": sum(len(t) for t in q["context_lines"]),
            "question": q["question"],
            "gold": gold,
            "prediction": pred,
        }
        rows.append(row)

    n = len(rows)
    ok_rows = [r for r in rows if not r["failed"]]
    summary = {
        "label": label,
        "split": split,
        "engine": engine,
        "prompt_version": PROMPT_VERSION,
        "date_render": date_render,
        "k": k,
        "questions": n,
        "failed_rows": len(failed),
        "cache_hits": cache_hits,
        "answer_wall_s": round(answer_wall, 1),
        "f1": round(100 * sum(r["f1"] for r in rows) / n, 2),
        "em": round(100 * sum(r["em"] for r in rows) / n, 2),
        "subem": round(100 * sum(r["subem"] for r in rows) / n, 2),
        "evidence_r10": round(
            100 * statistics.mean(r["evidence_r10"] for r in rows if r["evidence_r10"] is not None), 2
        ),
        "evidence_r20": round(
            100 * statistics.mean(r["evidence_r20"] for r in rows if r["evidence_r20"] is not None), 2
        ),
        "evidence_p10": round(
            100 * statistics.mean(r["evidence_p10"] for r in rows), 2
        ),
        "mean_packed_chars": round(statistics.mean(r["packed_chars"] for r in rows)),
        "per_category": {},
    }
    for cat in (1, 2, 3, 4):
        cat_rows = [r for r in rows if r["category"] == cat]
        if cat_rows:
            summary["per_category"][str(cat)] = {
                "n": len(cat_rows),
                "f1": round(100 * sum(r["f1"] for r in cat_rows) / len(cat_rows), 2),
                "evidence_r10": round(
                    100
                    * statistics.mean(
                        r["evidence_r10"] for r in cat_rows if r["evidence_r10"] is not None
                    ),
                    2,
                ),
            }

    out = run_dir / f"qa_{label}_{split}.json"
    out.write_text(json.dumps({"summary": summary, "failed": failed, "rows": rows}, indent=1))
    common.write_manifest(
        run_dir,
        {
            "kind": "qa_eval",
            "label": label,
            "split": split,
            "engine": engine,
            "k": k,
            "external_contexts": str(external_contexts) if external_contexts else None,
            "memd_version": (
                common.memd_version(store_dir) if external_contexts is None else None
            ),
        },
    )
    print(json.dumps(summary, indent=2))
    print(f"written: {out}")
    return summary


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    run_dir = Path(sys.argv[1]).resolve()
    args = sys.argv[2:]

    def opt(name, default):
        return args[args.index(name) + 1] if name in args else default

    evaluate(
        run_dir,
        split=opt("--split", "dev"),
        k=int(opt("--k", 20)),
        engine=opt("--engine", "codex"),
        label=opt("--label", "qa"),
        workers=int(opt("--workers", 4)),
        date_render="--date-render" in args,
        external_contexts=(
            Path(opt("--external-contexts", "")) if "--external-contexts" in args else None
        ),
    )
