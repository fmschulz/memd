"""Seed memd stores with LoCoMo conversation turns.

One tenant ("locomo"), one project per conversation ("conv-<sample_id>").
Each turn becomes exactly one chunk (chunk_type "message"); a seed-time
mapping chunk_id -> dia_id is written per run, so scoring never depends on
search-payload metadata fields.

Usage: python3 seed.py <run_dir> [--fmt plain|dated]
"""

import json
import sys
import time
from pathlib import Path

import common


def seed(run_dir: Path, fmt: str):
    data = common.load_dataset()
    store_dir = run_dir / "store"
    store_dir.mkdir(parents=True, exist_ok=True)

    mapping = {}
    total = 0
    seed_seconds = {}
    for conv in data:
        sample_id = conv["sample_id"]
        project = f"conv-{sample_id}"
        requests = []
        dia_order = []
        for _key, session_dt, turn in common.iter_turns(conv["conversation"]):
            text = common.turn_text(turn, session_dt, fmt)
            if not text.strip():
                continue
            requests.append(
                {
                    "tool": "memory.add",
                    "arguments": {
                        "tenant_id": common.TENANT,
                        "project_id": project,
                        "type": "message",
                        "text": text,
                    },
                }
            )
            dia_order.append(turn["dia_id"])
        start = time.monotonic()
        rows, _times = common.run_batch(requests, store_dir)
        seed_seconds[sample_id] = round(time.monotonic() - start, 2)
        if len(rows) != len(requests):
            sys.exit(f"{project}: {len(rows)} responses for {len(requests)} adds")
        for dia_id, row in zip(dia_order, rows):
            if not row.get("ok"):
                sys.exit(f"{project}: add failed: {json.dumps(row)[:400]}")
            chunk_id = (
                row.get("result", {}).get("chunk_id")
                or row.get("chunk_id")
                or (row.get("result", {}).get("chunk_ids") or [None])[0]
            )
            if not chunk_id:
                sys.exit(f"{project}: no chunk_id in response: {json.dumps(row)[:400]}")
            mapping[chunk_id] = dia_id
        total += len(requests)
        print(f"seeded {project}: {len(requests)} turns in {seed_seconds[sample_id]}s")

    (run_dir / "chunk_to_dia.json").write_text(json.dumps(mapping))
    (run_dir / "seed_meta.json").write_text(
        json.dumps(
            {
                "format": fmt,
                "total_turns": total,
                "seed_seconds_per_conversation": seed_seconds,
                "seed_seconds_total": round(sum(seed_seconds.values()), 2),
                "memd_version": common.memd_version(run_dir / "store"),
            },
            indent=2,
        )
    )
    print(f"total: {total} turns, {round(sum(seed_seconds.values()), 1)}s")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    run_dir = Path(sys.argv[1]).resolve()
    fmt = "plain"
    if "--fmt" in sys.argv:
        fmt = sys.argv[sys.argv.index("--fmt") + 1]
    if fmt not in ("plain", "dated"):
        sys.exit(f"unknown fmt: {fmt}")
    seed(run_dir, fmt)
