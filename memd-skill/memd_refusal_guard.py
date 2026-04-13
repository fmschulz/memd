#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen


RETRIEVAL_TOOLS = {
    "memory.search",
    "task.search",
    "artifact.search",
    "artifact.verify",
    "task.resume",
    "task.get",
    "artifact.get",
    "context.brief_project",
    "context.find_relevant_context",
    "context.search_context_documents",
    "context.get_hot_context",
    "artifact.find_failures",
    "artifact.find_decisions",
    "artifact.find_evidence",
    "artifact.find_highlights",
}

REFUSAL_PATTERNS = [
    re.compile(pattern, re.IGNORECASE | re.MULTILINE)
    for pattern in [
        r"\b(?:i|we)\s+can(?:not|'t)\b",
        r"\b(?:i|we)\s+am\s+unable\s+to\b",
        r"\bnot\s+possible\b",
        r"\bimpossible\b",
        r"\bblocked\b",
        r"\bcannot\s+proceed\b",
        r"\bcan't\s+proceed\b",
        r"\bcannot\s+answer\b",
        r"\bcan't\s+answer\b",
        r"\bdo\s+not\s+have\s+enough\s+information\b",
        r"\bdon't\s+have\s+enough\s+information\b",
        r"\bneed\s+more\s+information\b",
        r"\bno\s+relevant\s+record\s+was\s+found\b",
    ]
]


def _iso8601(ts: datetime) -> str:
    return ts.astimezone(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def _mcp_http_call(url: str, method: str, params: dict) -> dict:
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    }
    req = Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urlopen(req, timeout=30) as resp:
        body = resp.read().decode("utf-8")
    body = body.strip()
    if body.startswith("data:"):
        body = "\n".join(
            line[5:].strip()
            for line in body.splitlines()
            if line.startswith("data:") and line[5:].strip()
        )
    raw = json.loads(body)
    if "result" in raw:
        return raw["result"]
    return raw


def _parse_tool_payload(result: dict) -> dict:
    text = result["content"][0]["text"]
    return json.loads(text)


def _assert_memd_ready(url: str) -> None:
    _mcp_http_call(
        url,
        "initialize",
        {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "memd-refusal-guard", "version": "0.1.0"},
        },
    )


def _find_tool_calls(
    url: str,
    tenant_id: str,
    time_from: str,
    time_to: str,
    session_id: str | None = None,
) -> list[dict]:
    result = _mcp_http_call(
        url,
        "tools/call",
        {
            "name": "debug.find_tool_calls",
            "arguments": {
                "tenant_id": tenant_id,
                "time_from": time_from,
                "time_to": time_to,
                "session_id": session_id,
                "limit": 200,
            },
        },
    )
    payload = _parse_tool_payload(result)
    return payload.get("tool_calls", [])


def _looks_like_refusal(text: str) -> bool:
    normalized = text.strip()
    if not normalized:
        return False
    return any(pattern.search(normalized) for pattern in REFUSAL_PATTERNS)


def _guard_prompt(session_id: str, tenant_id: str) -> str:
    return (
        "memd refusal guard for this run:\n"
        f"- For substantive work, do not say the task is impossible, blocked, or cannot be answered until you have checked memd.\n"
        f"- Use tenant_id \"{tenant_id}\" for memd calls unless the user explicitly provides a different tenant scope.\n"
        f"- If you use task.search or artifact.search, include session_id \"{session_id}\" so the run can be audited.\n"
        "- If trust matters, use artifact.verify before concluding that no grounded support exists.\n"
        "- If memd has nothing relevant, say explicitly that you checked memd and found no relevant record.\n"
    )


def _parse_codex_args(raw_args: list[str]) -> tuple[list[str], str]:
    args = list(raw_args)
    if args and args[0] == "exec":
        args = args[1:]

    unsupported = {"review", "resume", "help"}
    if args and args[0] in unsupported:
        raise SystemExit("codex-memd-guard currently supports only codex exec-style runs.")

    value_opts = {
        "-c",
        "--config",
        "-i",
        "--image",
        "-m",
        "--model",
        "-s",
        "--sandbox",
        "-p",
        "--profile",
        "-C",
        "--cd",
        "--add-dir",
        "-o",
        "--output-last-message",
        "--output-schema",
        "--color",
    }
    flag_opts = {
        "--enable",
        "--disable",
        "--full-auto",
        "--dangerously-bypass-approvals-and-sandbox",
        "--skip-git-repo-check",
        "--ephemeral",
        "--json",
    }

    parsed: list[str] = []
    prompt_parts: list[str] = []
    i = 0
    while i < len(args):
        token = args[i]
        if token == "--":
            prompt_parts.extend(args[i + 1 :])
            break
        if token in value_opts:
            if i + 1 >= len(args):
                raise SystemExit(f"missing value for {token}")
            parsed.extend([token, args[i + 1]])
            i += 2
            continue
        if token in flag_opts or token.startswith("--enable=") or token.startswith("--disable="):
            parsed.append(token)
            i += 1
            continue
        if token.startswith("-"):
            parsed.append(token)
            i += 1
            continue
        prompt_parts.extend(args[i:])
        break

    prompt = " ".join(prompt_parts).strip()
    if not prompt and not sys.stdin.isatty():
        prompt = sys.stdin.read().strip()
    if not prompt:
        raise SystemExit("codex-memd-guard needs a prompt argument or stdin content.")
    return parsed, prompt


