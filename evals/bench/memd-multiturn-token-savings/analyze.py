#!/usr/bin/env python3
"""Analyze multi-turn token-savings benchmark transcripts."""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import re
from collections import Counter
from statistics import median
from typing import Any


BENCH = pathlib.Path(__file__).resolve().parent
TOKEN_FOOTER = re.compile(r"^tokens used\s*\n\s*([0-9][0-9,]*)", re.MULTILINE)
MCP_STARTED = re.compile(r"^mcp: ([^/]+)/([^\s]+) started", re.MULTILINE)
MCP_COMPLETED = re.compile(r"^mcp: ([^/]+)/([^\s]+) \(completed\)", re.MULTILINE)
CLAUDE_MCP_TOOL = re.compile(r"^mcp__([^_]+)__(.+)$")
CLI_SEARCH_COMMAND = re.compile(
    r"(?:^|\s)(?:(?:python3\s+)?(?:\.bench/)?memd_search\.py|memd\s+agent-context)\b"
)

CONDITION_ALIASES = {"with": "full_mcp"}
MEMORY_CONDITIONS = ("full_mcp", "thin_mcp", "cli_search", "cli_prefetch")
MCP_PAYLOAD_CONDITIONS = ("full_mcp", "thin_mcp")
DEFAULT_CONDITIONS = "without,full_mcp,thin_mcp,cli_search,cli_prefetch,with"


def canonical_condition(condition: str) -> str:
    return CONDITION_ALIASES.get(condition, condition)


def load_episodes() -> dict[str, dict[str, Any]]:
    return {e["id"]: e for e in json.loads((BENCH / "episodes.json").read_text())["episodes"]}


def footer_tokens(text: str) -> int | None:
    matches = list(TOKEN_FOOTER.finditer(text))
    if not matches:
        return None
    return int(matches[-1].group(1).replace(",", ""))


def estimated_tokens(text: str) -> int:
    return math.ceil(len(text.encode("utf-8")) / 4)


def token_total(snapshot: dict[str, Any]) -> int:
    return int(snapshot.get("token_usage", {}).get("total", {}).get("estimated_total_tokens", 0))


def by_tool_token_components(snapshot: dict[str, Any]) -> dict[str, dict[str, int]]:
    by_tool = snapshot.get("token_usage", {}).get("by_tool", {})
    return {
        tool: {
            "request": int(stats.get("estimated_request_tokens", 0)),
            "response": int(stats.get("estimated_response_tokens", 0)),
            "total": int(stats.get("estimated_total_tokens", 0)),
        }
        for tool, stats in by_tool.items()
    }


def subtract_by_tool_components(
    pre: dict[str, Any],
    post: dict[str, Any],
) -> dict[str, dict[str, int]]:
    pre_tools = by_tool_token_components(pre)
    post_tools = by_tool_token_components(post)
    tools = set(pre_tools) | set(post_tools)
    out = {}
    for tool in tools:
        pre_stats = pre_tools.get(tool, {})
        post_stats = post_tools.get(tool, {})
        out[tool] = {
            key: post_stats.get(key, 0) - pre_stats.get(key, 0)
            for key in ("request", "response", "total")
        }
    return out


def claude_events(raw: str) -> list[dict[str, Any]]:
    events = []
    for line in raw.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            events.append(event)
    return events


def claude_model_usage_components(result_event: dict[str, Any] | None) -> dict[str, int] | None:
    if not result_event:
        return None
    model_usage = result_event.get("modelUsage") or {}
    if not model_usage:
        return None
    out = {"input": 0, "output": 0, "cache_read": 0, "cache_creation": 0}
    for usage in model_usage.values():
        out["input"] += int(usage.get("inputTokens") or 0)
        out["output"] += int(usage.get("outputTokens") or 0)
        out["cache_read"] += int(usage.get("cacheReadInputTokens") or 0)
        out["cache_creation"] += int(usage.get("cacheCreationInputTokens") or 0)
    out["total"] = sum(out.values())
    return out


