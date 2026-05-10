#!/usr/bin/env python3
"""Run the Bright-Pro static subset through SuperLocalMemory.

This adapter reuses the selected Bright-Pro examples/documents from an existing
memd run so the two systems are scored on the same queries and corpus.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import shutil
import sys
import time
import uuid
from pathlib import Path
from typing import Any


DOC_ID_RE = re.compile(r"BRIGHT_PRO_DOC_ID:\s*([^\n]+)")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bright-pro-root", type=Path, default=Path("/tmp/bright-pro"))
    parser.add_argument(
        "--source-run",
        type=Path,
        default=Path("evals/bench/bright-pro-memd/results/biology_memd_q5_d141"),
        help="Existing run directory containing selected_examples/documents JSON.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("evals/bench/bright-pro-memd/results"),
    )
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=Path(".bench/bright-pro-memd/slm-data"),
    )
    parser.add_argument("--run-name", default=None)
    parser.add_argument("--task", default="biology")
    parser.add_argument("--top-k", type=int, default=50)
    parser.add_argument("--limit", type=int, default=100)
    parser.add_argument(
        "--embedder-mode",
        choices=["in-process", "slm-worker", "off"],
        default="in-process",
        help=(
            "Use SLM's worker embedder, an in-process SentenceTransformer "
            "with the same default model, or disable embedding channels."
        ),
    )
    parser.add_argument("--disable-cross-encoder", action="store_true")
    parser.add_argument("--keep-data-dir", action="store_true")
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


def doc_id_from_text(text: str | None) -> str | None:
    if not text:
        return None
    match = DOC_ID_RE.search(text)
    if not match:
        return None
    return match.group(1).strip()


class InProcessSentenceTransformerEmbedder:
    """Small adapter matching the SLM EmbeddingService methods used here."""

    def __init__(self, model_name: str, dimension: int) -> None:
        from sentence_transformers import SentenceTransformer

        self.model_name = model_name
        self.dimension = dimension
        self._model = SentenceTransformer(
            model_name,
            trust_remote_code=True,
            device="cpu",
        )
        actual = int(self._model.get_sentence_embedding_dimension())
        if actual != dimension:
            raise ValueError(f"embedding dimension mismatch: {actual} != {dimension}")

    def embed(self, text: str) -> list[float]:
        vec = self._model.encode(
            [text],
            normalize_embeddings=True,
            convert_to_numpy=True,
        )[0]
        return vec.astype("float32").tolist()

    @staticmethod
    def compute_fisher_params(embedding: list[float]) -> tuple[list[float], list[float]]:
        import numpy as np

        arr = np.asarray(embedding, dtype=np.float64)
        norm = float(np.linalg.norm(arr))
        if norm < 1e-10:
            mean = np.zeros(len(arr), dtype=np.float64)
            variance = np.full(len(arr), 2.0, dtype=np.float64)
            return mean.tolist(), variance.tolist()
        mean = arr / norm
        abs_mean = np.abs(mean)
        max_val = float(np.max(abs_mean)) + 1e-10
        signal_strength = abs_mean / max_val
        variance = 2.0 - (2.0 - 0.05) * signal_strength
        variance = np.clip(variance, 0.05, 2.0)
        return mean.tolist(), variance.tolist()


def load_selected(source_run: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    examples_path = source_run / "selected_examples.json"
    docs_path = source_run / "selected_documents.json"
    manifest_path = source_run / "manifest.json"
    return (
        json.loads(examples_path.read_text(encoding="utf-8")),
        json.loads(docs_path.read_text(encoding="utf-8")),
        json.loads(manifest_path.read_text(encoding="utf-8")),
    )


def main() -> int:
    args = parse_args()
    bp = require_bright_pro(args.bright_pro_root.resolve())
    selected_examples, selected_docs, source_manifest = load_selected(args.source_run)

    run_name = args.run_name or (
        f"{args.task}_superlocalmemory_q{len(selected_examples)}_d{len(selected_docs)}"
    )
    output_dir = args.output_dir / run_name
    output_dir.mkdir(parents=True, exist_ok=True)

    data_dir = args.data_dir / run_name
    if data_dir.exists() and not args.keep_data_dir:
        shutil.rmtree(data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)

    from superlocalmemory.core.config import SLMConfig
    from superlocalmemory.core.engine import MemoryEngine
    from superlocalmemory.storage.models import AtomicFact, FactType, Mode

    config = SLMConfig.for_mode(Mode.A, base_dir=data_dir)
    if args.disable_cross_encoder:
        config.retrieval.use_cross_encoder = False

    engine = MemoryEngine(config)
    add_start = time.perf_counter()
    engine.initialize()
    if args.embedder_mode == "in-process":
        embedder = InProcessSentenceTransformerEmbedder(
            config.embedding.model_name,
            config.embedding.dimension,
        )
        engine._embedder = embedder
        engine._retrieval_engine._embedder = embedder
    elif args.embedder_mode == "off":
        engine._embedder = None
        engine._retrieval_engine._embedder = None

    store_records: list[dict[str, Any]] = []
    for doc in selected_docs:
        doc_id = str(doc["id"])
        text = f"BRIGHT_PRO_DOC_ID: {doc_id}\n{doc['content']}"
        fact = AtomicFact(
            fact_id=uuid.uuid4().hex[:16],
            profile_id=config.active_profile,
            content=text,
            fact_type=FactType.SEMANTIC,
            entities=[],
            confidence=0.9,
            importance=0.5,
        )
        fact_id = engine.store_fact_direct(fact)
        store_records.append({"doc_id": doc_id, "fact_ids": [fact_id]})
    add_elapsed = time.perf_counter() - add_start

    search_start = time.perf_counter()
    scores: dict[str, dict[str, float]] = {}
    search_records: list[dict[str, Any]] = []
    for example in selected_examples:
        response = engine.recall(example["query"], limit=args.limit, fast=True)
        per_query: dict[str, float] = {}
        raw_results: list[dict[str, Any]] = []
        for rank, result in enumerate(response.results, start=1):
            fact = result.fact
            doc_id = doc_id_from_text(fact.content)
            raw_results.append(
                {
                    "rank": rank,
                    "fact_id": fact.fact_id,
                    "memory_id": fact.memory_id,
                    "doc_id": doc_id,
                    "score": float(result.score),
                    "content_preview": fact.content[:240],
                    "channel_scores": getattr(result, "channel_scores", None),
                }
            )
            if doc_id is None:
                continue
            per_query[doc_id] = max(per_query.get(doc_id, float("-inf")), float(result.score))
        qid = str(example["id"])
        scores[qid] = dict(sorted(per_query.items(), key=lambda item: -item[1])[: args.top_k])
        search_records.append(
            {
                "query_id": qid,
                "query": example["query"],
                "result_count": len(response.results),
                "doc_result_count": len(scores[qid]),
                "raw_results": raw_results,
            }
        )
    search_elapsed = time.perf_counter() - search_start

    score_file = output_dir / "score.json"
    score_file.write_text(json.dumps(scores, indent=2), encoding="utf-8")
    (output_dir / "store_records.json").write_text(
        json.dumps(store_records, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "search_records.json").write_text(
        json.dumps(search_records, ensure_ascii=False, indent=2),
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

    timing = {
        "add_seconds": round(add_elapsed, 3),
        "search_seconds": round(search_elapsed, 3),
        "avg_search_seconds_per_query": round(
            search_elapsed / max(1, len(selected_examples)),
            3,
        ),
    }
    manifest = {
        "method": "superlocalmemory",
        "package": "superlocalmemory",
        "mode": "A",
        "interface": "python_engine",
        "store_mode": "direct_document_facts",
        "embedder_mode": args.embedder_mode,
        "embedding_model": config.embedding.model_name,
        "source_run": str(args.source_run),
        "source_manifest": source_manifest,
        "run_name": run_name,
        "task": args.task,
        "query_count": len(selected_examples),
        "document_count": len(selected_docs),
        "gold_document_count": source_manifest.get("gold_document_count"),
        "top_k": args.top_k,
        "limit": args.limit,
        "data_dir": str(data_dir),
        "cross_encoder_disabled": bool(args.disable_cross_encoder),
        "hf_home": os.environ.get("HF_HOME"),
    }
    result_payload = {
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
        json.dumps(result_payload, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print(json.dumps(result_payload, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
