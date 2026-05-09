#!/usr/bin/env python3
"""Minimal stdio MCP facade exposing one read-only memd_search tool."""

from __future__ import annotations

import json
import os
import sys
import traceback
from typing import Any

from memd_search import DEFAULT_URL, call_mcp_tool, write_log


PROTOCOL_VERSION = "2025-11-25"
SERVER_NAME = "memd-thin-search"
TOOL_NAME = "memd_search"


def respond(request_id: Any, result: dict[str, Any] | None = None, error: dict[str, Any] | None = None) -> None:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": request_id}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result or {}
    print(json.dumps(msg, separators=(",", ":")), flush=True)


def tool_schema() -> dict[str, Any]:
    return {
        "name": TOOL_NAME,
        "description": "Read-only compact memd memory.search against the local daemon.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string"},
                "project_id": {"type": "string"},
                "query": {"type": "string"},
                "k": {"type": "integer", "default": 5},
                "compact": {"type": "boolean", "default": True},
                "token_budget": {"type": "integer", "default": 1200},
                "mode": {
                    "type": "string",
                    "enum": [
                        "generic",
                        "brief_project",
                        "resume_task",
                        "find_failures",
                        "find_decisions",
                        "find_evidence",
                        "find_highlights",
                    ],
                    "default": "generic",
                },
                "include_text": {"type": "boolean", "default": True},
            },
            "required": ["tenant_id", "query"],
            "additionalProperties": False,
        },
    }


def normalize_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    out = {
        "tenant_id": arguments["tenant_id"],
        "query": arguments["query"],
        "k": int(arguments.get("k") or 5),
        "compact": bool(arguments.get("compact", True)),
        "token_budget": int(arguments.get("token_budget") or 1200),
        "mode": arguments.get("mode") or "generic",
        "include_text": bool(arguments.get("include_text", True)),
    }
    if arguments.get("project_id"):
        out["project_id"] = arguments["project_id"]
    return out


def handle_request(obj: dict[str, Any]) -> None:
    method = obj.get("method")
    request_id = obj.get("id")

    if request_id is None and method in {"initialized", "notifications/initialized", "notifications/cancelled"}:
        return

    if method == "initialize":
        respond(
            request_id,
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": "0.1.0"},
            },
        )
        return

    if method in {"initialized", "notifications/initialized", "notifications/cancelled"}:
        return

    if method == "ping":
        respond(request_id, {})
        return

    if method == "tools/list":
        respond(request_id, {"tools": [tool_schema()]})
        return

    if method == "tools/call":
        params = obj.get("params") or {}
        if params.get("name") != TOOL_NAME:
            respond(
                request_id,
                error={
                    "code": -32602,
                    "message": f"thin facade only exposes {TOOL_NAME}",
                },
            )
            return
        arguments = normalize_arguments(params.get("arguments") or {})
        payload = call_mcp_tool(
            os.environ.get("MEMD_MCP_URL", DEFAULT_URL),
            "memory.search",
            arguments,
            int(os.environ.get("MEMD_THIN_TIMEOUT", "30")),
        )
        output = {
            "tool": "memory.search",
            "arguments": arguments,
            "result_count": len(payload.get("results") or []),
            "results": payload.get("results") or [],
            "budget_info": payload.get("budget_info"),
        }
        write_log(os.environ.get("MEMD_THIN_LOG_DIR"), output)
        text = json.dumps(output, sort_keys=True)
        respond(request_id, {"content": [{"type": "text", "text": text}]})
        return

    respond(request_id, error={"code": -32601, "message": f"method not found: {method}"})


def main() -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            handle_request(json.loads(line))
        except Exception as exc:  # pragma: no cover - defensive for MCP clients
            traceback.print_exc(file=sys.stderr)
            try:
                request_id = json.loads(line).get("id")
            except Exception:
                request_id = None
            if request_id is not None:
                respond(request_id, error={"code": -32603, "message": str(exc)})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