def claude_usage_components(result_event: dict[str, Any] | None) -> dict[str, int] | None:
    if not result_event:
        return None
    usage = result_event.get("usage") or {}
    if not usage:
        return None
    out = {
        "input": int(usage.get("input_tokens") or 0),
        "output": int(usage.get("output_tokens") or 0),
        "cache_read": int(usage.get("cache_read_input_tokens") or 0),
        "cache_creation": int(usage.get("cache_creation_input_tokens") or 0),
    }
    out["total"] = sum(out.values())
    return out


def contains_any(text: str, markers: list[str]) -> bool:
    haystack = text.lower()
    return any(marker.lower() in haystack for marker in markers)


def after_first_completed_mcp(raw: str) -> str:
    marker = "(completed)"
    idx = raw.find(marker)
    if idx < 0:
        return ""
    return raw[idx + len(marker):]


def codex_mcp_tool(server: str, tool: str) -> str:
    return f"{server}/{tool}"


def claude_memd_tool(name: str) -> str | None:
    match = CLAUDE_MCP_TOOL.match(name)
    if not match:
        return None
    server, rest = match.groups()
    if server == "memd":
        namespace, _, tool = rest.partition("_")
        normalized = f"{namespace}.{tool}" if tool else rest
        return f"{server}/{normalized}"
    if server == "memdthin":
        return f"{server}/{rest}"
    return None


def claude_init_counts(events: list[dict[str, Any]]) -> dict[str, Any]:
    for event in events:
        if event.get("type") == "system" and event.get("subtype") == "init":
            tools = event.get("tools") or []
            memd_tools = [
                tool for tool in tools
                if tool.startswith("mcp__memd__") or tool.startswith("mcp__memdthin__")
            ]
            return {
                "visible_tool_count": len(tools),
                "visible_mcp_tool_count": sum(1 for tool in tools if tool.startswith("mcp__")),
                "visible_memd_tool_count": len(memd_tools),
                "visible_memd_tools": memd_tools,
                "mcp_servers": event.get("mcp_servers") or [],
            }
    return {
        "visible_tool_count": None,
        "visible_mcp_tool_count": None,
        "visible_memd_tool_count": None,
        "visible_memd_tools": [],
        "mcp_servers": [],
    }


def cli_retrieval_artifacts(
    results_dir: pathlib.Path,
    cell: str,
    workdir: str | None,
) -> dict[str, Any]:
    candidates = [results_dir / "retrieval" / cell]
    if workdir:
        candidates.append(pathlib.Path(workdir) / ".bench" / "memd-search-logs")
    files: list[pathlib.Path] = []
    for directory in candidates:
        if directory.exists():
            files = sorted(directory.glob("memd_search_*.json"))
            if files:
                break
    payloads = []
    texts = []
    for path in files:
        text = path.read_text(errors="replace")
        texts.append(text)
        try:
            payloads.append(json.loads(text))
        except json.JSONDecodeError:
            continue
    combined = "\n".join(texts)
    return {
        "cli_retrieval_calls": len(files),
        "cli_retrieval_output_bytes": len(combined.encode("utf-8")),
        "cli_retrieval_output_estimated_tokens": estimated_tokens(combined) if combined else 0,
        "cli_retrieval_result_count": sum(int(p.get("result_count") or 0) for p in payloads),
        "cli_retrieval_text": combined,
        "cli_retrieval_files": [str(path) for path in files],
    }


