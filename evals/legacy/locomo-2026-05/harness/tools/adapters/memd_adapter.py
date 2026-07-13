"""memd LoCoMo adapter — uses the memd batch CLI in streaming JSONL mode.

Seeds via `memory.add_batch` chunks tagged with `locomo:dia_id:<dia_id>` per
turn; recalls via `memory.search` and extracts the dia_id from result tags.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Callable


class MemdBatchClient:
    def __init__(self, memd_path: Path, data_dir: Path, stderr_log: Path) -> None:
        self.proc = subprocess.Popen(
            [
                str(memd_path),
                "--data-dir", str(data_dir),
                "--embedding-model", "all-minilm",
                "--search-variant", "hybrid-feature",
                "batch", "--jsonl", "-", "--stream",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr_log.open("w", encoding="utf-8"),
            text=True,
            bufsize=1,
            env={**os.environ, "RUST_LOG": "error"},
        )

    def request(self, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        payload = {"tool": tool, "arguments": arguments}
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("memd batch exited before responding")
        row = json.loads(line)
        if not row.get("ok"):
            raise RuntimeError(f"memd {tool} failed: {row}")
        return row.get("result") or {}

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        self.proc.wait(timeout=30)


def run_memd(facts, queries, memd_bin, k, max_questions, summarize, batch_size=128):
    if memd_bin is None or not Path(memd_bin).exists():
        raise SystemExit(f"--memd-bin {memd_bin} not found; build with `cargo build --release -p memd`")

    tmp_root = Path(tempfile.mkdtemp(prefix="memd_locomo_"))
    data_dir = tmp_root / "data"
    data_dir.mkdir(parents=True)
    stderr_log = tmp_root / "memd.stderr.log"

    client = MemdBatchClient(Path(memd_bin), data_dir, stderr_log)
    try:
        by_sample: dict[str, list] = {}
        for fact in facts:
            by_sample.setdefault(fact.sample_id, []).append(fact)

        seed_start = time.perf_counter()
        for sample_id, sample_facts in by_sample.items():
            tenant_id = f"locomo_{sample_id}".replace("-", "_")
            for offset in range(0, len(sample_facts), batch_size):
                batch = sample_facts[offset : offset + batch_size]
                chunks = [
                    {
                        "text": fact.content,
                        "type": "message",
                        "tags": [
                            "benchmark:locomo",
                            f"locomo:sample:{sample_id}",
                            f"locomo:dia_id:{fact.dia_id}",
                        ],
                    }
                    for fact in batch
                ]
                client.request("memory.add_batch", {"tenant_id": tenant_id, "chunks": chunks})
        seed_ms = (time.perf_counter() - seed_start) * 1000.0

        rows = []
        for idx, query in enumerate(queries):
            if max_questions and idx >= max_questions:
                break
            tenant_id = f"locomo_{query.sample_id}".replace("-", "_")
            started = time.perf_counter()
            result = client.request(
                "memory.search",
                {"tenant_id": tenant_id, "query": query.question, "k": k},
            )
            latency_ms = (time.perf_counter() - started) * 1000.0
            ranked = []
            for item in result.get("results") or []:
                tags = item.get("tags") or []
                dia_tags = [tag.removeprefix("locomo:dia_id:") for tag in tags if tag.startswith("locomo:dia_id:")]
                ranked.append(dia_tags[0] if dia_tags else "")
            # Rank-of imported via summarize's bench_runner caller; inline a copy here.
            rank = next((i + 1 for i, dia in enumerate(ranked[:k]) if dia in query.evidence), None)
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
        return summarize("memd", rows, seed_ms, len(facts))
    finally:
        client.close()
        shutil.rmtree(tmp_root, ignore_errors=True)
