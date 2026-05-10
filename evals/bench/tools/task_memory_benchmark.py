#!/usr/bin/env python3
"""Phase 5 benchmark for memd task-oriented knowledge artifacts.

Benchmarks two memd-native retrieval modes against the same synthetic corpus:

1. memd_chunk_baseline:
   Flatten each case into plain memory chunks and query via `memory.search`.
2. memd_task_memory:
   Seed the real task lifecycle and query via `task.search`.

The report also optionally imports reference numbers from GenesisM's unified
benchmark JSON to show how this Phase 5 benchmark relates to prior external
benchmarking work.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
from urllib import error as urllib_error
from urllib import request as urllib_request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCRIPT_PATH = Path(__file__).resolve()
REPO_ROOT = SCRIPT_PATH.parents[3]
DEFAULT_CORPUS = REPO_ROOT / "docs/scientific-task-memory/benchmark-results/task_memory_benchmark_corpus.json"
DEFAULT_JSON_OUT = REPO_ROOT / "docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.json"
DEFAULT_MARKDOWN_OUT = REPO_ROOT / "docs/scientific-task-memory/benchmark-results/task_memory_benchmark_results.md"
DEFAULT_DATA_ROOT = REPO_ROOT / "docs/scientific-task-memory/benchmark-results/task_memory_benchmark_data"
ARK_CMD_TIMEOUT_S = 30.0
GENESIS_CMD_TIMEOUT_S = 180.0


@dataclass
class QueryResult:
    query_id: str
    query_type: str
    query_text: str
    expected_case_id: str
    expected_facet: str
    rank: int | None
    latency_ms: float


def discover_gpt54_root() -> Path | None:
    candidates = [
        REPO_ROOT.parent / "genesisM" / "gpt54",
        REPO_ROOT.parents[1] / "genesisM" / "gpt54",
        REPO_ROOT / ".." / ".." / "genesisM" / "gpt54",
    ]
    for candidate in candidates:
        candidate = candidate.resolve()
        if (candidate / "amem" / "cli.py").exists():
            return candidate
    return None


def discover_genesism_root() -> Path | None:
    candidates = [
        REPO_ROOT.parents[1] / "genesisM",
        REPO_ROOT.parent / "genesisM",
        REPO_ROOT / ".." / ".." / "genesisM",
    ]
    for candidate in candidates:
        candidate = candidate.resolve()
        if (candidate / "gpt54" / "amem" / "cli.py").exists():
            return candidate
    return None


def mean(values: list[float]) -> float:
    return statistics.mean(values) if values else 0.0


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round((len(ordered) - 1) * q))))
    return ordered[idx]


def benchmark_log(message: str) -> None:
    timestamp = time.strftime("%H:%M:%S")
    print(f"[phase5 {timestamp}] {message}", flush=True)


class McpStdioClient:
    def __init__(
        self,
        cmd: list[str],
        env: dict[str, str],
        startup_timeout_s: float = 30.0,
        stderr_file: Any | None = None,
    ) -> None:
        self.cmd = cmd
        self.env = env
        self.startup_timeout_s = startup_timeout_s
        self.stderr_file = stderr_file
        self.proc: subprocess.Popen[str] | None = None
        self.request_id = 0

    def start(self) -> None:
        self.proc = subprocess.Popen(
            self.cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr_file or subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=self.env,
        )
        self.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "task-memory-benchmark", "version": "0.1.0"},
            },
        )

    def stop(self) -> None:
        if self.proc is None or self.proc.poll() is not None:
            return
        if self.proc.stdin is not None:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
            return
        except subprocess.TimeoutExpired:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=5)

    def request(self, method: str, params: dict[str, Any] | None) -> dict[str, Any]:
        if self.proc is None or self.proc.stdin is None or self.proc.stdout is None:
            raise RuntimeError("MCP process is not started")
        self.request_id += 1
        payload = {"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        return self._read_response(self.request_id)

    def call_tool(self, tool_name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        response = self.request("tools/call", {"name": tool_name, "arguments": arguments})
        if "error" in response:
            raise RuntimeError(str(response["error"]))
        result = response.get("result", {})
        content = result.get("content", [])
        if not content:
            return {}
        text = content[0].get("text", "{}")
        if not isinstance(text, str):
            return {}
        return json.loads(text)

    def _read_response(self, request_id: int) -> dict[str, Any]:
        assert self.proc is not None and self.proc.stdout is not None
        start = time.perf_counter()
        while True:
            timeout_left = self.startup_timeout_s - (time.perf_counter() - start)
            if timeout_left <= 0:
                raise TimeoutError(f"timeout waiting for MCP response id={request_id}")
            ready, _, _ = select.select([self.proc.stdout], [], [], timeout_left)
            if not ready:
                continue
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP process closed stdout")
            try:
                payload = json.loads(line)
            except json.JSONDecodeError:
                continue
            if payload.get("id") == request_id:
                return payload


def read_chunks_indexed(client: McpStdioClient, tenant_id: str) -> int:
    payload = client.call_tool(
        "memory.metrics",
        {"tenant_id": tenant_id, "include_recent": False, "include_tiered": False},
    )
    index = payload.get("index", {})
    if not isinstance(index, dict):
        return 0
    stats = index.get(tenant_id, {})
    if not isinstance(stats, dict):
        return 0
    return int(stats.get("chunks_indexed", 0))


def wait_for_index_ready(
    client: McpStdioClient,
    tenant_id: str,
    target_chunks: int,
    timeout_s: float = 120.0,
    poll_s: float = 0.2,
) -> float:
    start = time.perf_counter()
    while True:
        current = read_chunks_indexed(client, tenant_id)
        if current >= target_chunks:
            return (time.perf_counter() - start) * 1000.0
        if (time.perf_counter() - start) > timeout_s:
            raise TimeoutError(
                f"index readiness timeout for tenant={tenant_id}: current={current}, target={target_chunks}"
            )
        time.sleep(poll_s)


def sanitize_tag_value(value: str) -> str:
    out: list[str] = []
    prev_underscore = False
    for ch in value:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_underscore = False
        elif not prev_underscore:
            out.append("_")
            prev_underscore = True
    sanitized = "".join(out).strip("_")
    return sanitized or "unknown"


def flatten_case_chunks(case: dict[str, Any]) -> list[dict[str, Any]]:
    case_id = case["case_id"]
    project_id = case["project_id"]
    dataset_refs = case.get("dataset_refs", [])
    entity_refs = case.get("entity_refs", [])
    run_start = case["run_start"]
    run_finish = case["run_finish"]
    finish = case["finish"]
    progress = case["progress"]
    evidence = case["evidence"]

    base_tags = [f"phase5:case:{case_id}"]
    for dataset in dataset_refs:
        name = dataset["name"]
        version = dataset.get("version", "")
        base_tags.append(
            f"phase5:dataset:{sanitize_tag_value(name)}::{sanitize_tag_value(version)}"
        )
    for entity in entity_refs:
        base_tags.append(
            f"phase5:entity:{sanitize_tag_value(entity['name'])}::{sanitize_tag_value(entity['entity_type'])}"
        )

    chunks = [
        {
            "text": "\n".join(
                [
                    f"Goal: {case['goal']}",
                    f"Motivation: {case['motivation']}",
                    f"Hypothesis: {case['hypothesis']}",
                    f"Scientific question: {case['scientific_question']}",
                    "Expected outputs: " + ", ".join(case.get("expected_outputs", [])),
                ]
            ),
            "type": "plan",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:task_goal"],
        },
        {
            "text": "\n".join(
                [
                    f"Summary: {progress['summary']}",
                    "Blockers: " + "; ".join(progress.get("blockers", [])),
                    "Failed attempts: " + "; ".join(progress.get("failed_attempts", [])),
                    f"Next step: {progress['next_step']}",
                ]
            ),
            "type": "summary",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:task_summary"],
        },
        {
            "text": "\n".join(
                [
                    f"Tool: {run_start['tool_name']}",
                    f"Tool version: {run_start.get('tool_version', '')}",
                    f"Command: {run_start['command']}",
                    f"Why chosen: {run_start['why_chosen']}",
                    f"Parameters: {json.dumps(run_start['parameters'], sort_keys=True)}",
                    "Inputs: " + ", ".join(run_start.get("inputs", [])),
                    "Outputs: " + ", ".join(run_finish.get("outputs", [])),
                    f"Metrics: {json.dumps(run_finish.get('metrics', {}), sort_keys=True)}",
                ]
            ),
            "type": "trace",
            "project_id": project_id,
            "tags": base_tags
            + [
                f"phase5:facet:run",
                f"phase5:tool:{sanitize_tag_value(run_start['tool_name'])}",
            ],
        },
        {
            "text": "\n".join(
                [
                    f"Evidence kind: {evidence['evidence_kind']}",
                    f"Supports claim: {evidence['supports_claim']}",
                    f"Summary: {evidence['summary']}",
                    f"Metric: {evidence.get('metric_name', '')}={evidence.get('metric_value', '')}",
                ]
            ),
            "type": "research",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:evidence"],
        },
        {
            "text": "What worked: " + "; ".join(finish.get("what_worked", [])),
            "type": "summary",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:worked"],
        },
        {
            "text": "\n".join(
                [
                    "What failed: " + "; ".join(finish.get("what_failed", [])),
                    "Uncertainty: " + "; ".join(finish.get("uncertainty", [])),
                ]
            ),
            "type": "research",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:failed"],
        },
        {
            "text": "\n".join(
                [
                    "Validation: " + "; ".join(finish.get("validation", [])),
                    "Followups: " + "; ".join(finish.get("followups", [])),
                    f"Confidence: {finish['confidence']}",
                ]
            ),
            "type": "summary",
            "project_id": project_id,
            "tags": base_tags + [f"phase5:facet:validation"],
        },
    ]
    return chunks


def seed_chunk_baseline_case(client: McpStdioClient, tenant_id: str, case: dict[str, Any]) -> int:
    payload = client.call_tool(
        "memory.add_batch",
        {"tenant_id": tenant_id, "chunks": flatten_case_chunks(case)},
    )
    chunk_ids = payload.get("chunk_ids", [])
    return len(chunk_ids) if isinstance(chunk_ids, list) else 0


def seed_task_case(client: McpStdioClient, tenant_id: str, case: dict[str, Any]) -> dict[str, Any]:
    project_id = case["project_id"]
    start_payload = client.call_tool(
        "task.start",
        {
            "tenant_id": tenant_id,
            "project_id": project_id,
            "goal": case["goal"],
            "motivation": case["motivation"],
            "hypothesis": case["hypothesis"],
            "scientific_question": case["scientific_question"],
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
            "expected_outputs": case.get("expected_outputs", []),
        },
    )
    task_id = start_payload["task_id"]
    projection_count = len(start_payload.get("projection_chunk_ids", []))

    progress = case["progress"]
    progress_payload = client.call_tool(
        "task.progress",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": project_id,
            "summary": progress["summary"],
            "blockers": progress.get("blockers", []),
            "failed_attempts": progress.get("failed_attempts", []),
            "next_step": progress["next_step"],
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
        },
    )
    projection_count += len(progress_payload.get("projection_chunk_ids", []))

    run_start = case["run_start"]
    run_start_payload = client.call_tool(
        "task.run_start",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": project_id,
            "tool_name": run_start["tool_name"],
            "tool_version": run_start.get("tool_version"),
            "command": run_start["command"],
            "why_chosen": run_start["why_chosen"],
            "parameters": run_start["parameters"],
            "inputs": run_start.get("inputs", []),
            "summary": run_start.get("summary"),
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
        },
    )
    projection_count += len(run_start_payload.get("projection_chunk_ids", []))

    run_finish = case["run_finish"]
    run_finish_payload = client.call_tool(
        "task.run_finish",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": project_id,
            "status": run_finish["status"],
            "tool_name": run_start["tool_name"],
            "tool_version": run_start.get("tool_version"),
            "command": run_start["command"],
            "outputs": run_finish.get("outputs", []),
            "metrics": run_finish.get("metrics"),
            "notes": run_finish["notes"],
            "validation": run_finish.get("validation", []),
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
        },
    )
    projection_count += len(run_finish_payload.get("projection_chunk_ids", []))

    evidence = case["evidence"]
    evidence_payload = client.call_tool(
        "task.add_evidence",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": project_id,
            "summary": evidence["summary"],
            "evidence_kind": evidence["evidence_kind"],
            "supports_claim": evidence["supports_claim"],
            "metric_name": evidence.get("metric_name"),
            "metric_value": evidence.get("metric_value"),
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
        },
    )
    projection_count += len(evidence_payload.get("projection_chunk_ids", []))

    finish = case["finish"]
    finish_payload = client.call_tool(
        "task.finish",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": project_id,
            "dataset_refs": case.get("dataset_refs", []),
            "entity_refs": case.get("entity_refs", []),
            "what_worked": finish.get("what_worked", []),
            "what_failed": finish.get("what_failed", []),
            "validation": finish.get("validation", []),
            "uncertainty": finish.get("uncertainty", []),
            "followups": finish.get("followups", []),
            "confidence": finish["confidence"],
        },
    )
    projection_count += len(finish_payload.get("projection_chunk_ids", []))

    return {"task_id": task_id, "projection_count": projection_count}


def match_baseline_result(result: dict[str, Any], case_id: str, facet: str) -> bool:
    tags = result.get("tags", [])
    if not isinstance(tags, list):
        return False
    return (
        f"phase5:case:{case_id}" in tags and f"phase5:facet:{facet}" in tags
    )


def match_baseline_case(result: dict[str, Any], case_id: str) -> bool:
    tags = result.get("tags", [])
    if not isinstance(tags, list):
        return False
    return f"phase5:case:{case_id}" in tags


def match_task_result(result: dict[str, Any], facet: str) -> bool:
    tags = result.get("tags", [])
    if not isinstance(tags, list):
        return False
    return f"task:projection:{facet}" in tags


def match_task_case(result: dict[str, Any], task_id: str) -> bool:
    tags = result.get("tags", [])
    if not isinstance(tags, list):
        return False
    return f"task:id:{sanitize_tag_value(task_id)}" in tags


def find_rank(
    results: list[dict[str, Any]],
    matcher,
) -> int | None:
    for idx, result in enumerate(results, start=1):
        if matcher(result):
            return idx
    return None


def summarize_query_results(rows: list[QueryResult]) -> dict[str, Any]:
    hit1 = mean([1.0 if row.rank == 1 else 0.0 for row in rows])
    hit3 = mean([1.0 if row.rank and row.rank <= 3 else 0.0 for row in rows])
    mrr = mean([1.0 / row.rank if row.rank else 0.0 for row in rows])
    latencies = [row.latency_ms for row in rows]
    by_type: dict[str, list[QueryResult]] = {}
    for row in rows:
        by_type.setdefault(row.query_type, []).append(row)
    return {
        "queries": len(rows),
        "hit1": hit1,
        "hit3": hit3,
        "mrr": mrr,
        "avg_search_ms": mean(latencies),
        "p50_search_ms": percentile(latencies, 0.5),
        "p95_search_ms": percentile(latencies, 0.95),
        "details": [row.__dict__ for row in rows],
        "per_type": {
            name: {
                "hit1": mean([1.0 if row.rank == 1 else 0.0 for row in items]),
                "hit3": mean([1.0 if row.rank and row.rank <= 3 else 0.0 for row in items]),
                "mrr": mean([1.0 / row.rank if row.rank else 0.0 for row in items]),
            }
            for name, items in by_type.items()
        },
    }


def run_baseline_queries(
    client: McpStdioClient,
    corpus: dict[str, Any],
    tenant_id: str,
    top_k: int,
    score_mode: str = "task_level",
) -> dict[str, Any]:
    rows: list[QueryResult] = []
    cases = {case["case_id"]: case for case in corpus["cases"]}
    for query in corpus["queries"]:
        case = cases[query["case_id"]]
        started = time.perf_counter()
        payload = client.call_tool(
            "memory.search",
            {
                "tenant_id": tenant_id,
                "query": query["query"],
                "k": top_k,
            },
        )
        latency_ms = (time.perf_counter() - started) * 1000.0
        results = payload.get("results", [])
        if score_mode == "facet_level":
            matcher = lambda result, case_id=query["case_id"], facet=query["target_facet"]: match_baseline_result(
                result, case_id, facet
            )
        else:
            matcher = lambda result, case_id=query["case_id"]: match_baseline_case(result, case_id)
        rank = find_rank(results if isinstance(results, list) else [], matcher)
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def run_task_queries(
    client: McpStdioClient,
    corpus: dict[str, Any],
    task_ids: dict[str, str],
    tenant_id: str,
    top_k: int,
    score_mode: str = "task_level",
) -> dict[str, Any]:
    rows: list[QueryResult] = []
    cases = {case["case_id"]: case for case in corpus["cases"]}
    for query in corpus["queries"]:
        case = cases[query["case_id"]]
        task_filters = dict(query.get("task_filters", {}))
        started = time.perf_counter()
        payload = client.call_tool(
            "task.search",
            {
                "tenant_id": tenant_id,
                "query": query["query"],
                "k": top_k,
                "filters": task_filters,
            },
        )
        latency_ms = (time.perf_counter() - started) * 1000.0
        results = payload.get("results", [])
        if score_mode == "facet_level":
            matcher = lambda result, facet=query["target_facet"], task_id=task_ids[query["case_id"]]: (
                match_task_case(result, task_id) and match_task_result(result, facet)
            )
        else:
            matcher = lambda result, task_id=task_ids[query["case_id"]]: match_task_case(result, task_id)
        rank = find_rank(results if isinstance(results, list) else [], matcher)
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def run_baseline_freshness(client: McpStdioClient, tenant_id: str) -> dict[str, Any]:
    project_id = "phase5_freshness_baseline"
    client.call_tool(
        "memory.add",
        {
            "tenant_id": tenant_id,
            "project_id": project_id,
            "text": "immediately searchable failure memory for freshness probe",
            "type": "research",
            "tags": ["phase5:case:freshness_probe", "phase5:facet:failed"],
        },
    )
    started = time.perf_counter()
    payload = client.call_tool(
        "memory.search",
        {
            "tenant_id": tenant_id,
            "project_id": project_id,
            "query": "immediately searchable failure memory",
            "k": 3,
        },
    )
    latency_ms = (time.perf_counter() - started) * 1000.0
    results = payload.get("results", [])
    rank = find_rank(
        results if isinstance(results, list) else [],
        lambda result: match_baseline_result(result, "freshness_probe", "failed"),
    )
    return {"found": rank is not None, "rank": rank, "search_ms": latency_ms}


def run_task_freshness(client: McpStdioClient, tenant_id: str) -> dict[str, Any]:
    start = client.call_tool(
        "task.start",
        {
            "tenant_id": tenant_id,
            "project_id": "phase5_freshness_task",
            "goal": "Freshness probe for task search",
            "motivation": "Need to verify immediate retrievability after completion",
            "hypothesis": "A newly finished task should be searchable immediately",
            "scientific_question": "How quickly does a new task become retrievable?",
            "dataset_refs": [{"name": "freshness_probe", "version": "v1"}],
            "expected_outputs": ["freshness probe finish artifact"],
        },
    )
    task_id = start["task_id"]
    client.call_tool(
        "task.finish",
        {
            "tenant_id": tenant_id,
            "task_id": task_id,
            "project_id": "phase5_freshness_task",
            "what_worked": ["Fresh artifact should appear in the first search immediately"],
            "what_failed": ["immediately searchable failure memory"],
            "validation": ["Freshness benchmark setup complete"],
            "uncertainty": [],
            "followups": [],
            "confidence": 1.0,
        },
    )
    started = time.perf_counter()
    payload = client.call_tool(
        "task.search",
        {
            "tenant_id": tenant_id,
            "query": "immediately searchable failure memory",
            "k": 3,
            "filters": {
                "task_id": task_id,
                "project_id": "phase5_freshness_task",
                "artifact_kind": "task_finish",
            },
        },
    )
    latency_ms = (time.perf_counter() - started) * 1000.0
    results = payload.get("results", [])
    rank = find_rank(
        results if isinstance(results, list) else [],
        lambda result: match_task_result(result, "failed"),
    )
    return {"found": rank is not None, "rank": rank, "search_ms": latency_ms}


def run_concurrency(
    memd_cmd: list[str] | None,
    env: dict[str, str] | None,
    writer,
    workers: int,
    ops_per_worker: int,
) -> dict[str, Any]:
    successes = 0
    errors: list[str] = []
    lock = threading.Lock()

    def worker_fn(worker_idx: int) -> None:
        nonlocal successes
        client: McpStdioClient | None = None
        try:
            if memd_cmd:
                client = McpStdioClient(memd_cmd, env or {}, startup_timeout_s=60.0)
                client.start()
            for op_idx in range(ops_per_worker):
                try:
                    writer(client, worker_idx, op_idx)
                    with lock:
                        successes += 1
                except Exception as exc:  # noqa: BLE001
                    with lock:
                        errors.append(str(exc))
        except Exception as exc:  # noqa: BLE001
            with lock:
                errors.append(str(exc))
        finally:
            if client is not None:
                client.stop()

    threads = [threading.Thread(target=worker_fn, args=(i,)) for i in range(workers)]
    started = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    total_ops = workers * ops_per_worker
    success_rate = successes / total_ops if total_ops else 0.0
    ops_per_sec = successes / (elapsed_ms / 1000.0) if elapsed_ms > 0 else 0.0
    return {
        "total_ops": total_ops,
        "successes": successes,
        "errors": errors,
        "success_rate": success_rate,
        "ops_per_sec": ops_per_sec,
        "total_ms": elapsed_ms,
    }


def load_genesism_reference(path: Path | None) -> dict[str, Any] | None:
    if path is None or not path.exists():
        return None
    payload = json.loads(path.read_text(encoding="utf-8"))
    systems = payload.get("systems", {})
    out = {}
    for name in ("gpt54", "memd"):
        if name not in systems:
            continue
        system = systems[name]
        out[name] = {
            "search_backend": system["search_backend"],
            "lifecycle_ms": system["lifecycle_cli"]["total_ms"],
            "hit3": system["search_quality"]["hit3"],
            "mrr": system["search_quality"]["mrr"],
            "avg_search_ms": system["search_quality"]["avg_search_ms"],
            "fresh_rank": system["freshness"]["rank"],
            "concurrency_success_rate": system["concurrency"]["success_rate"],
            "concurrency_ops_per_sec": system["concurrency"]["ops_per_sec"],
        }
    return out or None


def run_gpt54_cli(gpt54_root: Path, db_path: Path, *args: str) -> dict[str, Any]:
    env = os.environ.copy()
    cmd = [sys.executable, "-m", "amem", "--db", str(db_path)] + list(args)
    proc = subprocess.run(
        cmd,
        cwd=str(gpt54_root),
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or f"gpt54 command failed: {' '.join(cmd)}")
    stdout = proc.stdout.strip()
    return json.loads(stdout) if stdout else {}


def run_gpt54_daemon_command(
    gpt54_root: Path,
    db_path: Path,
    daemon_url: str,
    *args: str,
) -> dict[str, Any]:
    env = os.environ.copy()
    cmd = [sys.executable, "-m", "amem", "--db", str(db_path), "--daemon-url", daemon_url] + list(args)
    proc = subprocess.run(
        cmd,
        cwd=str(gpt54_root),
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or f"gpt54 daemon command failed: {' '.join(cmd)}")
    stdout = proc.stdout.strip()
    return json.loads(stdout) if stdout else {}


def post_json(url: str, payload: dict[str, Any]) -> dict[str, Any] | list[Any]:
    req = urllib_request.Request(
        url,
        data=json.dumps(payload, sort_keys=True).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib_request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib_error.HTTPError as exc:
        body = exc.read().decode("utf-8")
        raise RuntimeError(body) from exc


def run_text_cli(
    cmd: list[str],
    cwd: Path,
    env: dict[str, str] | None = None,
    timeout_s: float | None = None,
) -> str:
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(cwd),
            env=env or os.environ.copy(),
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout_s,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"command timed out after {timeout_s:.1f}s: {' '.join(cmd)}"
        ) from exc
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or f"command failed: {' '.join(cmd)}")
    return proc.stdout


ARK_ID_RE = re.compile(r"\[([0-9a-f]{8})\]")
ARK_START_RE = re.compile(r"Artifact\s+([0-9a-f]{8})\s+registered")
GEMINIPRO_START_RE = re.compile(r"Genesis Task ID:\s*([A-Za-z0-9_-]+)")
GEMINIPRO_SEARCH_RE = re.compile(r"Task:\s*([A-Za-z0-9_-]+)\s+\|")
GEMINIULTRA_START_RE = re.compile(r"Genesis ID:\s*(ART-[A-Za-z0-9]+)")
GEMINIULTRA_SEARCH_RE = re.compile(r"ID:\s*(ART-[A-Za-z0-9]+)\s+\|")


def reserve_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_port(port: int, timeout_s: float = 15.0) -> None:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"Timed out waiting for port {port}")


def gpt54_facet_kinds(facet: str) -> list[str]:
    mapping = {
        "task_goal": ["goal", "motivation", "hypothesis", "question"],
        "task_summary": ["summary"],
        "run": ["run", "tool"],
        "evidence": ["evidence"],
        "worked": ["worked"],
        "failed": ["failed"],
        "validation": ["validation"],
    }
    return mapping.get(facet, [facet])


def build_gpt54_context_request(case: dict[str, Any], query: dict[str, Any], top_k: int) -> dict[str, Any]:
    task_filters = query.get("task_filters", {})
    return {
        "project_slug": case["project_id"],
        "query": query["query"],
        "limit": top_k,
        "status": task_filters.get("status"),
        "kinds": gpt54_facet_kinds(query["target_facet"]),
        "dataset_names": [task_filters["dataset_name"]] if task_filters.get("dataset_name") else None,
        "entity_names": [task_filters["entity_name"]] if task_filters.get("entity_name") else None,
        "tool_names": [task_filters["tool_name"]] if task_filters.get("tool_name") else None,
    }


def build_gpt54_context_cli_args(request: dict[str, Any]) -> list[str]:
    args = [
        "context",
        "--project",
        request["project_slug"],
        "--query",
        request["query"],
        "--limit",
        str(request["limit"]),
        "--kinds-json",
        json.dumps(request["kinds"]),
    ]
    if request.get("status"):
        args.extend(["--status", request["status"]])
    if request.get("dataset_names"):
        args.extend(["--datasets-json", json.dumps(request["dataset_names"])])
    if request.get("entity_names"):
        args.extend(["--entities-json", json.dumps(request["entity_names"])])
    if request.get("tool_names"):
        args.extend(["--tools-json", json.dumps(request["tool_names"])])
    return args


def gpt54_filtered_task_ids(
    db_path: Path,
    *,
    project_slug: str | None,
    status: str | None,
    dataset_names: list[str] | None,
    entity_names: list[str] | None,
    tool_names: list[str] | None,
) -> set[str]:
    import duckdb

    con = duckdb.connect(str(db_path))
    try:
        clauses = []
        params: list[Any] = []
        if project_slug is not None:
            clauses.append("project_slug = ?")
            params.append(project_slug)
        if status is not None:
            clauses.append("status = ?")
            params.append(status)
        task_query = "SELECT task_id FROM tasks"
        if clauses:
            task_query += " WHERE " + " AND ".join(clauses)
        base_ids = {row[0] for row in con.execute(task_query, params).fetchall()}

        dataset_filter = {name.strip().lower() for name in (dataset_names or []) if name.strip()}
        if dataset_filter:
            placeholders = ", ".join("?" for _ in dataset_filter)
            rows = con.execute(
                f"""
                SELECT DISTINCT td.task_id
                FROM task_datasets td
                JOIN datasets d ON d.dataset_id = td.dataset_id
                WHERE lower(d.name) IN ({placeholders})
                """,
                list(dataset_filter),
            ).fetchall()
            base_ids &= {row[0] for row in rows}

        entity_filter = {name.strip().lower() for name in (entity_names or []) if name.strip()}
        if entity_filter:
            placeholders = ", ".join("?" for _ in entity_filter)
            rows = con.execute(
                f"""
                SELECT DISTINCT te.task_id
                FROM task_entities te
                JOIN entities e ON e.entity_id = te.entity_id
                WHERE lower(e.canonical_name) IN ({placeholders})
                """,
                list(entity_filter),
            ).fetchall()
            base_ids &= {row[0] for row in rows}

        tool_filter = {name.strip().lower() for name in (tool_names or []) if name.strip()}
        if tool_filter:
            placeholders = ", ".join("?" for _ in tool_filter)
            run_rows = con.execute(
                f"SELECT DISTINCT task_id FROM runs WHERE lower(tool_name) IN ({placeholders})",
                list(tool_filter),
            ).fetchall()
            insight_rows = con.execute(
                f"SELECT DISTINCT task_id FROM insights WHERE lower(tool_name) IN ({placeholders})",
                list(tool_filter),
            ).fetchall()
            base_ids &= {row[0] for row in run_rows} | {row[0] for row in insight_rows}

        return base_ids
    finally:
        con.close()


def query_gpt54_tantivy(db_path: Path, tantivy_url: str, request: dict[str, Any]) -> dict[str, Any]:
    task_ids = gpt54_filtered_task_ids(
        db_path,
        project_slug=request.get("project_slug"),
        status=request.get("status"),
        dataset_names=request.get("dataset_names"),
        entity_names=request.get("entity_names"),
        tool_names=request.get("tool_names"),
    )
    if not task_ids:
        return {"hits": []}
    payload = post_json(
        tantivy_url,
        {
            "query": request["query"],
            "project_slug": request.get("project_slug"),
            "limit": request["limit"],
            "task_ids": sorted(task_ids),
            "kinds": request["kinds"],
        },
    )
    return {"hits": payload if isinstance(payload, list) else []}


def warm_gpt54_tantivy(corpus: dict[str, Any], db_path: Path, tantivy_url: str) -> None:
    first_query = corpus["queries"][0]
    first_case = next(case for case in corpus["cases"] if case["case_id"] == first_query["case_id"])
    request = build_gpt54_context_request(first_case, first_query, 1)
    query_gpt54_tantivy(db_path, tantivy_url, request)


def seed_gpt54_case(gpt54_root: Path, db_path: Path, case: dict[str, Any]) -> dict[str, Any]:
    project_id = case["project_id"]
    start = run_gpt54_cli(
        gpt54_root,
        db_path,
        "start",
        "--project",
        project_id,
        "--title",
        case["goal"],
        "--goal",
        case["goal"],
        "--agent",
        "phase5-benchmark",
        "--motivation",
        case["motivation"],
        "--hypothesis",
        case["hypothesis"],
        "--question",
        case["scientific_question"],
        "--datasets-json",
        json.dumps(case.get("dataset_refs", [])),
        "--expected-outputs-json",
        json.dumps(case.get("expected_outputs", [])),
        "--parameters-json",
        json.dumps(case["run_start"]["parameters"]),
    )
    task_id = start["task_id"]

    progress = case["progress"]
    state = "blocked" if progress.get("blockers") else "in_progress"
    run_gpt54_cli(
        gpt54_root,
        db_path,
        "progress",
        "--task-id",
        task_id,
        "--summary",
        progress["summary"],
        "--state",
        state,
        "--failed-json",
        json.dumps([{"summary": item} for item in progress.get("failed_attempts", [])]),
        "--blockers-json",
        json.dumps([{"summary": item} for item in progress.get("blockers", [])]),
        "--next-step",
        progress["next_step"],
    )

    run_start = case["run_start"]
    started_run = run_gpt54_cli(
        gpt54_root,
        db_path,
        "run-start",
        "--task-id",
        task_id,
        "--tool-name",
        run_start["tool_name"],
        "--tool-version",
        str(run_start.get("tool_version") or ""),
        "--exec-command",
        run_start["command"],
        "--why",
        run_start["why_chosen"],
        "--summary",
        run_start.get("summary", ""),
        "--parameters-json",
        json.dumps(run_start["parameters"]),
        "--inputs-json",
        json.dumps(run_start.get("inputs", [])),
    )

    run_finish = case["run_finish"]
    run_gpt54_cli(
        gpt54_root,
        db_path,
        "run-finish",
        "--run-id",
        started_run["run_id"],
        "--status",
        run_finish["status"],
        "--outputs-json",
        json.dumps(run_finish.get("outputs", [])),
        "--metrics-json",
        json.dumps(run_finish.get("metrics", {})),
        "--notes",
        run_finish["notes"],
    )

    evidence = case["evidence"]
    evidence_args = [
        "add-evidence",
        "--task-id",
        task_id,
        "--summary",
        evidence["summary"],
        "--kind",
        evidence["evidence_kind"],
    ]
    if evidence.get("metric_name"):
        evidence_args.extend(["--metric-name", evidence["metric_name"]])
    if evidence.get("metric_value") is not None:
        evidence_args.extend(["--metric-value", str(evidence["metric_value"])])
    if evidence.get("supports_claim"):
        evidence_args.append("--supports-claim")
    run_gpt54_cli(gpt54_root, db_path, *evidence_args)

    finish = case["finish"]
    run_gpt54_cli(
        gpt54_root,
        db_path,
        "finish",
        "--task-id",
        task_id,
        "--summary",
        f"Completed {case['case_id']}",
        "--outcome",
        "success",
        "--worked-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("what_worked", [])]),
        "--failed-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("what_failed", [])]),
        "--validation-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("validation", [])]),
        "--tools-json",
        json.dumps(
            [
                {
                    "summary": run_start["tool_name"],
                    "why": run_start["why_chosen"],
                    "tool_name": run_start["tool_name"],
                    "tool_settings_json": json.dumps(run_start["parameters"], sort_keys=True),
                }
            ]
        ),
        "--followups-json",
        json.dumps(finish.get("followups", [])),
        "--confidence",
        str(finish["confidence"]),
    )
    return {"task_id": task_id}


def run_gpt54_queries(
    gpt54_root: Path,
    db_path: Path,
    corpus: dict[str, Any],
    task_ids: dict[str, str],
    top_k: int,
) -> dict[str, Any]:
    rows: list[QueryResult] = []
    for query in corpus["queries"]:
        case = next(case for case in corpus["cases"] if case["case_id"] == query["case_id"])
        request = build_gpt54_context_request(case, query, top_k)

        started = time.perf_counter()
        payload = run_gpt54_cli(gpt54_root, db_path, *build_gpt54_context_cli_args(request))
        latency_ms = (time.perf_counter() - started) * 1000.0
        hits = payload.get("hits", [])
        rank = find_rank(
            hits if isinstance(hits, list) else [],
            lambda result, task_id=task_ids[query["case_id"]], kinds=set(request["kinds"]): result.get("task_id") == task_id
            and result.get("kind") in kinds,
        )
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def run_gpt54_freshness(gpt54_root: Path, db_path: Path) -> dict[str, Any]:
    start = run_gpt54_cli(
        gpt54_root,
        db_path,
        "start",
        "--project",
        "phase5_freshness_task",
        "--title",
        "Freshness probe for task search",
        "--goal",
        "Freshness probe for task search",
        "--agent",
        "phase5-benchmark",
        "--motivation",
        "Need to verify immediate retrievability after completion",
        "--hypothesis",
        "A newly finished task should be searchable immediately",
        "--question",
        "How quickly does a new task become retrievable?",
        "--datasets-json",
        json.dumps([{"name": "freshness_probe", "version": "v1"}]),
    )
    task_id = start["task_id"]
    run_gpt54_cli(
        gpt54_root,
        db_path,
        "finish",
        "--task-id",
        task_id,
        "--summary",
        "Freshness benchmark complete",
        "--outcome",
        "success",
        "--worked-json",
        json.dumps([{"summary": "Fresh artifact should appear in the first search immediately", "why": "freshness probe"}]),
        "--failed-json",
        json.dumps([{"summary": "immediately searchable failure memory", "why": "freshness probe"}]),
        "--validation-json",
        json.dumps([{"summary": "Freshness benchmark setup complete", "why": "freshness probe"}]),
    )
    started = time.perf_counter()
    payload = run_gpt54_cli(
        gpt54_root,
        db_path,
        "context",
        "--project",
        "phase5_freshness_task",
        "--query",
        "immediately searchable failure memory",
        "--limit",
        "3",
        "--kinds-json",
        json.dumps(["failed"]),
    )
    latency_ms = (time.perf_counter() - started) * 1000.0
    hits = payload.get("hits", [])
    rank = find_rank(
        hits if isinstance(hits, list) else [],
        lambda result: result.get("task_id") == task_id and result.get("kind") == "failed",
    )
    return {"found": rank is not None, "rank": rank, "search_ms": latency_ms}


def run_gpt54_concurrency(gpt54_root: Path, db_path: Path, workers: int, ops_per_worker: int) -> dict[str, Any]:
    seeded = run_gpt54_cli(
        gpt54_root,
        db_path,
        "start",
        "--project",
        "phase5_concurrency_task",
        "--title",
        "Concurrency task seed",
        "--goal",
        "Concurrency task seed",
        "--agent",
        "phase5-benchmark",
        "--motivation",
        "Need to measure concurrent task lifecycle writes",
        "--hypothesis",
        "Concurrent task progress writes should succeed",
        "--question",
        "Does concurrent progress logging remain reliable?",
    )
    task_id = seeded["task_id"]

    def writer(_client: McpStdioClient, worker_idx: int, op_idx: int) -> None:
        run_gpt54_cli(
            gpt54_root,
            db_path,
            "progress",
            "--task-id",
            task_id,
            "--summary",
            f"benchmark progress note worker={worker_idx} op={op_idx}",
            "--state",
            "in_progress",
            "--next-step",
            "continue benchmark",
        )

    # Reuse the same thread/process orchestration logic even though each op is a CLI subprocess.
    return run_concurrency(
        memd_cmd=[],
        env={},
        writer=writer,
        workers=workers,
        ops_per_worker=ops_per_worker,
    )


def seed_gpt54_case_with(call_fn, case: dict[str, Any]) -> dict[str, Any]:
    project_id = case["project_id"]
    start = call_fn(
        "start",
        "--project",
        project_id,
        "--title",
        case["goal"],
        "--goal",
        case["goal"],
        "--agent",
        "phase5-benchmark",
        "--motivation",
        case["motivation"],
        "--hypothesis",
        case["hypothesis"],
        "--question",
        case["scientific_question"],
        "--datasets-json",
        json.dumps(case.get("dataset_refs", [])),
        "--expected-outputs-json",
        json.dumps(case.get("expected_outputs", [])),
        "--parameters-json",
        json.dumps(case["run_start"]["parameters"]),
    )
    task_id = start["task_id"]

    progress = case["progress"]
    state = "blocked" if progress.get("blockers") else "in_progress"
    call_fn(
        "progress",
        "--task-id",
        task_id,
        "--summary",
        progress["summary"],
        "--state",
        state,
        "--failed-json",
        json.dumps([{"summary": item} for item in progress.get("failed_attempts", [])]),
        "--blockers-json",
        json.dumps([{"summary": item} for item in progress.get("blockers", [])]),
        "--next-step",
        progress["next_step"],
    )

    run_start = case["run_start"]
    started_run = call_fn(
        "run-start",
        "--task-id",
        task_id,
        "--tool-name",
        run_start["tool_name"],
        "--tool-version",
        str(run_start.get("tool_version") or ""),
        "--exec-command",
        run_start["command"],
        "--why",
        run_start["why_chosen"],
        "--summary",
        run_start.get("summary", ""),
        "--parameters-json",
        json.dumps(run_start["parameters"]),
        "--inputs-json",
        json.dumps(run_start.get("inputs", [])),
    )

    run_finish = case["run_finish"]
    call_fn(
        "run-finish",
        "--run-id",
        started_run["run_id"],
        "--status",
        run_finish["status"],
        "--outputs-json",
        json.dumps(run_finish.get("outputs", [])),
        "--metrics-json",
        json.dumps(run_finish.get("metrics", {})),
        "--notes",
        run_finish["notes"],
    )

    evidence = case["evidence"]
    evidence_args = [
        "add-evidence",
        "--task-id",
        task_id,
        "--summary",
        evidence["summary"],
        "--kind",
        evidence["evidence_kind"],
    ]
    if evidence.get("metric_name"):
        evidence_args.extend(["--metric-name", evidence["metric_name"]])
    if evidence.get("metric_value") is not None:
        evidence_args.extend(["--metric-value", str(evidence["metric_value"])])
    if evidence.get("supports_claim"):
        evidence_args.append("--supports-claim")
    call_fn(*evidence_args)

    finish = case["finish"]
    call_fn(
        "finish",
        "--task-id",
        task_id,
        "--summary",
        f"Completed {case['case_id']}",
        "--outcome",
        "success",
        "--worked-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("what_worked", [])]),
        "--failed-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("what_failed", [])]),
        "--validation-json",
        json.dumps([{"summary": item, "why": "phase5 benchmark"} for item in finish.get("validation", [])]),
        "--tools-json",
        json.dumps(
            [
                {
                    "summary": run_start["tool_name"],
                    "why": run_start["why_chosen"],
                    "tool_name": run_start["tool_name"],
                    "tool_settings_json": json.dumps(run_start["parameters"], sort_keys=True),
                }
            ]
        ),
        "--followups-json",
        json.dumps(finish.get("followups", [])),
        "--confidence",
        str(finish["confidence"]),
    )
    return {"task_id": task_id}


def run_gpt54_queries_with(call_fn, corpus: dict[str, Any], task_ids: dict[str, str], top_k: int) -> dict[str, Any]:
    rows: list[QueryResult] = []
    for query in corpus["queries"]:
        case = next(case for case in corpus["cases"] if case["case_id"] == query["case_id"])
        request = build_gpt54_context_request(case, query, top_k)

        started = time.perf_counter()
        payload = call_fn(*build_gpt54_context_cli_args(request))
        latency_ms = (time.perf_counter() - started) * 1000.0
        hits = payload.get("hits", [])
        rank = find_rank(
            hits if isinstance(hits, list) else [],
            lambda result, task_id=task_ids[query["case_id"]], kinds=set(request["kinds"]): result.get("task_id") == task_id
            and result.get("kind") in kinds,
        )
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def run_gpt54_tantivy_queries(
    db_path: Path,
    tantivy_url: str,
    corpus: dict[str, Any],
    task_ids: dict[str, str],
    top_k: int,
) -> dict[str, Any]:
    rows: list[QueryResult] = []
    for query in corpus["queries"]:
        case = next(case for case in corpus["cases"] if case["case_id"] == query["case_id"])
        request = build_gpt54_context_request(case, query, top_k)
        started = time.perf_counter()
        payload = query_gpt54_tantivy(db_path, tantivy_url, request)
        latency_ms = (time.perf_counter() - started) * 1000.0
        hits = payload.get("hits", [])
        rank = find_rank(
            hits if isinstance(hits, list) else [],
            lambda result, task_id=task_ids[query["case_id"]], kinds=set(request["kinds"]): result.get("task_id") == task_id
            and result.get("kind") in kinds,
        )
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def run_gpt54_freshness_with(call_fn) -> dict[str, Any]:
    start = call_fn(
        "start",
        "--project",
        "phase5_freshness_task",
        "--title",
        "Freshness probe for task search",
        "--goal",
        "Freshness probe for task search",
        "--agent",
        "phase5-benchmark",
        "--motivation",
        "Need to verify immediate retrievability after completion",
        "--hypothesis",
        "A newly finished task should be searchable immediately",
        "--question",
        "How quickly does a new task become retrievable?",
        "--datasets-json",
        json.dumps([{"name": "freshness_probe", "version": "v1"}]),
    )
    task_id = start["task_id"]
    call_fn(
        "finish",
        "--task-id",
        task_id,
        "--summary",
        "Freshness benchmark complete",
        "--outcome",
        "success",
        "--worked-json",
        json.dumps([{"summary": "Fresh artifact should appear in the first search immediately", "why": "freshness probe"}]),
        "--failed-json",
        json.dumps([{"summary": "immediately searchable failure memory", "why": "freshness probe"}]),
        "--validation-json",
        json.dumps([{"summary": "Freshness benchmark setup complete", "why": "freshness probe"}]),
    )
    started = time.perf_counter()
    payload = call_fn(
        "context",
        "--project",
        "phase5_freshness_task",
        "--query",
        "immediately searchable failure memory",
        "--limit",
        "3",
        "--kinds-json",
        json.dumps(["failed"]),
    )
    latency_ms = (time.perf_counter() - started) * 1000.0
    hits = payload.get("hits", [])
    rank = find_rank(
        hits if isinstance(hits, list) else [],
        lambda result: result.get("task_id") == task_id and result.get("kind") == "failed",
    )
    return {"found": rank is not None, "rank": rank, "search_ms": latency_ms}


def run_gpt54_concurrency_with(call_fn, workers: int, ops_per_worker: int) -> dict[str, Any]:
    seeded = call_fn(
        "start",
        "--project",
        "phase5_concurrency_task",
        "--title",
        "Concurrency task seed",
        "--goal",
        "Concurrency task seed",
        "--agent",
        "phase5-benchmark",
        "--motivation",
        "Need to measure concurrent task lifecycle writes",
        "--hypothesis",
        "Concurrent task progress writes should succeed",
        "--question",
        "Does concurrent progress logging remain reliable?",
    )
    task_id = seeded["task_id"]

    def writer(_client: McpStdioClient | None, worker_idx: int, op_idx: int) -> None:
        call_fn(
            "progress",
            "--task-id",
            task_id,
            "--summary",
            f"benchmark progress note worker={worker_idx} op={op_idx}",
            "--state",
            "in_progress",
            "--next-step",
            "continue benchmark",
        )

    return run_concurrency(
        memd_cmd=None,
        env=None,
        writer=writer,
        workers=workers,
        ops_per_worker=ops_per_worker,
    )


def run_ark_command(ark_root: Path, home_dir: Path, *args: str) -> str:
    env = os.environ.copy()
    env["HOME"] = str(home_dir)
    return run_text_cli(
        [sys.executable, str(ark_root / "ark.py"), *args],
        cwd=ark_root,
        env=env,
        timeout_s=ARK_CMD_TIMEOUT_S,
    )


def seed_ark_case(ark_root: Path, home_dir: Path, case: dict[str, Any]) -> dict[str, Any]:
    output = run_ark_command(
        ark_root,
        home_dir,
        "start",
        case["goal"],
        "--agent",
        "phase5-benchmark",
        "--project",
        case["project_id"],
        "--motivation",
        case["motivation"],
        "--approach",
        case["hypothesis"],
        "--tools",
        case["run_start"]["tool_name"],
        "--provenance",
        case["run_start"]["why_chosen"],
        "--tags",
        case["case_id"],
    )
    match = ARK_START_RE.search(output)
    if not match:
        raise RuntimeError(f"failed to parse ark artifact id from: {output}")
    artifact_id = match.group(1)
    run_ark_command(
        ark_root,
        home_dir,
        "update",
        artifact_id,
        "--progress",
        case["progress"]["summary"],
    )
    finish = case["finish"]
    run_ark_command(
        ark_root,
        home_dir,
        "finish",
        artifact_id,
        "--outcome",
        f"Completed {case['case_id']}",
        "--worked",
        "; ".join(finish.get("what_worked", [])),
        "--failed",
        "; ".join(finish.get("what_failed", [])),
        "--dead-ends",
        "; ".join(case["progress"].get("failed_attempts", [])),
        "--tools",
        case["run_start"]["tool_name"],
        "--provenance",
        case["run_start"]["why_chosen"],
        "--confidence",
        str(finish["confidence"]),
    )
    return {"task_id": artifact_id}


def search_ark_case(ark_root: Path, home_dir: Path, query: str, project_id: str, top_k: int) -> list[str]:
    output = run_ark_command(
        ark_root,
        home_dir,
        "search",
        query,
        "--project",
        project_id,
        "--limit",
        str(top_k),
    )
    return ARK_ID_RE.findall(output)


def run_geminipro_command(root: Path, cwd: Path, *args: str) -> str:
    return run_text_cli(
        [sys.executable, str(root / "genesis.py"), *args],
        cwd=cwd,
        timeout_s=GENESIS_CMD_TIMEOUT_S,
    )


def seed_geminipro_case(root: Path, cwd: Path, case: dict[str, Any]) -> dict[str, Any]:
    output = run_geminipro_command(
        root,
        cwd,
        "start",
        "--project",
        case["project_id"],
        "--agent",
        "phase5-benchmark",
        "--objective",
        case["goal"],
        "--tools",
        json.dumps(
            {
                "motivation": case["motivation"],
                "tool": case["run_start"]["tool_name"],
                "why": case["run_start"]["why_chosen"],
                "parameters": case["run_start"]["parameters"],
            },
            sort_keys=True,
        ),
    )
    match = GEMINIPRO_START_RE.search(output)
    if not match:
        raise RuntimeError(f"failed to parse geminipro task id from: {output}")
    task_id = match.group(1)
    run_geminipro_command(
        root,
        cwd,
        "update",
        "--task_id",
        task_id,
        "--status",
        "IN_PROGRESS",
        "--worked",
        case["progress"]["summary"],
        "--failed",
        "; ".join(case["progress"].get("failed_attempts", [])),
    )
    finish = case["finish"]
    run_geminipro_command(
        root,
        cwd,
        "update",
        "--task_id",
        task_id,
        "--status",
        "COMPLETED",
        "--worked",
        "; ".join(finish.get("what_worked", [])),
        "--failed",
        "; ".join(finish.get("what_failed", [])),
    )
    return {"task_id": task_id}


def search_geminipro_case(root: Path, cwd: Path, query: str, top_k: int) -> list[str]:
    output = run_geminipro_command(root, cwd, "search", query, "--limit", str(top_k))
    return GEMINIPRO_SEARCH_RE.findall(output)


def run_geminiultra_command(root: Path, db_path: Path, *args: str) -> str:
    env = os.environ.copy()
    env["GENESIS_DB"] = str(db_path)
    env["GENESIS_DIR"] = str(db_path.parent)
    return run_text_cli(
        [sys.executable, str(root / "genesis.py"), *args],
        cwd=root,
        env=env,
        timeout_s=GENESIS_CMD_TIMEOUT_S,
    )


def seed_geminiultra_case(root: Path, db_path: Path, case: dict[str, Any]) -> dict[str, Any]:
    objective = f"{case['goal']} Motivation: {case['motivation']}"
    output = run_geminiultra_command(
        root,
        db_path,
        "start",
        "--project",
        case["project_id"],
        "--objective",
        objective,
    )
    match = GEMINIULTRA_START_RE.search(output)
    if not match:
        raise RuntimeError(f"failed to parse geminiultra artifact id from: {output}")
    task_id = match.group(1)
    run_geminiultra_command(
        root,
        db_path,
        "update",
        "--id",
        task_id,
        "--progress",
        case["progress"]["summary"],
    )
    finish = case["finish"]
    run_geminiultra_command(
        root,
        db_path,
        "finish",
        "--id",
        task_id,
        "--worked",
        "; ".join(finish.get("what_worked", [])),
        "--failed",
        "; ".join(finish.get("what_failed", [])),
        "--tools",
        f"{case['run_start']['tool_name']}: {case['run_start']['why_chosen']}",
    )
    return {"task_id": task_id}


def search_geminiultra_case(root: Path, db_path: Path, query: str, top_k: int) -> list[str]:
    output = run_geminiultra_command(root, db_path, "search", "--query", query, "--limit", str(top_k))
    return GEMINIULTRA_SEARCH_RE.findall(output)


def summarize_task_level_queries(
    corpus: dict[str, Any],
    task_ids: dict[str, str],
    top_k: int,
    search_fn,
) -> dict[str, Any]:
    rows: list[QueryResult] = []
    for query in corpus["queries"]:
        case = next(case for case in corpus["cases"] if case["case_id"] == query["case_id"])
        started = time.perf_counter()
        ids = search_fn(case, query, top_k)
        latency_ms = (time.perf_counter() - started) * 1000.0
        expected_id = task_ids[query["case_id"]]
        rank = ids.index(expected_id) + 1 if expected_id in ids else None
        rows.append(
            QueryResult(
                query_id=query["id"],
                query_type=query["query_type"],
                query_text=query["query"],
                expected_case_id=query["case_id"],
                expected_facet=query["target_facet"],
                rank=rank,
                latency_ms=latency_ms,
            )
        )
    return summarize_query_results(rows)


def summarize_corpus_projects(corpus: dict[str, Any]) -> list[dict[str, Any]]:
    projects: dict[str, dict[str, Any]] = {}
    for case in corpus["cases"]:
        project = projects.setdefault(
            case["project_id"],
            {
                "project_id": case["project_id"],
                "case_ids": [],
                "datasets": [],
                "tools": [],
            },
        )
        project["case_ids"].append(case["case_id"])
        for dataset in case.get("dataset_refs", []):
            dataset_label = f"{dataset['name']}@{dataset.get('version', '')}"
            if dataset_label not in project["datasets"]:
                project["datasets"].append(dataset_label)
        tool_name = case["run_start"]["tool_name"]
        if tool_name not in project["tools"]:
            project["tools"].append(tool_name)
    return list(projects.values())


def run_benchmark(args: argparse.Namespace) -> dict[str, Any]:
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    benchmark_log(
        f"Loaded corpus {args.corpus.name} with {len(corpus['cases'])} cases and {len(corpus['queries'])} queries"
    )
    work_dir = args.data_root
    if work_dir.exists():
        shutil.rmtree(work_dir)
    work_dir.mkdir(parents=True, exist_ok=True)

    memd_cmd = [
        str(args.memd_path),
        "--mode",
        "mcp",
        "--data-dir",
        os.path.relpath(work_dir, REPO_ROOT),
        "--embedding-model",
        args.embedding_model,
        "--search-variant",
        args.system_variant,
    ]
    env = os.environ.copy()
    task_tenant = "phase5_task_memory"
    baseline_tenant = "phase5_chunk_baseline"

    with (args.json_out.parent / "task_memory_benchmark.log").open("w", encoding="utf-8") as log_file:
        benchmark_log("Running memd-native benchmark")
        client = McpStdioClient(memd_cmd, env, startup_timeout_s=60.0, stderr_file=log_file)
        client.start()
        try:
            initial_task_chunks = read_chunks_indexed(client, task_tenant)
            initial_baseline_chunks = read_chunks_indexed(client, baseline_tenant)

            baseline_seed_start = time.perf_counter()
            baseline_chunk_count = 0
            for case in corpus["cases"]:
                benchmark_log(f"memd chunk seed {case['case_id']}")
                baseline_chunk_count += seed_chunk_baseline_case(client, baseline_tenant, case)
            baseline_seed_ms = (time.perf_counter() - baseline_seed_start) * 1000.0
            baseline_wait_ms = wait_for_index_ready(
                client,
                baseline_tenant,
                initial_baseline_chunks + baseline_chunk_count,
            )

            task_seed_start = time.perf_counter()
            task_ids: dict[str, str] = {}
            task_projection_count = 0
            for case in corpus["cases"]:
                benchmark_log(f"memd task seed {case['case_id']}")
                seeded = seed_task_case(client, task_tenant, case)
                task_ids[case["case_id"]] = seeded["task_id"]
                task_projection_count += seeded["projection_count"]
            task_seed_ms = (time.perf_counter() - task_seed_start) * 1000.0
            task_wait_ms = wait_for_index_ready(
                client,
                task_tenant,
                initial_task_chunks + task_projection_count,
            )

            baseline_summary = run_baseline_queries(
                client, corpus, baseline_tenant, args.top_k, score_mode="task_level"
            )
            task_summary = run_task_queries(
                client, corpus, task_ids, task_tenant, args.top_k, score_mode="task_level"
            )
            baseline_freshness = run_baseline_freshness(client, baseline_tenant)
            task_freshness = run_task_freshness(client, task_tenant)
        finally:
            client.stop()
    benchmark_log("memd-native benchmark complete")

    # Concurrency uses separate processes against the same data dir.
    baseline_concurrency = run_concurrency(
        memd_cmd,
        env,
        writer=lambda client, worker_idx, op_idx: client.call_tool(
            "memory.add",
            {
                "tenant_id": baseline_tenant,
                "project_id": "phase5_concurrency_baseline",
                "text": f"baseline concurrency write worker={worker_idx} op={op_idx}",
                "type": "doc",
                "tags": [f"phase5:case:concurrency_{worker_idx}_{op_idx}", "phase5:facet:task_summary"],
            },
        ),
        workers=args.workers,
        ops_per_worker=args.ops_per_worker,
    )

    seed_client = McpStdioClient(memd_cmd, env, startup_timeout_s=60.0)
    seed_client.start()
    try:
        seeded_task = seed_client.call_tool(
            "task.start",
            {
                "tenant_id": task_tenant,
                "project_id": "phase5_concurrency_task",
                "goal": "Concurrency task seed",
                "motivation": "Need to measure concurrent task lifecycle writes",
                "hypothesis": "Concurrent task progress writes should succeed",
                "scientific_question": "Does concurrent progress logging remain reliable?",
                "dataset_refs": [{"name": "concurrency_probe", "version": "v1"}],
                "expected_outputs": ["concurrency progress artifacts"],
            },
        )
        concurrency_task_id = seeded_task["task_id"]
    finally:
        seed_client.stop()

    task_concurrency = run_concurrency(
        memd_cmd,
        env,
        writer=lambda client, worker_idx, op_idx: client.call_tool(
            "task.progress",
            {
                "tenant_id": task_tenant,
                "task_id": concurrency_task_id,
                "project_id": "phase5_concurrency_task",
                "summary": f"benchmark progress note worker={worker_idx} op={op_idx}",
                "blockers": [],
                "failed_attempts": [],
                "next_step": "continue benchmark",
            },
        ),
        workers=args.workers,
        ops_per_worker=args.ops_per_worker,
    )

    gpt54_live = None
    gpt54_root = args.gpt54_root.resolve() if args.gpt54_root else discover_gpt54_root()
    gpt54_db_path = work_dir / "gpt54" / "memory.duckdb"
    if gpt54_root and (gpt54_root / "amem" / "cli.py").exists():
        benchmark_log("Running gpt54 live CLI benchmark")
        gpt54_seed_start = time.perf_counter()
        gpt54_task_ids: dict[str, str] = {}
        for case in corpus["cases"]:
            benchmark_log(f"gpt54 cli seed {case['case_id']}")
            seeded = seed_gpt54_case(gpt54_root, gpt54_db_path, case)
            gpt54_task_ids[case["case_id"]] = seeded["task_id"]
        gpt54_seed_ms = (time.perf_counter() - gpt54_seed_start) * 1000.0
        gpt54_summary = run_gpt54_queries(
            gpt54_root, gpt54_db_path, corpus, gpt54_task_ids, args.top_k
        )
        gpt54_freshness = run_gpt54_freshness(gpt54_root, gpt54_db_path)
        gpt54_concurrency = run_gpt54_concurrency(
            gpt54_root, gpt54_db_path, args.workers, args.ops_per_worker
        )
        gpt54_live = {
            "mode": "gpt54 live CLI context search over the same seeded benchmark corpus",
            "seed_total_ms": gpt54_seed_ms,
            "search_quality": gpt54_summary,
            "freshness": gpt54_freshness,
            "concurrency": gpt54_concurrency,
            "task_ids": gpt54_task_ids,
            "root": os.path.relpath(gpt54_root, REPO_ROOT),
        }
        benchmark_log("gpt54 live CLI benchmark complete")

    gpt54_live_daemon = None
    if gpt54_root and (gpt54_root / "amem" / "daemon.py").exists():
        benchmark_log("Running gpt54 live daemon benchmark")
        daemon_db = work_dir / "gpt54_daemon" / "memory.duckdb"
        gpt54_daemon_seed_start = time.perf_counter()
        gpt54_daemon_task_ids = {}
        for case in corpus["cases"]:
            benchmark_log(f"gpt54 daemon seed {case['case_id']}")
            seeded = seed_gpt54_case(gpt54_root, daemon_db, case)
            gpt54_daemon_task_ids[case["case_id"]] = seeded["task_id"]
        gpt54_daemon_seed_ms = (time.perf_counter() - gpt54_daemon_seed_start) * 1000.0
        port = reserve_port()
        daemon_proc = subprocess.Popen(
            [sys.executable, "-m", "amem", "--db", str(daemon_db), "serve", "--host", "127.0.0.1", "--port", str(port)],
            cwd=str(gpt54_root),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        try:
            wait_for_port(port)
            daemon_url = f"http://127.0.0.1:{port}"
            call_fn = lambda *a: run_gpt54_daemon_command(gpt54_root, daemon_db, daemon_url, *a)
            gpt54_daemon_summary = run_gpt54_queries_with(call_fn, corpus, gpt54_daemon_task_ids, args.top_k)
            gpt54_daemon_freshness = run_gpt54_freshness_with(call_fn)
        finally:
            daemon_proc.terminate()
            try:
                daemon_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon_proc.kill()
                daemon_proc.wait(timeout=5)
        gpt54_daemon_concurrency = run_gpt54_concurrency(gpt54_root, daemon_db, args.workers, args.ops_per_worker)
        gpt54_live_daemon = {
            "mode": "gpt54 live daemon context search over the same seeded benchmark corpus",
            "seed_total_ms": gpt54_daemon_seed_ms,
            "search_quality": gpt54_daemon_summary,
            "freshness": gpt54_daemon_freshness,
            "concurrency": gpt54_daemon_concurrency,
            "task_ids": gpt54_daemon_task_ids,
            "root": os.path.relpath(gpt54_root, REPO_ROOT),
        }
        benchmark_log("gpt54 live daemon benchmark complete")

    gpt54_live_tantivy = None
    if gpt54_root and (gpt54_root / "amem" / "tantivy_backend.py").exists():
        benchmark_log("Running gpt54 live Tantivy benchmark")
        tantivy_db = work_dir / "gpt54_tantivy" / "memory.duckdb"
        gpt54_tantivy_seed_start = time.perf_counter()
        gpt54_tantivy_task_ids = {}
        for case in corpus["cases"]:
            benchmark_log(f"gpt54 tantivy seed {case['case_id']}")
            seeded = seed_gpt54_case(gpt54_root, tantivy_db, case)
            gpt54_tantivy_task_ids[case["case_id"]] = seeded["task_id"]
        gpt54_tantivy_seed_ms = (time.perf_counter() - gpt54_tantivy_seed_start) * 1000.0

        tantivy_port = reserve_port()
        tantivy_proc = subprocess.Popen(
            [sys.executable, "-m", "amem", "--db", str(tantivy_db), "tantivy-serve", "--host", "127.0.0.1", "--port", str(tantivy_port)],
            cwd=str(gpt54_root),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        try:
            wait_for_port(tantivy_port, timeout_s=30.0)
            tantivy_url = f"http://127.0.0.1:{tantivy_port}/search"
            warm_gpt54_tantivy(corpus, tantivy_db, tantivy_url)
            gpt54_tantivy_summary = run_gpt54_tantivy_queries(
                tantivy_db, tantivy_url, corpus, gpt54_tantivy_task_ids, args.top_k
            )
        finally:
            tantivy_proc.terminate()
            try:
                tantivy_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                tantivy_proc.kill()
                tantivy_proc.wait(timeout=5)

        # Freshness for Tantivy requires rebuilding the served index after the new write.
        fresh_start = run_gpt54_cli(
            gpt54_root,
            tantivy_db,
            "start",
            "--project",
            "phase5_freshness_task",
            "--title",
            "Freshness probe for task search",
            "--goal",
            "Freshness probe for task search",
            "--agent",
            "phase5-benchmark",
        )
        fresh_task_id = fresh_start["task_id"]
        run_gpt54_cli(
            gpt54_root,
            tantivy_db,
            "finish",
            "--task-id",
            fresh_task_id,
            "--summary",
            "Freshness benchmark complete",
            "--outcome",
            "success",
            "--worked-json",
            json.dumps([{"summary": "Fresh artifact should appear in the first search immediately", "why": "freshness probe"}]),
            "--failed-json",
            json.dumps([{"summary": "immediately searchable failure memory", "why": "freshness probe"}]),
            "--validation-json",
            json.dumps([{"summary": "Freshness benchmark setup complete", "why": "freshness probe"}]),
        )
        fresh_port = reserve_port()
        fresh_proc = subprocess.Popen(
            [sys.executable, "-m", "amem", "--db", str(tantivy_db), "tantivy-serve", "--host", "127.0.0.1", "--port", str(fresh_port)],
            cwd=str(gpt54_root),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        try:
            wait_for_port(fresh_port, timeout_s=30.0)
            fresh_url = f"http://127.0.0.1:{fresh_port}/search"
            warm_gpt54_tantivy(corpus, tantivy_db, fresh_url)
            started = time.perf_counter()
            payload = query_gpt54_tantivy(
                tantivy_db,
                fresh_url,
                {
                    "project_slug": "phase5_freshness_task",
                    "query": "immediately searchable failure memory",
                    "limit": 3,
                    "status": None,
                    "kinds": ["failed"],
                    "dataset_names": None,
                    "entity_names": None,
                    "tool_names": None,
                },
            )
            gpt54_tantivy_freshness = {
                "found": find_rank(
                    payload.get("hits", []),
                    lambda result: result.get("task_id") == fresh_task_id and result.get("kind") == "failed",
                )
                is not None,
                "rank": find_rank(
                    payload.get("hits", []),
                    lambda result: result.get("task_id") == fresh_task_id and result.get("kind") == "failed",
                ),
                "search_ms": (time.perf_counter() - started) * 1000.0,
            }
        finally:
            fresh_proc.terminate()
            try:
                fresh_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                fresh_proc.kill()
                fresh_proc.wait(timeout=5)

        gpt54_tantivy_concurrency = run_gpt54_concurrency(gpt54_root, tantivy_db, args.workers, args.ops_per_worker)
        gpt54_live_tantivy = {
            "mode": "gpt54 live warm Tantivy service search over the same seeded benchmark corpus",
            "seed_total_ms": gpt54_tantivy_seed_ms,
            "search_quality": gpt54_tantivy_summary,
            "freshness": gpt54_tantivy_freshness,
            "concurrency": gpt54_tantivy_concurrency,
            "task_ids": gpt54_tantivy_task_ids,
            "root": os.path.relpath(gpt54_root, REPO_ROOT),
        }
        benchmark_log("gpt54 live Tantivy benchmark complete")

    genesism_root = args.genesism_root.resolve() if args.genesism_root else discover_genesism_root()
    external_live_systems: dict[str, Any] = {}
    if gpt54_live:
        external_live_systems["gpt54_live_cli"] = gpt54_live
    if gpt54_live_daemon:
        external_live_systems["gpt54_live_daemon"] = gpt54_live_daemon
    if gpt54_live_tantivy:
        external_live_systems["gpt54_live_tantivy"] = gpt54_live_tantivy

    if genesism_root:
        claude_root = (args.claude_root.resolve() if args.claude_root else genesism_root / "claude")
        if (claude_root / "ark.py").exists():
            benchmark_log("Running claude ark benchmark")
            ark_home = work_dir / "claude_ark_home"
            claude_seed_start = time.perf_counter()
            claude_ids = {}
            for case in corpus["cases"]:
                benchmark_log(f"claude ark seed {case['case_id']}")
                seeded = seed_ark_case(claude_root, ark_home, case)
                claude_ids[case["case_id"]] = seeded["task_id"]
            claude_seed_ms = (time.perf_counter() - claude_seed_start) * 1000.0
            claude_summary = summarize_task_level_queries(
                corpus,
                claude_ids,
                args.top_k,
                lambda case, query, top_k: search_ark_case(claude_root, ark_home, query["query"], case["project_id"], top_k),
            )
            # freshness
            fresh_output = run_ark_command(
                claude_root, ark_home, "start", "Freshness probe for task search",
                "--agent", "phase5-benchmark", "--project", "phase5_freshness_task",
                "--motivation", "Need to verify immediate retrievability after completion"
            )
            match = ARK_START_RE.search(fresh_output)
            fresh_id = match.group(1) if match else None
            if fresh_id:
                run_ark_command(
                    claude_root, ark_home, "finish", fresh_id,
                    "--outcome", "Freshness benchmark complete",
                    "--worked", "Fresh artifact should appear in the first search immediately",
                    "--failed", "immediately searchable failure memory",
                    "--status", "completed",
                )
            started = time.perf_counter()
            ids = search_ark_case(claude_root, ark_home, "immediately searchable failure memory", "phase5_freshness_task", 3)
            claude_freshness = {"found": fresh_id in ids if fresh_id else False, "rank": ids.index(fresh_id)+1 if fresh_id in ids else None, "search_ms": (time.perf_counter()-started)*1000.0}
            # concurrency
            seed = run_ark_command(claude_root, ark_home, "start", "Concurrency task seed", "--agent", "phase5-benchmark", "--project", "phase5_concurrency_task")
            match = ARK_START_RE.search(seed)
            conc_id = match.group(1) if match else None
            claude_concurrency = run_concurrency(
                None,
                None,
                lambda _client, worker_idx, op_idx: run_ark_command(
                    claude_root, ark_home, "update", conc_id, "--progress", f"benchmark progress note worker={worker_idx} op={op_idx}"
                ),
                args.workers,
                args.ops_per_worker,
            )
            external_live_systems["claude_live"] = {
                "mode": "claude ark live CLI artifact search on the same benchmark corpus",
                "seed_total_ms": claude_seed_ms,
                "search_quality": claude_summary,
                "freshness": claude_freshness,
                "concurrency": claude_concurrency,
                "scoring_mode": "task_level",
                "root": os.path.relpath(claude_root, REPO_ROOT),
            }
            benchmark_log("claude ark benchmark complete")

        geminipro_root = (args.geminipro_root.resolve() if args.geminipro_root else genesism_root / "geminipro")
        if (geminipro_root / "genesis.py").exists():
            benchmark_log("Running geminipro benchmark")
            geminipro_work = work_dir / "geminipro"
            geminipro_work.mkdir(parents=True, exist_ok=True)
            gp_seed_start = time.perf_counter()
            gp_ids = {}
            for case in corpus["cases"]:
                benchmark_log(f"geminipro seed {case['case_id']}")
                seeded = seed_geminipro_case(geminipro_root, geminipro_work, case)
                gp_ids[case["case_id"]] = seeded["task_id"]
            gp_seed_ms = (time.perf_counter() - gp_seed_start) * 1000.0
            gp_summary = summarize_task_level_queries(
                corpus,
                gp_ids,
                args.top_k,
                lambda case, query, top_k: search_geminipro_case(geminipro_root, geminipro_work, query["query"], top_k),
            )
            fresh = seed_geminipro_case(
                geminipro_root,
                geminipro_work,
                {
                    "case_id": "freshness_probe",
                    "project_id": "phase5_freshness_task",
                    "goal": "Freshness probe for task search",
                    "motivation": "Need to verify immediate retrievability after completion",
                    "hypothesis": "A newly finished task should be searchable immediately",
                    "scientific_question": "How quickly does a new task become retrievable?",
                    "run_start": {"parameters": {}, "tool_name": "freshness", "why_chosen": "probe"},
                    "progress": {"summary": "fresh progress", "failed_attempts": ["immediately searchable failure memory"], "blockers": [], "next_step": "none"},
                    "finish": {"what_worked": ["Fresh artifact should appear in the first search immediately"], "what_failed": ["immediately searchable failure memory"]},
                },
            )
            started = time.perf_counter()
            ids = search_geminipro_case(geminipro_root, geminipro_work, "immediately searchable failure memory", 3)
            gp_freshness = {"found": fresh["task_id"] in ids, "rank": ids.index(fresh["task_id"])+1 if fresh["task_id"] in ids else None, "search_ms": (time.perf_counter()-started)*1000.0}
            conc_seed = run_geminipro_command(geminipro_root, geminipro_work, "start", "--project", "phase5_concurrency_task", "--agent", "phase5-benchmark", "--objective", "Concurrency task seed", "--tools", "{}")
            match = GEMINIPRO_START_RE.search(conc_seed)
            conc_id = match.group(1) if match else None
            gp_concurrency = run_concurrency(
                None,
                None,
                lambda _client, worker_idx, op_idx: run_geminipro_command(
                    geminipro_root, geminipro_work, "update", "--task_id", conc_id, "--status", "IN_PROGRESS", "--worked", f"progress worker={worker_idx} op={op_idx}", "--failed", ""
                ),
                args.workers,
                args.ops_per_worker,
            )
            external_live_systems["geminipro_live"] = {
                "mode": "geminipro live CLI search on the same benchmark corpus",
                "seed_total_ms": gp_seed_ms,
                "search_quality": gp_summary,
                "freshness": gp_freshness,
                "concurrency": gp_concurrency,
                "scoring_mode": "task_level",
                "root": os.path.relpath(geminipro_root, REPO_ROOT),
            }
            benchmark_log("geminipro benchmark complete")

        geminiultra_root = (args.geminiultra_root.resolve() if args.geminiultra_root else genesism_root / "geminiultra")
        if (geminiultra_root / "genesis.py").exists():
            benchmark_log("Running geminiultra benchmark")
            gu_db = work_dir / "geminiultra" / "memory.duckdb"
            gu_db.parent.mkdir(parents=True, exist_ok=True)
            gu_seed_start = time.perf_counter()
            gu_ids = {}
            for case in corpus["cases"]:
                benchmark_log(f"geminiultra seed {case['case_id']}")
                seeded = seed_geminiultra_case(geminiultra_root, gu_db, case)
                gu_ids[case["case_id"]] = seeded["task_id"]
            gu_seed_ms = (time.perf_counter() - gu_seed_start) * 1000.0
            gu_summary = summarize_task_level_queries(
                corpus,
                gu_ids,
                args.top_k,
                lambda case, query, top_k: search_geminiultra_case(geminiultra_root, gu_db, query["query"], top_k),
            )
            fresh = seed_geminiultra_case(
                geminiultra_root,
                gu_db,
                {
                    "case_id": "freshness_probe",
                    "project_id": "phase5_freshness_task",
                    "goal": "Freshness probe for task search",
                    "motivation": "Need to verify immediate retrievability after completion",
                    "run_start": {"tool_name": "freshness", "why_chosen": "probe"},
                    "progress": {"summary": "fresh progress"},
                    "finish": {"what_worked": ["Fresh artifact should appear in the first search immediately"], "what_failed": ["immediately searchable failure memory"]},
                },
            )
            started = time.perf_counter()
            ids = search_geminiultra_case(geminiultra_root, gu_db, "immediately searchable failure memory", 3)
            gu_freshness = {"found": fresh["task_id"] in ids, "rank": ids.index(fresh["task_id"])+1 if fresh["task_id"] in ids else None, "search_ms": (time.perf_counter()-started)*1000.0}
            conc_seed = run_geminiultra_command(geminiultra_root, gu_db, "start", "--project", "phase5_concurrency_task", "--objective", "Concurrency task seed")
            match = GEMINIULTRA_START_RE.search(conc_seed)
            conc_id = match.group(1) if match else None
            gu_concurrency = run_concurrency(
                None,
                None,
                lambda _client, worker_idx, op_idx: run_geminiultra_command(
                    geminiultra_root, gu_db, "update", "--id", conc_id, "--progress", f"progress worker={worker_idx} op={op_idx}"
                ),
                args.workers,
                args.ops_per_worker,
            )
            external_live_systems["geminiultra_live"] = {
                "mode": "geminiultra live CLI search on the same benchmark corpus",
                "seed_total_ms": gu_seed_ms,
                "search_quality": gu_summary,
                "freshness": gu_freshness,
                "concurrency": gu_concurrency,
                "scoring_mode": "task_level",
                "root": os.path.relpath(geminiultra_root, REPO_ROOT),
            }
            benchmark_log("geminiultra benchmark complete")

    report = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "corpus_path": os.path.relpath(args.corpus.resolve(), REPO_ROOT),
        "corpus_description": corpus["description"],
        "corpus_version": corpus["version"],
        "corpus_notes": corpus.get("notes", {}),
        "corpus_case_count": len(corpus["cases"]),
        "corpus_query_count": len(corpus["queries"]),
        "corpus_projects": summarize_corpus_projects(corpus),
        "top_k": args.top_k,
        "workers": args.workers,
        "ops_per_worker": args.ops_per_worker,
        "embedding_model": args.embedding_model,
        "system_variant": args.system_variant,
        "systems": {
            "memd_chunk_baseline": {
                "mode": "memory.search over flattened chunk-native benchmark artifacts",
                "seed_total_ms": baseline_seed_ms,
                "index_ready_wait_ms": baseline_wait_ms,
                "documents_seeded": baseline_chunk_count,
                "search_quality": baseline_summary,
                "freshness": baseline_freshness,
                "concurrency": baseline_concurrency,
            },
            "memd_task_memory": {
                "mode": "task.* lifecycle writes plus task.search over exact-filtered task artifacts",
                "seed_total_ms": task_seed_ms,
                "index_ready_wait_ms": task_wait_ms,
                "documents_seeded": task_projection_count,
                "task_ids": task_ids,
                "search_quality": task_summary,
                "freshness": task_freshness,
                "concurrency": task_concurrency,
            },
        },
        "external_live": external_live_systems,
        "external_reference": {
            "genesism_unified_benchmark": load_genesism_reference(args.genesism_reference_json)
        },
        "methodology": {
            "notes": [
                "The chunk baseline flattens each task case into plain memory chunks and uses memory.search.",
                "The task-memory mode seeds the real task lifecycle and queries task.search with task-aware filters.",
                "Freshness measures immediate retrievability after a new write.",
                "Concurrency launches separate memd stdio processes against the same data dir to approximate concurrent CLI usage.",
                "Quality scoring is task-level across all systems so flatter schemas can participate correctly.",
                "The gpt54 Tantivy variant applies DuckDB-side task filters and then times direct HTTP requests against a warmed Tantivy service."
            ]
        },
    }
    return report


def render_markdown(report: dict[str, Any]) -> str:
    systems = report["systems"]
    corpus_notes = report.get("corpus_notes", {})
    corpus_projects = report.get("corpus_projects", [])
    lines = [
        "# Phase 5 Task Memory Benchmark",
        "",
        f"- Corpus: `{report['corpus_path']}`",
        f"- Version: `{report['corpus_version']}`",
        f"- Generated at: `{report['generated_at']}`",
        f"- Embedding model: `{report['embedding_model']}`",
        f"- Search variant: `{report['system_variant']}`",
        "",
        "## Corpus Design",
        "",
        f"- Cases: `{report.get('corpus_case_count', 0)}`",
        f"- Queries: `{report.get('corpus_query_count', 0)}`",
        f"- Shared-project sibling groups: `{len(corpus_projects)}`",
    ]
    if corpus_notes.get("purpose"):
        lines.append(f"- Purpose: {corpus_notes['purpose']}")
    if corpus_notes.get("hardening"):
        lines.append(f"- Hardening: {corpus_notes['hardening']}")
    if corpus_projects:
        lines.extend(
            [
                "",
                "| Project | Cases | Shared datasets | Shared tools |",
                "|---|---|---|---|",
            ]
        )
        for project in corpus_projects:
            lines.append(
                "| {project} | {cases} | {datasets} | {tools} |".format(
                    project=project["project_id"],
                    cases="<br>".join(project["case_ids"]),
                    datasets="<br>".join(project["datasets"]),
                    tools="<br>".join(project["tools"]),
                )
            )

    lines.extend(
        [
            "",
        "## memd-native comparison",
        "",
        "| System | Mode | hit@3 | MRR | avg search ms | p95 search ms | Fresh rank | Concurrency success | Concurrency ops/s | Seed ms |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for name, payload in systems.items():
        search = payload["search_quality"]
        freshness = payload["freshness"]
        concurrency = payload["concurrency"]
        lines.append(
            "| {name} | {mode} | {hit3:.2f} | {mrr:.2f} | {avg:.1f} | {p95:.1f} | {fresh} | {success:.2%} | {ops:.2f} | {seed:.1f} |".format(
                name=name,
                mode=payload["mode"],
                hit3=search["hit3"],
                mrr=search["mrr"],
                avg=search["avg_search_ms"],
                p95=search["p95_search_ms"],
                fresh=freshness["rank"] or "miss",
                success=concurrency["success_rate"],
                ops=concurrency["ops_per_sec"],
                seed=payload["seed_total_ms"],
            )
        )

    baseline = systems["memd_chunk_baseline"]
    task_memory = systems["memd_task_memory"]
    lines.extend(
        [
            "",
            "## Why memd-native Modes Differ",
            "",
            (
                f"- `memd_chunk_baseline` flattened the corpus into `{baseline['documents_seeded']}` generic chunks and searched them with "
                f"`memory.search`. On the hardened sibling-task corpus, that representation produced "
                f"`hit@3={baseline['search_quality']['hit3']:.2f}` and `MRR={baseline['search_quality']['mrr']:.2f}`."
            ),
            (
                f"- `memd_task_memory` wrote `{task_memory['documents_seeded']}` lifecycle projections and searched them with `task.search`, "
                f"exact artifact filters, and candidate reranking. That increased seed time from `{baseline['seed_total_ms'] / 1000.0:.1f}s` to "
                f"`{task_memory['seed_total_ms'] / 1000.0:.1f}s`, changed retrieval from `hit@3={baseline['search_quality']['hit3']:.2f}` / "
                f"`MRR={baseline['search_quality']['mrr']:.2f}` to `hit@3={task_memory['search_quality']['hit3']:.2f}` / "
                f"`MRR={task_memory['search_quality']['mrr']:.2f}`, and changed average search latency from "
                f"`{baseline['search_quality']['avg_search_ms']:.1f}ms` to `{task_memory['search_quality']['avg_search_ms']:.1f}ms`."
            ),
        ]
    )

    external_live = report.get("external_live", {})
    if external_live:
        lines.extend(
            [
                "",
                "## Live external comparison",
                "",
                "| System | Mode | hit@3 | MRR | avg search ms | p95 search ms | Fresh rank | Concurrency success | Concurrency ops/s | Seed ms |",
                "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for name, payload in external_live.items():
            search = payload["search_quality"]
            freshness = payload["freshness"]
            concurrency = payload["concurrency"]
            lines.append(
                "| {name} | {mode} | {hit3:.2f} | {mrr:.2f} | {avg:.1f} | {p95:.1f} | {fresh} | {success:.2%} | {ops:.2f} | {seed:.1f} |".format(
                    name=name,
                    mode=payload["mode"],
                    hit3=search["hit3"],
                    mrr=search["mrr"],
                    avg=search["avg_search_ms"],
                    p95=search["p95_search_ms"],
                    fresh=freshness["rank"] or "miss",
                    success=concurrency["success_rate"],
                    ops=concurrency["ops_per_sec"],
                    seed=payload["seed_total_ms"],
                )
            )

    reference = report.get("external_reference", {}).get("genesism_unified_benchmark")
    if reference:
        lines.extend(
            [
                "",
                "## GenesisM unified benchmark reference",
                "",
                "These numbers are imported from GenesisM's unified benchmark and are not directly comparable to the memd-native Phase 5 task benchmark because the old GenesisM `memd` measurement predated memd's task-lifecycle tools.",
                "",
                "| External system | Search backend | lifecycle ms | hit@3 | MRR | avg search ms | fresh rank | concurrency success | concurrency ops/s |",
                "|---|---|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for name, payload in reference.items():
            lines.append(
                "| {name} | {backend} | {life:.1f} | {hit3:.2f} | {mrr:.2f} | {avg:.1f} | {fresh} | {success:.2%} | {ops:.2f} |".format(
                    name=name,
                    backend=payload["search_backend"],
                    life=payload["lifecycle_ms"],
                    hit3=payload["hit3"],
                    mrr=payload["mrr"],
                    avg=payload["avg_search_ms"],
                    fresh=payload["fresh_rank"] or "miss",
                    success=payload["concurrency_success_rate"],
                    ops=payload["concurrency_ops_per_sec"],
                )
            )

    lines.extend(
        [
            "",
            "## Reproducibility",
            "",
            "- Primary entrypoint: `python3 evals/bench/tools/task_memory_benchmark.py --memd-path target/debug/memd`",
            f"- Corpus source-of-truth: `{report['corpus_path']}`",
            "- External checkouts expected nearby: `gpt54`, `claude`, `geminipro`, and `geminiultra` under a GenesisM workspace, or passed explicitly with the `--*-root` flags.",
            "- The runner prints stage progress so long external sections are visible during execution.",
            f"- External CLI timeouts: `ark={ARK_CMD_TIMEOUT_S:.0f}s`, `geminipro={GENESIS_CMD_TIMEOUT_S:.0f}s`, `geminiultra={GENESIS_CMD_TIMEOUT_S:.0f}s`.",
            "",
            "## Interpretation",
            "",
            "- `memd_chunk_baseline` shows how well plain chunk retrieval works when the same knowledge is flattened into generic memory chunks.",
            "- `memd_task_memory` shows how much the structured task lifecycle plus exact filters improve retrieval of failures, parameters, evidence, and why-chosen rationale.",
            "- The GenesisM reference section is included to preserve continuity with the prior cross-system benchmark work.",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run the memd Phase 5 task-memory benchmark.")
    parser.add_argument(
        "--memd-path",
        default="target/debug/memd",
        type=Path,
        help="Path to the memd binary",
    )
    parser.add_argument(
        "--corpus",
        default=DEFAULT_CORPUS,
        type=Path,
        help="Path to the task-memory benchmark corpus JSON",
    )
    parser.add_argument(
        "--json-out",
        default=DEFAULT_JSON_OUT,
        type=Path,
        help="Where to write the JSON report",
    )
    parser.add_argument(
        "--markdown-out",
        default=DEFAULT_MARKDOWN_OUT,
        type=Path,
        help="Where to write the Markdown report",
    )
    parser.add_argument(
        "--data-root",
        default=DEFAULT_DATA_ROOT,
        type=Path,
        help="Scratch data directory for the benchmark run",
    )
    parser.add_argument(
        "--genesism-root",
        type=Path,
        default=None,
        help="Optional path to the GenesisM workspace root; if omitted the runner will try to discover one relative to this repo",
    )
    parser.add_argument(
        "--gpt54-root",
        type=Path,
        default=None,
        help="Optional path to a live gpt54 checkout; if omitted the runner will try to discover one relative to the memd repo",
    )
    parser.add_argument("--claude-root", type=Path, default=None, help="Optional path to the GenesisM claude/ark checkout")
    parser.add_argument("--geminipro-root", type=Path, default=None, help="Optional path to the GenesisM geminipro checkout")
    parser.add_argument("--geminiultra-root", type=Path, default=None, help="Optional path to the GenesisM geminiultra checkout")
    parser.add_argument("--top-k", type=int, default=5, help="Top-k depth for searches")
    parser.add_argument("--workers", type=int, default=4, help="Concurrency workers")
    parser.add_argument("--ops-per-worker", type=int, default=2, help="Concurrency ops per worker")
    parser.add_argument(
        "--embedding-model",
        default="all-minilm",
        help="Embedding model passed to memd",
    )
    parser.add_argument(
        "--system-variant",
        default="hybrid-feature",
        help="Search variant passed to memd",
    )
    parser.add_argument(
        "--genesism-reference-json",
        default=None,
        type=Path,
        help="Optional GenesisM unified benchmark JSON for reference comparison",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.genesism_reference_json is None:
        discovered_genesism = args.genesism_root.resolve() if args.genesism_root else discover_genesism_root()
        if discovered_genesism is not None:
            candidate = discovered_genesism / "unified_benchmark_results.json"
            args.genesism_reference_json = candidate if candidate.exists() else None
    if not args.memd_path.exists():
        raise FileNotFoundError(f"memd binary not found: {args.memd_path}")
    if not args.corpus.exists():
        raise FileNotFoundError(f"benchmark corpus not found: {args.corpus}")
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    args.markdown_out.parent.mkdir(parents=True, exist_ok=True)

    report = run_benchmark(args)
    args.json_out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    args.markdown_out.write_text(render_markdown(report), encoding="utf-8")
    print(f"Wrote JSON report to {args.json_out}")
    print(f"Wrote Markdown report to {args.markdown_out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
