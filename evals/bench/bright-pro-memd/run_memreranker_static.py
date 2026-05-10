#!/usr/bin/env python3
"""Rerank an existing Bright-Pro score.json with MemReranker."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import time
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bright-pro-root", type=Path, default=Path("/tmp/bright-pro"))
    parser.add_argument(
        "--source-run",
        type=Path,
        default=Path("evals/bench/bright-pro-memd/results/biology_memd_q5_d141"),
        help="Run directory with selected_examples.json and selected_documents.json.",
    )
    parser.add_argument("--candidate-score", type=Path, required=True)
    parser.add_argument("--candidate-name", required=True)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("evals/bench/bright-pro-memd/results"),
    )
    parser.add_argument("--run-name", default=None)
    parser.add_argument("--task", default="biology")
    parser.add_argument("--model", default="IAAR-Shanghai/MemReranker-4B")
    parser.add_argument("--device", default="cuda")
    parser.add_argument("--batch-size", type=int, default=1)
    parser.add_argument("--candidate-k", type=int, default=50)
    parser.add_argument("--top-k", type=int, default=50)
    return parser.parse_args()


def import_from_path(module_name: str, path: Path):
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not import {module_name} from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_bright_pro(root: Path) -> dict[str, Any]:
    if not (root / "bright_pro_data.py").is_file():
        raise FileNotFoundError(
            f"Bright-Pro checkout not found at {root}. "
            "Clone https://github.com/yale-nlp/Bright-Pro first."
        )
    sys.path.insert(0, str(root))
    sys.path.insert(0, str(root / "retrieval"))

    from bright_pro_data import build_aspect_weights, build_doc_to_aspect_id  # type: ignore
    from metrics import calculate_retrieval_metrics  # type: ignore

    alpha_module = import_from_path(
        "bright_pro_alpha_ndcg",
        root / "retrieval" / "evaluation" / "alpha-ndcg-evaluation.py",
    )
    return {
        "build_doc_to_aspect_id": build_doc_to_aspect_id,
        "build_aspect_weights": build_aspect_weights,
        "calculate_retrieval_metrics": calculate_retrieval_metrics,
        "alpha_module": alpha_module,
    }


def load_selected(source_run: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    return (
        json.loads((source_run / "selected_examples.json").read_text(encoding="utf-8")),
        json.loads((source_run / "selected_documents.json").read_text(encoding="utf-8")),
        json.loads((source_run / "manifest.json").read_text(encoding="utf-8")),
    )


def main() -> int:
    args = parse_args()
    bp = require_bright_pro(args.bright_pro_root.resolve())
    selected_examples, selected_docs, source_manifest = load_selected(args.source_run)
    doc_text = {str(doc["id"]): str(doc["content"]) for doc in selected_docs}
    candidates = json.loads(args.candidate_score.read_text(encoding="utf-8"))

    run_name = args.run_name or (
        f"{args.task}_{args.candidate_name}_memreranker_q{len(selected_examples)}_d{len(selected_docs)}"
    )
    output_dir = args.output_dir / run_name
    output_dir.mkdir(parents=True, exist_ok=True)

    load_start = time.perf_counter()
    from sentence_transformers import CrossEncoder

    model = CrossEncoder(args.model, device=args.device, trust_remote_code=True)
    load_elapsed = time.perf_counter() - load_start

    rerank_start = time.perf_counter()
    scores: dict[str, dict[str, float]] = {}
    records: list[dict[str, Any]] = []
    for example in selected_examples:
        qid = str(example["id"])
        original = candidates.get(qid, {})
        ranked = sorted(original.items(), key=lambda item: -float(item[1]))[: args.candidate_k]
        pairs = [(example["query"], doc_text[doc_id]) for doc_id, _ in ranked if doc_id in doc_text]
        doc_ids = [doc_id for doc_id, _ in ranked if doc_id in doc_text]
        if pairs:
            pred = model.predict(pairs, batch_size=args.batch_size)
            pred_scores = [float(x) for x in pred]
        else:
            pred_scores = []
        reranked = sorted(zip(doc_ids, pred_scores), key=lambda item: -item[1])[: args.top_k]
        scores[qid] = {doc_id: score for doc_id, score in reranked}
        records.append(
            {
                "query_id": qid,
                "candidate_count": len(ranked),
                "reranked_count": len(reranked),
                "top_docs": [
                    {"doc_id": doc_id, "score": score}
                    for doc_id, score in reranked[:10]
                ],
            }
        )
    rerank_elapsed = time.perf_counter() - rerank_start

    score_file = output_dir / "score.json"
    score_file.write_text(json.dumps(scores, indent=2), encoding="utf-8")
    (output_dir / "rerank_records.json").write_text(
        json.dumps(records, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "selected_examples.json").write_text(
        json.dumps(selected_examples, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "selected_documents.json").write_text(
        json.dumps(selected_docs, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    qrels = {
        str(example["id"]): {str(doc_id): 1 for doc_id in example.get("gold_ids", [])}
        for example in selected_examples
    }
    standard_metrics = bp["calculate_retrieval_metrics"](
        results=scores,
        qrels=qrels,
        k_values=[1, 5, 10, 25, 50, 100],
    )
    alpha_metrics = bp["alpha_module"].evaluate_file(
        score_file,
        selected_examples,
        bp["build_doc_to_aspect_id"](args.task),
        bp["build_aspect_weights"](args.task),
        alpha=0.5,
        k=25,
    )

    manifest = {
        "method": "memreranker",
        "model": args.model,
        "interface": "sentence_transformers_cross_encoder",
        "candidate_name": args.candidate_name,
        "candidate_score": str(args.candidate_score),
        "source_run": str(args.source_run),
        "source_manifest": source_manifest,
        "run_name": run_name,
        "task": args.task,
        "query_count": len(selected_examples),
        "document_count": len(selected_docs),
        "candidate_k": args.candidate_k,
        "top_k": args.top_k,
        "device": args.device,
        "batch_size": args.batch_size,
    }
    timing = {
        "load_seconds": round(load_elapsed, 3),
        "rerank_seconds": round(rerank_elapsed, 3),
        "avg_rerank_seconds_per_query": round(
            rerank_elapsed / max(1, len(selected_examples)),
            3,
        ),
    }
    payload = {
        "manifest": manifest,
        "timing": timing,
        "standard_metrics": standard_metrics,
        "alpha_ndcg": alpha_metrics,
        "score_file": str(score_file),
    }
    (output_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "results.json").write_text(
        json.dumps(payload, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
