"""SuperLocalMemory LoCoMo adapter (lexical-fallback mode).

Requires `pip install superlocalmemory`. SLM with embeddings ON deadlocks
under the LoCoMo workload due to its subprocess embedding-worker singleton.
This adapter runs the lexical-only fallback (provider="cloud" with empty
endpoint forces `is_available=False`) that successfully completes full
LoCoMo. See ../README.md for the limitation in detail.
"""

from __future__ import annotations

import os
import shutil
import time
from dataclasses import replace
from pathlib import Path


def configure_environment() -> None:
    os.environ.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
    os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
    pid_file = Path.home() / ".superlocalmemory" / ".embedding-worker.pid"
    pid_file.unlink(missing_ok=True)


def run_slm(facts, queries, *, data_dir, k, max_questions, summarize, rank_of):
    if data_dir is None:
        raise SystemExit("--slm-data-dir is required for the superlocalmemory adapter")
    configure_environment()

    from superlocalmemory.core.config import SLMConfig
    from superlocalmemory.core.engine import MemoryEngine
    from superlocalmemory.storage.models import AtomicFact, FactType, Mode

    if data_dir.exists():
        shutil.rmtree(data_dir)
    data_dir.mkdir(parents=True)

    cfg = SLMConfig.for_mode(Mode.A, base_dir=data_dir)
    cfg.active_profile = "default"
    cfg.db_path = data_dir / "memory.db"
    # Lexical-only fallback (see module docstring).
    cfg.embedding = replace(cfg.embedding, provider="cloud", api_endpoint="", api_key="")
    cfg.retrieval = replace(cfg.retrieval, use_cross_encoder=False)

    engine = MemoryEngine(cfg)
    engine.initialize()

    try:
        by_sample: dict[str, list] = {}
        for fact in facts:
            by_sample.setdefault(fact.sample_id, []).append(fact)

        seed_start = time.perf_counter()
        for sample_id, sample_facts in by_sample.items():
            engine.db.execute(
                "INSERT OR IGNORE INTO profiles (profile_id, name, mode) VALUES (?, ?, ?)",
                (sample_id, sample_id, "a"),
            )
            engine.profile_id = sample_id
            with engine.db.transaction():
                for fact in sample_facts:
                    atomic = AtomicFact(
                        fact_id=f"{sample_id}::{fact.dia_id}",
                        profile_id=sample_id,
                        content=fact.content,
                        fact_type=FactType.EPISODIC,
                        session_id=sample_id,
                        source_turn_ids=[fact.dia_id],
                        confidence=1.0,
                        importance=0.5,
                    )
                    engine.store_fact_direct(atomic)
        seed_ms = (time.perf_counter() - seed_start) * 1000.0

        rows = []
        for idx, query in enumerate(queries):
            if max_questions and idx >= max_questions:
                break
            started = time.perf_counter()
            response = engine.recall(
                query.question, profile_id=query.sample_id, limit=k, fast=False
            )
            latency_ms = (time.perf_counter() - started) * 1000.0
            ranked_internal = [str(r.fact.fact_id) for r in response.results]
            ranked = [fid.split("::", 1)[1] if "::" in fid else fid for fid in ranked_internal]
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

        summary = summarize("superlocalmemory", rows, seed_ms, len(facts))
        summary["slm_mode"] = "lexical_fallback"
        return summary
    finally:
        engine.close()