def _run_codex(url: str, tenant_id: str, session_id: str, raw_args: list[str]) -> tuple[int, str, str, str]:
    args, user_prompt = _parse_codex_args(raw_args)
    guard_prompt = _guard_prompt(session_id, tenant_id)
    combined_prompt = f"{guard_prompt}\n\nUser request:\n{user_prompt}\n"

    output_path = None
    for idx, token in enumerate(args):
        if token in {"-o", "--output-last-message"} and idx + 1 < len(args):
            output_path = args[idx + 1]
            break

    tmp_last = None
    if output_path is None:
        tmp_last = tempfile.NamedTemporaryFile(prefix="codex-memd-guard-", suffix=".txt", delete=False)
        tmp_last.close()
        output_path = tmp_last.name
        args.extend(["-o", output_path])

    cmd = [
        "codex",
        "exec",
        "-c",
        f'mcp_servers.memd.url="{url}"',
        *args,
        "-",
    ]
    proc = subprocess.run(
        cmd,
        input=combined_prompt,
        text=True,
        capture_output=True,
    )
    last_message = ""
    if output_path and Path(output_path).exists():
        last_message = Path(output_path).read_text(encoding="utf-8").strip()
    if not last_message:
        last_message = proc.stdout.strip()
    if tmp_last is not None:
        Path(tmp_last.name).unlink(missing_ok=True)
    return proc.returncode, proc.stdout, proc.stderr, last_message


def _run_claude(url: str, tenant_id: str, session_id: str, raw_args: list[str]) -> tuple[int, str, str, str]:
    args = list(raw_args)
    if "-p" not in args and "--print" not in args:
        raise SystemExit("claude-memd-guard currently supports only --print / -p mode.")
    if "--mcp-config" in args or "--strict-mcp-config" in args:
        raise SystemExit("claude-memd-guard manages --mcp-config itself; do not pass it explicitly.")

    prompt = ""
    if args and not args[-1].startswith("-"):
        prompt = args.pop()
    if not prompt and not sys.stdin.isatty():
        prompt = sys.stdin.read()

    guard_prompt = _guard_prompt(session_id, tenant_id)
    with tempfile.NamedTemporaryFile(prefix="claude-memd-guard-", suffix=".json", mode="w", delete=False) as fh:
        json.dump({"mcpServers": {"memd": {"type": "http", "url": url}}}, fh)
        config_path = fh.name

    cmd = [
        "claude",
        *args,
        "--append-system-prompt",
        guard_prompt,
        "--strict-mcp-config",
        "--mcp-config",
        config_path,
    ]
    if prompt:
        cmd.append(prompt)
    proc = subprocess.run(cmd, text=True, capture_output=True)
    Path(config_path).unlink(missing_ok=True)
    return proc.returncode, proc.stdout, proc.stderr, proc.stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser(description="Guard refusal-style agent outputs unless memd was consulted first.")
    parser.add_argument("tool", choices=["codex", "claude"])
    parser.add_argument("--tenant-id", default=os.environ.get("MEMD_GUARD_TENANT_ID", "default"))
    parser.add_argument("--url", default=os.environ.get("MEMD_URL", "http://127.0.0.1:8787/mcp"))
    parser.add_argument("tool_args", nargs=argparse.REMAINDER)
    ns = parser.parse_args()

    tool_args = list(ns.tool_args)
    if tool_args and tool_args[0] == "--":
        tool_args = tool_args[1:]

    try:
        _assert_memd_ready(ns.url)
    except URLError as exc:
        print(f"memd refusal guard could not reach {ns.url}: {exc}", file=sys.stderr)
        return 2
    except Exception as exc:  # pragma: no cover - defensive CLI surface
        print(f"memd refusal guard failed to initialize memd at {ns.url}: {exc}", file=sys.stderr)
        return 2

    session_id = f"memd-guard-{uuid.uuid4()}"
    start = datetime.now(timezone.utc) - timedelta(seconds=2)

    if ns.tool == "codex":
        code, stdout_text, stderr_text, final_text = _run_codex(
            ns.url, ns.tenant_id, session_id, tool_args
        )
    else:
        code, stdout_text, stderr_text, final_text = _run_claude(
            ns.url, ns.tenant_id, session_id, tool_args
        )

    if stdout_text:
        sys.stdout.write(stdout_text)
    if stderr_text:
        sys.stderr.write(stderr_text)
    if code != 0:
        return code

    if not _looks_like_refusal(final_text):
        return 0

    end = datetime.now(timezone.utc) + timedelta(seconds=2)
    try:
        session_calls = _find_tool_calls(
            ns.url,
            ns.tenant_id,
            _iso8601(start),
            _iso8601(end),
            session_id=session_id,
        )
        tool_calls = session_calls
        if not {call.get("tool_name", "") for call in session_calls} & RETRIEVAL_TOOLS:
            tool_calls = _find_tool_calls(ns.url, ns.tenant_id, _iso8601(start), _iso8601(end))
    except Exception as exc:  # pragma: no cover - defensive CLI surface
        print(
            f"memd refusal guard could not verify tool usage after a refusal-like answer: {exc}",
            file=sys.stderr,
        )
        return 3

    seen = {call.get("tool_name", "") for call in tool_calls}
    if seen & RETRIEVAL_TOOLS:
        return 0

    print(
        "memd refusal guard blocked the answer: refusal-style output was produced but no memd retrieval call "
        f"was observed for tenant '{ns.tenant_id}' between {_iso8601(start)} and {_iso8601(end)}. "
        "Retry after consulting memd first.",
        file=sys.stderr,
    )
    return 3


if __name__ == "__main__":
    raise SystemExit(main())