def base_row(
    results_dir: pathlib.Path,
    agent: str,
    condition: str,
    episode_id: str,
    episode: dict[str, Any],
) -> dict[str, Any] | None:
    cell = f"{agent}__{condition}__{episode_id}"
    run_path = results_dir / "runs" / f"{cell}.txt"
    final_path = results_dir / "final" / f"{cell}.txt"
    metadata_path = results_dir / "metadata" / f"{cell}.json"
    post_test_path = results_dir / "tests" / f"{cell}.txt"
    diff_path = results_dir / "diffs" / f"{cell}.diff"
    if not run_path.exists() and not metadata_path.exists():
        return None
    raw = run_path.read_text(errors="replace") if run_path.exists() else ""
    final = final_path.read_text(errors="replace") if final_path.exists() else ""
    metadata = json.loads(metadata_path.read_text()) if metadata_path.exists() else {}
    test_output = post_test_path.read_text(errors="replace") if post_test_path.exists() else ""
    diff_text = diff_path.read_text(errors="replace") if diff_path.exists() else ""
    interface_condition = canonical_condition(
        metadata.get("interface_condition") or metadata.get("condition") or condition
    )
    cli_artifacts = cli_retrieval_artifacts(results_dir, cell, metadata.get("workdir"))
    row = {
        "agent": agent,
        "condition": condition,
        "interface_condition": interface_condition,
        "requested_condition": metadata.get("requested_condition", condition),
        "episode_id": episode_id,
        "kind": episode["kind"],
        "cli_rc": metadata.get("cli_rc"),
        "test_rc": metadata.get("test_rc"),
        "tests_passed": metadata.get("test_rc") == 0,
        "elapsed_seconds": metadata.get("elapsed_seconds"),
        "estimated_transcript_tokens": estimated_tokens(raw),
        "patch_chars": len(diff_text),
        "test_output_chars": len(test_output),
        "final_chars": len(final),
        "final": final.strip(),
        "raw": raw,
        **{k: v for k, v in cli_artifacts.items() if k != "cli_retrieval_text"},
    }
    row["_retrieval_text"] = cli_artifacts["cli_retrieval_text"]
    row["_combined_text"] = "\n".join([raw, final, cli_artifacts["cli_retrieval_text"]])
    return row


def analyze_codex(
    results_dir: pathlib.Path,
    condition: str,
    episode_id: str,
    episode: dict[str, Any],
) -> dict[str, Any] | None:
    row = base_row(results_dir, "codex", condition, episode_id, episode)
    if row is None:
        return None
    raw = row["raw"]
    started = Counter(codex_mcp_tool(server, tool) for server, tool in MCP_STARTED.findall(raw))
    completed = Counter(codex_mcp_tool(server, tool) for server, tool in MCP_COMPLETED.findall(raw))
    row.update(
        {
            "provider_total_tokens": footer_tokens(raw),
            "token_source": "codex_footer",
            "agent_turns": sum(1 for line in raw.splitlines() if line == "codex"),
            "memd_tool_attempts": sum(started.values()),
            "memd_tool_calls": sum(completed.values()),
            "memd_tools": dict(sorted(completed.items())),
            "memd_tool_attempts_by_tool": dict(sorted(started.items())),
            "shell_calls": sum(1 for line in raw.splitlines() if line == "exec"),
            "cli_search_command_mentions": len(CLI_SEARCH_COMMAND.findall(raw)),
        }
    )
    row["_post_mcp_text"] = after_first_completed_mcp(raw)
    row["retrieval_correct"] = retrieval_correct(row, episode)
    return row


