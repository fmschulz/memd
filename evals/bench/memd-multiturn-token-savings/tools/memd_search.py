#!/usr/bin/env python3
"""Read-only compact memory.search wrapper for interface benchmarks."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import time
import urllib.request
from typing import Any


DEFAULT_URL = "http://127.0.0.1:8787/mcp"


def call_mcp_tool(url: str, name: str, arguments: dict[str, Any], timeout: int) -> dict[str, Any]:
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    raw = urllib.request.urlopen(
        urllib.request.Request(
            url,
            data=json.dumps(req).encode(),
            headers={"content-type": "application/json"},
        ),
        timeout=timeout,
    ).read()
    obj = json.loads(raw)
    if "error" in obj:
        raise RuntimeError(obj["error"])
    text = obj["result"]["content"][0]["text"]
    return json.loads(text)


def build_search_args(args: argparse.Namespace) -> dict[str, Any]:
    search_args: dict[str, Any] = {
        "tenant_id": args.tenant_id,
        "query": args.query,
        "k": args.k,
        "compact": True,
        "token_budget": args.token_budget,
    }
    if args.project_id:
        search_args["project_id"] = args.project_id
    if args.mode:
        search_args["mode"] = args.mode
    if args.include_text:
        search_args["include_text"] = True
    return search_args


def normalize_payload(search_args: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
    results = payload.get("results") or []
    return {
        "tool": "memory.search",
        "arguments": search_args,
        "result_count": len(results),
        "results": results,
        "budget_info": payload.get("budget_info"),
    }


def write_log(log_dir: str | None, payload: dict[str, Any]) -> None:
    if not log_dir:
        return
    path = pathlib.Path(log_dir)
    path.mkdir(parents=True, exist_ok=True)
    stamp = int(time.time() * 1000)
    out = path / f"memd_search_{stamp}.json"
    out.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    with (path / "memd_search_log.jsonl").open("a") as handle:
        handle.write(json.dumps(payload, sort_keys=True) + "\n")


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--tenant-id", required=True)
    p.add_argument("--project-id")
    p.add_argument("--query", required=True)
    p.add_argument("--k", type=int, default=5)
    p.add_argument("--token-budget", type=int, default=1200)
    p.add_argument(
        "--mode",
        choices=(
            "generic",
            "brief_project",
            "resume_task",
            "find_failures",
            "find_decisions",
            "find_evidence",
            "find_highlights",
        ),
        default="generic",
    )
    p.add_argument("--include-text", action="store_true", default=True)
    p.add_argument("--url", default=os.environ.get("MEMD_MCP_URL", DEFAULT_URL))
    p.add_argument("--timeout", type=int, default=30)
    p.add_argument("--log-dir")
    p.add_argument("--pretty", action="store_true")
    return p


def main() -> int:
    args = parser().parse_args()
    search_args = build_search_args(args)
    payload = call_mcp_tool(args.url, "memory.search", search_args, args.timeout)
    output = normalize_payload(search_args, payload)
    write_log(args.log_dir, output)
    if args.pretty:
        print(json.dumps(output, indent=2, sort_keys=True))
    else:
        print(json.dumps(output, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
