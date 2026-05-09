#!/usr/bin/env python3
"""Write one memory.metrics snapshot to stdout."""

from __future__ import annotations

import json
import urllib.request


URL = "http://127.0.0.1:8787/mcp"


def main() -> int:
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "memory.metrics",
            "arguments": {"include_recent": False, "include_tiered": False},
        },
    }
    raw = urllib.request.urlopen(
        urllib.request.Request(
            URL,
            data=json.dumps(req).encode(),
            headers={"content-type": "application/json"},
        ),
        timeout=30,
    ).read()
    obj = json.loads(raw)
    if "error" in obj:
        raise RuntimeError(obj["error"])
    text = obj["result"]["content"][0]["text"]
    print(json.dumps(json.loads(text), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
