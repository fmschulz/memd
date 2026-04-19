#!/usr/bin/env python3
"""Populate memd with the v2 bench gold facts.

Idempotent: the script writes to dedicated bench tenants
(`bench_v2_alpha`, `bench_v2_beta`, `bench_v2_gamma`) and tags each
chunk so a second run is a no-op (we skip facts whose exact text is
already present under the tag).

Requires the local memd daemon on http://127.0.0.1:8787/mcp.
"""
import json
import pathlib
import sys
import urllib.error
import urllib.request

BENCH = pathlib.Path(__file__).resolve().parent.parent
# Gold facts live OUTSIDE the repo tree so agents under any bench
# fixture cwd cannot grep-cheat their way to the answers. Benchmark
# runners need read access to ~/.local/share/memd-bench-v2/.
GOLD_FACTS_PATH = pathlib.Path.home() / ".local/share/memd-private-artifacts-z7k/gold_facts.json"
FACTS = json.load(open(GOLD_FACTS_PATH))
DAEMON = "http://127.0.0.1:8787/mcp"


def call_tool(name: str, args: dict) -> dict:
    payload = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }
    ).encode()
    req = urllib.request.Request(DAEMON, data=payload, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            body = json.loads(r.read())
    except urllib.error.URLError as e:
        raise SystemExit(f"memd daemon unreachable at {DAEMON}: {e}")
    if "error" in body:
        raise SystemExit(f"tool {name} error: {body['error']}")
    text = body["result"]["content"][0]["text"]
    return json.loads(text)


def already_seeded(tenant: str, project: str, fact_id: str) -> str | None:
    """Return chunk_id if a chunk tagged with this fact_id exists."""
    out = call_tool(
        "memory.search",
        {
            "tenant_id": tenant,
            "query": fact_id,
            "k": 5,
            "filters": {"types": ["decision"]},
        },
    )
    for r in out.get("results", []):
        tags = r.get("tags") or []
        if any(t == f"bench_v2_fact:{fact_id}" for t in tags):
            return r.get("chunk_id")
    return None


def main() -> None:
    written = 0
    skipped = 0
    for tenant, spec in FACTS["tenants"].items():
        project = spec["project_id"]
        for fact in spec["facts"]:
            existing = already_seeded(tenant, project, fact["fact_id"])
            if existing:
                print(f"  = {tenant}/{fact['fact_id']} (already seeded: {existing})")
                skipped += 1
                continue
            out = call_tool(
                "memory.add",
                {
                    "tenant_id": tenant,
                    "project_id": project,
                    "text": fact["text"],
                    "type": "decision",
                    "tags": [
                        f"bench_v2_fact:{fact['fact_id']}",
                        f"bench_v2_category:{fact['category']}",
                        "benchmark:v2-xproject",
                    ],
                },
            )
            print(f"  + {tenant}/{fact['fact_id']} -> {out.get('chunk_id')}")
            written += 1
    print(f"\nwritten={written} skipped={skipped}")


if __name__ == "__main__":
    main()
