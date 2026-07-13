"""Mem0 LoCoMo adapter.

Requires `pip install mem0ai sentence-transformers`. Uses the Mem0 `vllm`
LLM provider so a self-hosted OpenAI-compatible endpoint can be pointed at
via --mem0-llm-endpoint / --mem0-llm-model. Stores one mem0 memory per
LoCoMo turn with dia_id in metadata; dedupes returned dia_ids at rank time.
"""

from __future__ import annotations

import shutil
import time
from pathlib import Path
from typing import Any, Callable


def build_memory(data_dir: Path, llm_endpoint: str, llm_model: str, embedding_model: str):
    from mem0 import Memory

    config = {
        "vector_store": {
            "provider": "qdrant",
            "config": {
                "collection_name": f"locomo_{int(time.time())}",
                "path": str(data_dir / "qdrant"),
                "embedding_model_dims": 384,
                "on_disk": True,
            },
        },
        "llm": {
            "provider": "vllm",
            "config": {
                "model": llm_model,
                "vllm_base_url": llm_endpoint,
                "api_key": "vllm-local",
                "temperature": 0.0,
                "max_tokens": 1024,
            },
        },
        "embedder": {
            "provider": "huggingface",
            "config": {
                "model": embedding_model,
                "embedding_dims": 384,
            },
        },
        "history_db_path": str(data_dir / "history.db"),
        "version": "v1.1",
    }
    return Memory.from_config(config)


def run_mem0(facts, queries, *, data_dir, llm_endpoint, llm_model, embedding_model,
             k, max_questions, summarize, rank_of):
    if data_dir is None:
        raise SystemExit("--mem0-data-dir is required for the mem0 adapter")
    if data_dir.exists():
        shutil.rmtree(data_dir)
    data_dir.mkdir(parents=True)

    memory = build_memory(data_dir, llm_endpoint, llm_model, embedding_model)

    seed_start = time.perf_counter()
    for fact in facts:
        memory.add(
            messages=[{"role": "user", "content": fact.text}],
            user_id=fact.sample_id,
            metadata={"dia_id": fact.dia_id, "source": "locomo"},
        )
    seed_ms = (time.perf_counter() - seed_start) * 1000.0

    rows = []
    for idx, query in enumerate(queries):
        if max_questions and idx >= max_questions:
            break
        started = time.perf_counter()
        result = memory.search(
            query=query.question,
            filters={"user_id": query.sample_id},
            limit=k,
        )
        latency_ms = (time.perf_counter() - started) * 1000.0

        items = result.get("results") if isinstance(result, dict) else result
        ranked, seen = [], set()
        for item in items or []:
            metadata = item.get("metadata") or {}
            dia_id = str(metadata.get("dia_id") or "")
            if dia_id and dia_id not in seen:
                seen.add(dia_id)
                ranked.append(dia_id)
            if len(ranked) >= k:
                break
        rank = rank_of(ranked, query.evidence, k)
        rows.append(
            {
                "sample_id": query.sample_id,
                "question": query.question,
                "category": query.category,
                "evidence": list(query.evidence),
                "ranked_ids": ranked,
                "rank": rank,
                "latency_ms": latency_ms,
            }
        )

    return summarize("mem0", rows, seed_ms, len(facts))