def analyze_claude(
    results_dir: pathlib.Path,
    condition: str,
    episode_id: str,
    episode: dict[str, Any],
) -> dict[str, Any] | None:
    row = base_row(results_dir, "claude", condition, episode_id, episode)
    if row is None:
        return None
    raw = row["raw"]
    events = claude_events(raw)
    result_events = [event for event in events if event.get("type") == "result"]
    result_event = result_events[-1] if result_events else None
    final = row["final"] or ((result_event or {}).get("result") or "")
    tool_uses: dict[str, str] = {}
    shell_calls = 0
    for event in events:
        if event.get("type") != "assistant":
            continue
        message = event.get("message") or {}
        for item in message.get("content") or []:
            if not isinstance(item, dict) or item.get("type") != "tool_use":
                continue
            name = item.get("name") or ""
            tool_id = item.get("id") or ""
            if tool_id:
                tool_uses[tool_id] = name
            if name == "Bash":
                shell_calls += 1
    memd_tools: Counter[str] = Counter()
    for name in tool_uses.values():
        memd_name = claude_memd_tool(name)
        if memd_name:
            memd_tools[memd_name] += 1
    usage = claude_usage_components(result_event)
    model_usage = claude_model_usage_components(result_event)
    init = claude_init_counts(events)
    row["_combined_text"] += "\n" + final
    row["_post_mcp_text"] = ""
    row.update(
        {
            "provider_total_tokens": (model_usage or usage or {}).get("total"),
            "token_source": "claude_modelUsage",
            "claude_main_tokens": (usage or {}).get("total"),
            "claude_input_tokens": (usage or {}).get("input"),
            "claude_cache_creation_input_tokens": (usage or {}).get("cache_creation"),
            "claude_cache_read_input_tokens": (usage or {}).get("cache_read"),
            "claude_output_tokens": (usage or {}).get("output"),
            "claude_model_input_tokens": (model_usage or {}).get("input"),
            "claude_model_cache_creation_input_tokens": (model_usage or {}).get("cache_creation"),
            "claude_model_cache_read_input_tokens": (model_usage or {}).get("cache_read"),
            "claude_model_output_tokens": (model_usage or {}).get("output"),
            "claude_cost_usd": (result_event or {}).get("total_cost_usd"),
            "agent_turns": (result_event or {}).get("num_turns"),
            "memd_tool_calls": sum(memd_tools.values()),
            "memd_tools": dict(sorted(memd_tools.items())),
            "shell_calls": shell_calls,
            "cli_search_command_mentions": len(CLI_SEARCH_COMMAND.findall(raw)),
            **init,
        }
    )
    row["retrieval_correct"] = retrieval_correct(row, episode)
    return row


def retrieval_correct(row: dict[str, Any], episode: dict[str, Any]) -> bool:
    interface = row["interface_condition"]
    if interface == "without":
        return False
    markers = episode.get("retrieval_markers", [])
    evidence_markers = [marker for marker in markers if marker != episode.get("experience_id")]
    if not evidence_markers:
        evidence_markers = markers
    if interface in ("full_mcp", "thin_mcp"):
        used_retrieval = row.get("memd_tool_calls", 0) > 0
        retrieval_text = row.get("_retrieval_text", "")
        if retrieval_text:
            evidence_text = retrieval_text
        elif interface == "full_mcp" and row.get("_post_mcp_text"):
            evidence_text = row.get("_post_mcp_text", "")
            if episode.get("experience_id"):
                evidence_markers = evidence_markers + [episode["experience_id"]]
        else:
            evidence_text = row.get("_combined_text", "")
    elif interface in ("cli_search", "cli_prefetch"):
        used_retrieval = row.get("cli_retrieval_calls", 0) > 0
        evidence_text = row.get("_retrieval_text", "")
    else:
        used_retrieval = False
        evidence_text = ""
    return bool(used_retrieval and contains_any(evidence_text, evidence_markers))


def add_memd_metric_delta(results_dir: pathlib.Path, row: dict[str, Any]) -> None:
    if row["interface_condition"] == "without":
        row["memd_request_payload_tokens"] = 0
        row["memd_response_payload_tokens"] = 0
        row["memd_total_payload_tokens"] = 0
        row["memd_payload_components_by_tool"] = {}
        return
    cell = f"{row['agent']}__{row['condition']}__{row['episode_id']}"
    pre_path = results_dir / "metrics" / f"{cell}__pre.json"
    post_path = results_dir / "metrics" / f"{cell}__post.json"
    if not pre_path.exists() or not post_path.exists():
        row["memd_request_payload_tokens"] = 0
        row["memd_response_payload_tokens"] = 0
        row["memd_total_payload_tokens"] = 0
        row["memd_payload_components_by_tool"] = {}
        return
    pre = json.loads(pre_path.read_text())
    post = json.loads(post_path.read_text())
    components = subtract_by_tool_components(pre, post)
    memory_search = components.get("memory.search", {"request": 0, "response": 0, "total": 0})
    row["memd_raw_total_delta"] = token_total(post) - token_total(pre)
    row["memd_request_payload_tokens"] = max(0, memory_search["request"])
    row["memd_response_payload_tokens"] = max(0, memory_search["response"])
    row["memd_total_payload_tokens"] = max(0, memory_search["total"])
    row["memd_payload_components_by_tool"] = (
        {"memory.search": memory_search} if any(memory_search.values()) else {}
    )


