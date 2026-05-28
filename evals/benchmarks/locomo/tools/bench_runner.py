#!/usr/bin/env python3
"""LoCoMo retrieval benchmark — single driver across all adapter systems.

Usage:
    bench_runner.py --system memd --memd-bin ../../../target/release/memd
    bench_runner.py --system mem0 --mem0-llm-endpoint http://127.0.0.1:8010/v1
    bench_runner.py --system superlocalmemory
    bench_runner.py --merge a.json b.json --out merged.json --markdown-out merged.md

Each adapter is responsible for the seed + recall loop; this driver loads
LoCoMo, dispatches to the chosen adapter, and renders the markdown report.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

REPO_ROOT = Path(__file__).resolve().parents[4]
TOOLS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR))


SESSION_RE = re.compile(r"^session_(\d+)$")


@dataclass(frozen=True)
class Fact:
    sample_id: str
    dia_id: str
    text: str
    content: str


@dataclass(frozen=True)
class Query:
    sample_id: str
    question: str
    evidence: tuple[str, ...]
    category: int


def load_locomo(path: Path, categories: set[int]) -> tuple[list[Fact], list[Query]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, list):
        raise ValueError(f"expected LoCoMo top-level array in {path}")

    facts: list[Fact] = []
    queries: list[Query] = []
    for conv_index, sample in enumerate(data):
        sample_id = str(sample.get("sample_id") or f"conversation_{conv_index}")
        conversation = sample.get("conversation") or {}

        session_keys = []
        for key in conversation:
            match = SESSION_RE.match(str(key))
            if match:
                session_keys.append((int(match.group(1)), key))

        for session_num, session_key in sorted(session_keys):
            session_date = conversation.get(f"{session_key}_date_time", "")
            turns = conversation.get(session_key) or []
            for turn in turns:
                if not isinstance(turn, dict):
                    continue
                dia_id = str(turn.get("dia_id") or "")
                text = str(turn.get("text") or "").strip()
                speaker = str(turn.get("speaker") or "").strip()
                if not dia_id or not text:
                    continue
                content = (
                    f"sample_id={sample_id} dia_id={dia_id} session={session_num} "
                    f"date={session_date} speaker={speaker}: {text}"
                )
                facts.append(Fact(sample_id, dia_id, text, content))

        for qa in sample.get("qa") or []:
            if not isinstance(qa, dict):
                continue
            category = int(qa.get("category") or 0)
            if category not in categories:
                continue
            question = str(qa.get("question") or qa.get("q") or "").strip()
            evidence = tuple(str(x) for x in (qa.get("evidence") or []) if x)
            if question and evidence:
                queries.append(Query(sample_id, question, evidence, category))

    return facts, queries


def rank_of(ranked_ids: Iterable[str], gold_ids: Iterable[str], k: int) -> int | None:
    gold = set(gold_ids)
    for idx, item_id in enumerate(ranked_ids, start=1):
        if idx > k:
            return None
        if item_id in gold:
            return idx
    return None


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * q)))
    return ordered[idx]


def summarize(system: str, rows: list[dict[str, Any]], seed_ms: float, documents: int) -> dict[str, Any]:
    ranks = [row["rank"] for row in rows]
    latencies = [row["latency_ms"] for row in rows]

    def hit_at(k: int) -> float:
        return statistics.mean(1.0 if r and r <= k else 0.0 for r in ranks) if ranks else 0.0

    by_category: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        by_category.setdefault(str(row["category"]), []).append(row)

    per_category = {}
    for category, items in sorted(by_category.items(), key=lambda x: int(x[0])):
        category_ranks = [item["rank"] for item in items]
        per_category[category] = {
            "questions": len(items),
            "hit1": statistics.mean(1.0 if r == 1 else 0.0 for r in category_ranks),
            "hit3": statistics.mean(1.0 if r and r <= 3 else 0.0 for r in category_ranks),
            "hit10": statistics.mean(1.0 if r and r <= 10 else 0.0 for r in category_ranks),
            "mrr_at_10": statistics.mean(1.0 / r if r else 0.0 for r in category_ranks),
        }

    return {
        "system": system,
        "documents_seeded": documents,
        "seed_total_ms": seed_ms,
        "questions": len(rows),
        "hit1": hit_at(1),
        "hit3": hit_at(3),
        "hit10": hit_at(10),
        "mrr_at_10": statistics.mean(1.0 / r if r else 0.0 for r in ranks) if ranks else 0.0,
        "avg_search_ms": statistics.mean(latencies) if latencies else 0.0,
        "p50_search_ms": percentile(latencies, 0.50),
        "p95_search_ms": percentile(latencies, 0.95),
        "per_category": per_category,
        "details": rows,
    }


def render_markdown(result: dict[str, Any]) -> str:
    lines = [
        "# LoCoMo Retrieval Benchmark",
        "",
        "Direct retrieval benchmark on upstream `locomo10.json`: each system is seeded with the same conversation turns and scored against LoCoMo evidence IDs.",
        "",
        f"- Dataset: `{result['dataset']}`",
        f"- Categories: `{', '.join(map(str, result['categories']))}`",
        f"- Top-k: `{result['k']}`",
        f"- Questions: `{result['questions']}`",
        f"- Facts: `{result['facts']}`",
        "",
        "| System | MRR@10 | Hit@1 | Hit@3 | Hit@10 | Avg search ms | P95 search ms | Seed ms |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for system in result["systems"]:
        lines.append(
            "| {system} | {mrr_at_10:.3f} | {hit1:.3f} | {hit3:.3f} | {hit10:.3f} | "
            "{avg_search_ms:.1f} | {p95_search_ms:.1f} | {seed_total_ms:.1f} |".format(**system)
        )
    lines.extend(["", "## Per Category", ""])
    for system in result["systems"]:
        lines.append(f"### {system['system']}")
        lines.append("| Category | Questions | MRR@10 | Hit@1 | Hit@3 | Hit@10 |")
        lines.append("|---:|---:|---:|---:|---:|---:|")
        for category, metrics in system["per_category"].items():
            lines.append(
                "| {cat} | {questions} | {mrr_at_10:.3f} | {hit1:.3f} | {hit3:.3f} | {hit10:.3f} |".format(
                    cat=category, **metrics
                )
            )
        lines.append("")
    return "\n".join(lines)


def write_report(args: argparse.Namespace, systems: list[dict[str, Any]], categories: set[int], n_questions: int, n_facts: int) -> None:
    result = {
        "dataset": str(args.dataset),
        "categories": sorted(categories),
        "k": args.k,
        "questions": n_questions,
        "facts": n_facts,
        "systems": systems,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2, sort_keys=True), encoding="utf-8")
    args.markdown_out.write_text(render_markdown(result), encoding="utf-8")
    print(render_markdown(result))


def merge_results(inputs: list[Path], out: Path, markdown_out: Path) -> None:
    merged: dict | None = None
    systems: list[dict] = []
    for path in inputs:
        data = json.loads(path.read_text(encoding="utf-8"))
        if merged is None:
            merged = {k: data[k] for k in ("dataset", "categories", "k", "questions", "facts")}
            merged["systems"] = []
        else:
            for key in ("dataset", "categories", "k"):
                if data[key] != merged[key]:
                    raise SystemExit(f"{path}: {key} mismatch; all inputs must be the same workload")
            merged["questions"] = max(merged["questions"], data["questions"])
            merged["facts"] = max(merged["facts"], data["facts"])
        systems.extend(data["systems"])
    assert merged is not None
    merged["systems"] = systems
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(merged, indent=2, sort_keys=True), encoding="utf-8")
    markdown_out.write_text(render_markdown(merged), encoding="utf-8")
    print(render_markdown(merged))


def parse_categories(raw: str) -> set[int]:
    return {int(part.strip()) for part in raw.split(",") if part.strip()}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--system", choices=["memd", "mem0", "superlocalmemory"])
    parser.add_argument("--merge", nargs="+", type=Path, help="Merge per-system result JSONs into one comparison.")
    parser.add_argument("--dataset", type=Path, default=Path(__file__).resolve().parents[1] / "datasets" / "locomo10.json")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--markdown-out", type=Path, required=True)
    parser.add_argument("--categories", default="1,2,3,4")
    parser.add_argument("--k", type=int, default=10)
    parser.add_argument("--max-conversations", type=int, default=0)
    parser.add_argument("--max-questions", type=int, default=0)

    # memd
    parser.add_argument("--memd-bin", type=Path)

    # mem0
    parser.add_argument("--mem0-llm-endpoint", default="http://127.0.0.1:8010/v1")
    parser.add_argument("--mem0-llm-model", default="gemma4-31b")
    parser.add_argument("--mem0-embedding-model", default="sentence-transformers/all-MiniLM-L6-v2")
    parser.add_argument("--mem0-data-dir", type=Path)

    # slm
    parser.add_argument("--slm-data-dir", type=Path)

    args = parser.parse_args()

    if args.merge:
        merge_results(args.merge, args.out, args.markdown_out)
        return 0

    if not args.system:
        parser.error("--system is required (unless --merge is given)")

    categories = parse_categories(args.categories)
    facts, queries = load_locomo(args.dataset, categories)

    if args.max_conversations:
        sample_ids = sorted({f.sample_id for f in facts})[: args.max_conversations]
        sample_set = set(sample_ids)
        facts = [f for f in facts if f.sample_id in sample_set]
        queries = [q for q in queries if q.sample_id in sample_set]

    if args.system == "memd":
        from adapters.memd_adapter import run_memd
        summary = run_memd(facts, queries, args.memd_bin, args.k, args.max_questions, summarize)
    elif args.system == "mem0":
        from adapters.mem0_adapter import run_mem0
        summary = run_mem0(
            facts, queries,
            data_dir=args.mem0_data_dir,
            llm_endpoint=args.mem0_llm_endpoint,
            llm_model=args.mem0_llm_model,
            embedding_model=args.mem0_embedding_model,
            k=args.k,
            max_questions=args.max_questions,
            summarize=summarize,
            rank_of=rank_of,
        )
    elif args.system == "superlocalmemory":
        from adapters.slm_adapter import run_slm
        summary = run_slm(
            facts, queries,
            data_dir=args.slm_data_dir,
            k=args.k,
            max_questions=args.max_questions,
            summarize=summarize,
            rank_of=rank_of,
        )
    else:
        raise SystemExit(f"unknown system: {args.system}")

    n_questions = min(len(queries), args.max_questions or len(queries))
    write_report(args, [summary], categories, n_questions, len(facts))
    return 0


if __name__ == "__main__":
    sys.exit(main())
