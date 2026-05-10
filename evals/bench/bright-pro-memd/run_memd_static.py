#!/usr/bin/env python3
"""Run a scoped Bright-Pro static retrieval benchmark through memd.

The script uses the upstream Bright-Pro repository for data loading and metric
implementations, but it keeps the memd adapter local to this repository.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


TASKS = [
    "biology",
    "earth_science",
    "economics",
    "psychology",
    "robotics",
    "stackoverflow",
    "sustainable_living",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bright-pro-root", type=Path, default=Path("/tmp/bright-pro"))
    parser.add_argument("--memd-bin", type=Path, default=Path("target/release/memd"))
    parser.add_argument("--task", choices=TASKS, default="biology")
    parser.add_argument("--max-queries", type=int, default=5)
    parser.add_argument(
        "--decoy-docs",
        type=int,
        default=1000,
        help="Number of non-gold documents to include unless --full-corpus is set.",
    )
    parser.add_argument("--full-corpus", action="store_true")
    parser.add_argument("--top-k", type=int, default=100)
    parser.add_argument(
        "--memd-k",
        type=int,
        default=None,
        help="Number of raw memd results to request before filtering to corpus docs.",
    )
    parser.add_argument("--batch-size", type=int, default=50)
    parser.add_argument(
        "--search-variant",
        choices=["hybrid-feature", "hybrid-cross-encoder", "dense-only", "bm25-only"],
        default="hybrid-feature",
        help="memd retrieval variant passed to the CLI.",
    )
    parser.add_argument(
        "--rust-log",
        default="error",
        help="RUST_LOG value for benchmarked memd commands.",
    )
    parser.add_argument("--tenant-id", default="bright_pro_memd")
    parser.add_argument("--project-id", default=None)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("evals/bench/bright-pro-memd/results"),
    )
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=Path(".bench/bright-pro-memd/memd-data"),
    )
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

    from bright_pro_data import (  # type: ignore
        build_aspect_weights,
        build_doc_to_aspect_id,
        load_bright_pro,
    )
    from metrics import calculate_retrieval_metrics  # type: ignore

    alpha_module = import_from_path(
        "bright_pro_alpha_ndcg",
        root / "retrieval" / "evaluation" / "alpha-ndcg-evaluation.py",
    )
    return {
        "load_bright_pro": load_bright_pro,
        "build_doc_to_aspect_id": build_doc_to_aspect_id,
        "build_aspect_weights": build_aspect_weights,
        "calculate_retrieval_metrics": calculate_retrieval_metrics,
        "alpha_module": alpha_module,
    }


def select_subset(
    examples: list[dict[str, Any]],
    documents: list[dict[str, Any]],
    max_queries: int,
    decoy_docs: int,
    full_corpus: bool,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    selected_examples = examples[:max_queries] if max_queries > 0 else examples
    gold_ids = {
        str(doc_id)
        for example in selected_examples
        for doc_id in example.get("gold_ids", [])
    }

    if full_corpus:
        selected_docs = documents
        scope = "full_corpus"
    else:
        selected_docs = []
        seen = set()
        for doc in documents:
            doc_id = str(doc["id"])
            if doc_id in gold_ids and doc_id not in seen:
                selected_docs.append(doc)
                seen.add(doc_id)
        for doc in documents:
            if len([d for d in selected_docs if str(d["id"]) not in gold_ids]) >= decoy_docs:
                break
            doc_id = str(doc["id"])
            if doc_id in seen:
                continue
            selected_docs.append(doc)
            seen.add(doc_id)
        scope = "gold_plus_decoys"

    manifest = {
        "scope": scope,
        "query_count": len(selected_examples),
        "document_count": len(selected_docs),
        "gold_document_count": len(gold_ids),
        "full_corpus": full_corpus,
        "decoy_docs": None if full_corpus else decoy_docs,
    }
    return selected_examples, selected_docs, manifest


def doc_id_from_result(result: dict[str, Any]) -> str | None:
    for tag in result.get("tags", []) or []:
        if isinstance(tag, str) and tag.startswith("doc_id:"):
            return tag.removeprefix("doc_id:")
    text = result.get("text") or ""
    prefix = "BRIGHT_PRO_DOC_ID: "
    if isinstance(text, str) and text.startswith(prefix):
        return text.splitlines()[0].removeprefix(prefix).strip()
    return None


def chunked(items: list[dict[str, Any]], size: int):
    for start in range(0, len(items), size):
        yield items[start : start + size]


def run_command(
    args: list[str],
    cwd: Path | None = None,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    env.setdefault("RUST_LOG", "error")
    proc = subprocess.run(
        args,
        cwd=str(cwd) if cwd else None,
        env=env,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        stderr_tail = proc.stderr[-4000:]
        raise RuntimeError(
            f"command failed with exit {proc.returncode}: {' '.join(args)}\n{stderr_tail}"
        )
    return proc


def write_memd_batch(
    path: Path,
    tenant_id: str,
    project_id: str,
    documents: list[dict[str, Any]],
    batch_size: int,
) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for docs in chunked(documents, batch_size):
            chunks = []
            for doc in docs:
                doc_id = str(doc["id"])
                chunks.append(
                    {
                        "project_id": project_id,
                        "type": "doc",
                        "text": f"BRIGHT_PRO_DOC_ID: {doc_id}\n{doc['content']}",
                        "tags": ["bright-pro", f"task:{project_id}", f"doc_id:{doc_id}"],
                        "source": {
                            "uri": f"bright-pro://{project_id}/{doc_id}",
                            "path": doc_id,
                        },
                    }
                )
            request = {
                "tool": "memory.add_batch",
                "arguments": {"tenant_id": tenant_id, "chunks": chunks},
            }
            handle.write(json.dumps(request, ensure_ascii=False) + "\n")


def write_search_batch(
    path: Path,
    tenant_id: str,
    project_id: str,
    examples: list[dict[str, Any]],
    memd_k: int,
) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for example in examples:
            request = {
                "tool": "memory.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "query": example["query"],
                    "k": memd_k,
                    "filters": {"types": ["doc"]},
                    "oversample_factor": 1,
                    "include_artifact": False,
                },
            }
            handle.write(json.dumps(request, ensure_ascii=False) + "\n")


def parse_search_output(
    path: Path,
    examples: list[dict[str, Any]],
    top_k: int,
) -> dict[str, dict[str, float]]:
    scores: dict[str, dict[str, float]] = {}
    with path.open("r", encoding="utf-8") as handle:
        for example, line in zip(examples, handle):
            row = json.loads(line)
            if not row.get("ok"):
                raise RuntimeError(f"memd search failed on line {row.get('line')}: {row}")
            qid = str(example["id"])
            per_query: dict[str, float] = {}
            for result in row.get("result", {}).get("results", []):
                doc_id = doc_id_from_result(result)
                if doc_id is None:
                    continue
                score = float(result.get("score", 0.0))
                per_query[doc_id] = max(per_query.get(doc_id, float("-inf")), score)
            scores[qid] = dict(
                sorted(per_query.items(), key=lambda item: -item[1])[:top_k]
            )
    return scores


def compute_alpha(
    alpha_module,
    score_file: Path,
    examples: list[dict[str, Any]],
    doc_to_aspect: dict[str, str],
    aspect_weights: dict[str, float],
    alpha: float = 0.5,
    k: int = 25,
) -> dict[str, Any]:
    return alpha_module.evaluate_file(
        score_file,
        examples,
        doc_to_aspect,
        aspect_weights,
        alpha=alpha,
        k=k,
    )


def main() -> int:
    args = parse_args()
    project_id = args.project_id or f"bright-pro-{args.task}"
    memd_bin = args.memd_bin.resolve()
    if not memd_bin.is_file():
        raise FileNotFoundError(f"memd binary not found: {memd_bin}")

    bp = require_bright_pro(args.bright_pro_root.resolve())
    load_bright_pro = bp["load_bright_pro"]
    memd_k = args.memd_k if args.memd_k is not None else max(args.top_k * 2, args.top_k + 25)
    memd_k = min(100, memd_k)
    if args.top_k > memd_k:
        raise ValueError(f"--top-k ({args.top_k}) cannot exceed effective --memd-k ({memd_k})")

    examples = load_bright_pro("examples", args.task)
    documents = load_bright_pro("documents", args.task)
    selected_examples, selected_docs, manifest = select_subset(
        examples,
        documents,
        args.max_queries,
        args.decoy_docs,
        args.full_corpus,
    )
    manifest.update(
        {
            "task": args.task,
            "tenant_id": args.tenant_id,
            "project_id": project_id,
            "top_k": args.top_k,
            "memd_k": memd_k,
            "search_variant": args.search_variant,
            "rust_log": args.rust_log,
            "search_filters": {"types": ["doc"]},
            "search_oversample_factor": 1,
            "corpus_filter": "results with doc_id:* tags",
            "memd_bin": str(memd_bin),
            "bright_pro_root": str(args.bright_pro_root.resolve()),
        }
    )

    run_name = (
        f"{args.task}_memd_"
        f"{'full' if args.full_corpus else f'q{len(selected_examples)}_d{len(selected_docs)}'}"
    )
    output_dir = args.output_dir / run_name
    output_dir.mkdir(parents=True, exist_ok=True)

    data_dir = args.data_dir / run_name
    if data_dir.exists() and not args.keep_data_dir:
        shutil.rmtree(data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)

    (output_dir / "selected_examples.json").write_text(
        json.dumps(selected_examples, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "selected_documents.json").write_text(
        json.dumps(selected_docs, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    (output_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    add_jsonl = output_dir / "memd_add.jsonl"
    add_out = output_dir / "memd_add.out.jsonl"
    search_jsonl = output_dir / "memd_search.jsonl"
    search_out = output_dir / "memd_search.out.jsonl"
    write_memd_batch(add_jsonl, args.tenant_id, project_id, selected_docs, args.batch_size)
    write_search_batch(search_jsonl, args.tenant_id, project_id, selected_examples, memd_k)

    add_start = time.perf_counter()
    add_result = run_command(
        [
            str(memd_bin),
            "--data-dir",
            str(data_dir),
            "--search-variant",
            args.search_variant,
            "batch",
            "--jsonl",
            str(add_jsonl),
            "--output",
            str(add_out),
        ],
        env_overrides={"RUST_LOG": args.rust_log},
    )
    add_elapsed = time.perf_counter() - add_start
    if add_result.stderr:
        (output_dir / "memd_add.stderr.log").write_text(add_result.stderr, encoding="utf-8")

    search_start = time.perf_counter()
    search_result = run_command(
        [
            str(memd_bin),
            "--data-dir",
            str(data_dir),
            "--search-variant",
            args.search_variant,
            "batch",
            "--jsonl",
            str(search_jsonl),
            "--output",
            str(search_out),
        ],
        env_overrides={"RUST_LOG": args.rust_log},
    )
    search_elapsed = time.perf_counter() - search_start
    if search_result.stderr:
        (output_dir / "memd_search.stderr.log").write_text(
            search_result.stderr,
            encoding="utf-8",
        )

    score_file = output_dir / "score.json"
    scores = parse_search_output(search_out, selected_examples, args.top_k)
    score_file.write_text(json.dumps(scores, indent=2), encoding="utf-8")

    qrels = {
        str(example["id"]): {str(doc_id): 1 for doc_id in example.get("gold_ids", [])}
        for example in selected_examples
    }
    standard_metrics = bp["calculate_retrieval_metrics"](
        results=scores,
        qrels=qrels,
        k_values=[1, 5, 10, 25, 50, 100],
    )

    doc_to_aspect = bp["build_doc_to_aspect_id"](args.task)
    aspect_weights = bp["build_aspect_weights"](args.task)
    alpha_metrics = compute_alpha(
        bp["alpha_module"],
        score_file,
        selected_examples,
        doc_to_aspect,
        aspect_weights,
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
    result_payload = {
        "manifest": manifest,
        "timing": timing,
        "standard_metrics": standard_metrics,
        "alpha_ndcg": alpha_metrics,
        "score_file": str(score_file),
    }
    (output_dir / "results.json").write_text(
        json.dumps(result_payload, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    print(json.dumps(result_payload, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