def analyze_cell(
    results_dir: pathlib.Path,
    agent: str,
    condition: str,
    episode_id: str,
    episode: dict[str, Any],
) -> dict[str, Any] | None:
    if agent == "codex":
        row = analyze_codex(results_dir, condition, episode_id, episode)
    elif agent == "claude":
        row = analyze_claude(results_dir, condition, episode_id, episode)
    else:
        raise ValueError(agent)
    if row is not None:
        add_memd_metric_delta(results_dir, row)
        row.pop("raw", None)
        row.pop("_combined_text", None)
        row.pop("_retrieval_text", None)
        row.pop("_post_mcp_text", None)
    return row


def row_total_including_retrieval(row: dict[str, Any]) -> int | None:
    provider = row.get("provider_total_tokens")
    if provider is None:
        return None
    additive = (
        row.get("memd_total_payload_tokens") or 0
        if row["interface_condition"] in MCP_PAYLOAD_CONDITIONS
        else 0
    )
    return provider + additive


def paired(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    out = []
    by_pair: dict[tuple[str, str], dict[str, dict[str, Any]]] = {}
    for row in rows:
        by_pair.setdefault((row["agent"], row["episode_id"]), {})[row["interface_condition"]] = row
    for (agent, episode_id), pair in sorted(by_pair.items()):
        without_row = pair.get("without")
        if not without_row:
            continue
        without_tokens = without_row.get("provider_total_tokens")
        without_elapsed = without_row.get("elapsed_seconds")
        for interface in MEMORY_CONDITIONS:
            with_row = pair.get(interface)
            if not with_row:
                continue
            with_tokens = with_row.get("provider_total_tokens")
            additive_payload = (
                with_row.get("memd_total_payload_tokens") or 0
                if interface in MCP_PAYLOAD_CONDITIONS
                else 0
            )
            condition_total = row_total_including_retrieval(with_row)
            solver_savings = None
            net_savings = None
            net_savings_pct = None
            if with_tokens is not None and without_tokens is not None and condition_total is not None:
                solver_savings = without_tokens - with_tokens
                net_savings = without_tokens - condition_total
                net_savings_pct = net_savings / without_tokens if without_tokens else None
            elapsed = with_row.get("elapsed_seconds")
            elapsed_delta = None
            elapsed_savings_pct = None
            if elapsed is not None and without_elapsed is not None:
                elapsed_delta = without_elapsed - elapsed
                elapsed_savings_pct = elapsed_delta / without_elapsed if without_elapsed else None
            out.append(
                {
                    "agent": agent,
                    "episode_id": episode_id,
                    "interface_condition": interface,
                    "without_provider_tokens": without_tokens,
                    "condition_provider_tokens": with_tokens,
                    "additive_mcp_payload_tokens": additive_payload,
                    "cli_retrieval_output_estimated_tokens": with_row.get("cli_retrieval_output_estimated_tokens", 0),
                    "condition_total_including_retrieval": condition_total,
                    "solver_savings_tokens": solver_savings,
                    "net_savings_tokens": net_savings,
                    "net_savings_pct": net_savings_pct,
                    "without_elapsed_seconds": without_elapsed,
                    "condition_elapsed_seconds": elapsed,
                    "elapsed_savings_seconds": elapsed_delta,
                    "elapsed_savings_pct": elapsed_savings_pct,
                    "without_tests_passed": without_row["tests_passed"],
                    "condition_tests_passed": with_row["tests_passed"],
                    "retrieval_correct": with_row["retrieval_correct"],
                    "without_turns": without_row.get("agent_turns"),
                    "condition_turns": with_row.get("agent_turns"),
                    "without_shell_calls": without_row.get("shell_calls"),
                    "condition_shell_calls": with_row.get("shell_calls"),
                }
            )
    return out


def safe_sum(values: list[int | None]) -> int | None:
    present = [v for v in values if v is not None]
    return sum(present) if present else None


def safe_median(values: list[int | float | None]) -> int | float | None:
    present = [v for v in values if v is not None]
    return median(present) if present else None


def aggregates(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    out = []
    buckets: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for row in rows:
        buckets.setdefault((row["agent"], row["interface_condition"]), []).append(row)
    for (agent, interface), bucket in sorted(buckets.items()):
        provider = safe_sum([row.get("provider_total_tokens") for row in bucket])
        additive_payload = sum(
            row.get("memd_total_payload_tokens") or 0
            for row in bucket
            if interface in MCP_PAYLOAD_CONDITIONS
        )
        total = provider + additive_payload if provider is not None else None
        out.append(
            {
                "agent": agent,
                "interface_condition": interface,
                "cells": len(bucket),
                "tests_passed": sum(1 for row in bucket if row["tests_passed"]),
                "retrieval_correct": sum(1 for row in bucket if row["retrieval_correct"]),
                "provider_total_tokens": provider,
                "additive_mcp_payload_tokens": additive_payload,
                "observed_memory_search_payload_tokens": sum(row.get("memd_total_payload_tokens") or 0 for row in bucket),
                "cli_retrieval_output_estimated_tokens": sum(
                    row.get("cli_retrieval_output_estimated_tokens") or 0 for row in bucket
                ),
                "total_including_retrieval": total,
                "elapsed_seconds_total": safe_sum([row.get("elapsed_seconds") for row in bucket]),
                "elapsed_seconds_median": safe_median([row.get("elapsed_seconds") for row in bucket]),
                "claude_model_cache_creation_input_tokens": safe_sum([
                    row.get("claude_model_cache_creation_input_tokens") for row in bucket
                ]),
                "claude_model_cache_read_input_tokens": safe_sum([
                    row.get("claude_model_cache_read_input_tokens") for row in bucket
                ]),
                "visible_tool_count_median": safe_median([row.get("visible_tool_count") for row in bucket]),
                "visible_memd_tool_count_median": safe_median([row.get("visible_memd_tool_count") for row in bucket]),
            }
        )
    return out


def fmt(value: Any, suffix: str = "") -> str:
    if value is None:
        return ""
    if isinstance(value, float):
        return f"{value:.1f}{suffix}"
    return f"{value}{suffix}"


def fmt_pct(value: float | None) -> str:
    return "" if value is None else f"{value * 100:.1f}%"


def render_markdown(
    rows: list[dict[str, Any]],
    pairs: list[dict[str, Any]],
    aggregate_rows: list[dict[str, Any]],
    run_set: str,
) -> str:
    lines = [f"# memd multi-turn token-savings benchmark: {run_set}\n"]
    lines.append("## Aggregate By Interface\n")
    lines.append("| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |")
    lines.append("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for row in aggregate_rows:
        lines.append(
            f"| {row['agent']} | {row['interface_condition']} | {row['cells']} | "
            f"{row['tests_passed']} | {row['retrieval_correct']} | "
            f"{fmt(row['provider_total_tokens'])} | {fmt(row['additive_mcp_payload_tokens'])} | "
            f"{fmt(row['cli_retrieval_output_estimated_tokens'])} | "
            f"{fmt(row['total_including_retrieval'])} | {fmt(row['elapsed_seconds_total'])} | "
            f"{fmt(row['elapsed_seconds_median'])} | "
            f"{fmt(row['claude_model_cache_creation_input_tokens'])} | "
            f"{fmt(row['claude_model_cache_read_input_tokens'])} | "
            f"{fmt(row['visible_tool_count_median'])} | {fmt(row['visible_memd_tool_count_median'])} |"
        )

    lines.append("\n## Per-cell\n")
    lines.append("| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |")
    lines.append("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for row in rows:
        lines.append(
            f"| {row['agent']} | {row['episode_id']} | {row['interface_condition']} | "
            f"{int(row['tests_passed'])} | {fmt(row.get('provider_total_tokens'))} | "
            f"{fmt(row_total_including_retrieval(row))} | "
            f"{fmt(row.get('agent_turns'))} | {fmt(row.get('shell_calls'))} | "
            f"{fmt(row.get('memd_tool_calls'))} | {int(bool(row.get('retrieval_correct')))} | "
            f"{fmt(row.get('memd_total_payload_tokens'))} | "
            f"{fmt(row.get('cli_retrieval_output_estimated_tokens'))} | "
            f"{fmt(row.get('elapsed_seconds'))} | "
            f"{fmt(row.get('visible_tool_count'))} | {fmt(row.get('visible_memd_tool_count'))} |"
        )

    lines.append("\n## Paired Net Token Savings\n")
    lines.append("| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |")
    lines.append("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|")
    net_values = []
    for pair in pairs:
        net = pair["net_savings_tokens"]
        if net is not None:
            net_values.append(net)
        lines.append(
            f"| {pair['agent']} | {pair['episode_id']} | {pair['interface_condition']} | "
            f"{fmt(pair['without_provider_tokens'])} | {fmt(pair['condition_provider_tokens'])} | "
            f"{fmt(pair['additive_mcp_payload_tokens'])} | "
            f"{fmt(pair['cli_retrieval_output_estimated_tokens'])} | "
            f"{fmt(pair['condition_total_including_retrieval'])} | "
            f"{fmt(pair['solver_savings_tokens'])} | {fmt(net)} | "
            f"{fmt_pct(pair['net_savings_pct'])} | "
            f"{fmt(pair['without_elapsed_seconds'])} | {fmt(pair['condition_elapsed_seconds'])} | "
            f"{fmt(pair['elapsed_savings_seconds'])} | {fmt_pct(pair['elapsed_savings_pct'])} | "
            f"{int(pair['condition_tests_passed'])}/{int(pair['without_tests_passed'])} | "
            f"{int(pair['retrieval_correct'])} |"
        )

    if net_values:
        lines.append(f"\nMedian net savings: {median(net_values):+.0f} tokens across {len(net_values)} pairs.")
    lines.append(
        "\nToken caveat: Codex uses its CLI footer. Claude uses modelUsage totals. "
        "MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. "
        "CLI retrieval output is reported separately because command output is visible in the agent transcript."
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-set", default="pilot1")
    parser.add_argument("--agents", default="codex,claude")
    parser.add_argument("--conditions", default=DEFAULT_CONDITIONS)
    args = parser.parse_args()

    results_dir = BENCH / "results" / args.run_set
    episodes = load_episodes()
    agents = [a.strip() for a in args.agents.split(",") if a.strip()]
    conditions = [c.strip() for c in args.conditions.split(",") if c.strip()]
    rows = []
    for agent in agents:
        for episode_id, episode in episodes.items():
            for condition in conditions:
                row = analyze_cell(results_dir, agent, condition, episode_id, episode)
                if row is not None:
                    rows.append(row)
    pairs = paired(rows)
    aggregate_rows = aggregates(rows)
    report = {
        "run_set": args.run_set,
        "agents": agents,
        "conditions": conditions,
        "rows": rows,
        "pairs": pairs,
        "aggregates": aggregate_rows,
    }
    results_dir.mkdir(parents=True, exist_ok=True)
    (results_dir / "summary.json").write_text(json.dumps(report, indent=2))
    markdown = render_markdown(rows, pairs, aggregate_rows, args.run_set)
    (results_dir / "summary.md").write_text(markdown)
    print(markdown)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
