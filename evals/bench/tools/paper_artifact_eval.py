#!/usr/bin/env python3
"""Run a three-agent paper coordination benchmark against a shared memd daemon.

The benchmark uses:
- agent 1: Codex CLI
- agent 2: Claude Code CLI
- agent 3: Codex CLI with access to earlier artifacts

Each agent reads the same local paper briefing, writes task artifacts into the
same tenant/challenge, and returns a structured summary. The script then queries
memd for the resulting artifacts and task histories and writes a benchmark
summary to disk.
"""

from __future__ import annotations

import argparse
import json
import re
import signal
import subprocess
import sys
import textwrap
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from html import unescape
from pathlib import Path
from typing import Any

import requests


REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_URL = "https://doi.org/10.1111/jeu.13003"
DEFAULT_TENANT = "paper_eval_jeu13003"
DEFAULT_PROJECT = "kaonashia_paper_eval"
DEFAULT_CHALLENGE = "jeu13003_shared_challenge"


@dataclass
class AgentRun:
    label: str
    cli: str
    prompt_path: Path
    stdout_path: Path
    returncode: int
    duration_s: float
    parsed_output: dict[str, Any] | None


def strip_tags(text: str) -> str:
    return unescape(re.sub(r"<[^>]+>", " ", text))


def fetch_paper_metadata(doi_url: str) -> dict[str, Any]:
    doi = doi_url
    if doi_url.startswith("https://doi.org/"):
        doi = doi_url.split("https://doi.org/", 1)[1]

    crossref = requests.get(
        f"https://api.crossref.org/works/{doi}",
        timeout=30,
        headers={"User-Agent": "memd-paper-artifact-eval/0.1"},
    )
    crossref.raise_for_status()
    msg = crossref.json()["message"]

    openalex = requests.get(
        f"https://api.openalex.org/works?filter=doi:{doi}",
        timeout=30,
        headers={"User-Agent": "memd-paper-artifact-eval/0.1"},
    )
    openalex.raise_for_status()
    oa_results = openalex.json().get("results", [])
    oa = oa_results[0] if oa_results else {}

    title = strip_tags((msg.get("title") or [doi])[0])
    abstract = strip_tags(msg.get("abstract") or "").strip()
    authors = []
    for author in msg.get("author", []):
        given = author.get("given", "").strip()
        family = author.get("family", "").strip()
        name = " ".join(part for part in [given, family] if part)
        if name:
            authors.append(name)

    return {
        "doi": doi,
        "source_url": doi_url,
        "title": title,
        "abstract": abstract,
        "journal": (msg.get("container-title") or [None])[0],
        "published_print": msg.get("published-print", {}),
        "authors": authors,
        "pdf_url": oa.get("primary_location", {}).get("pdf_url"),
        "source_note": (
            "The Wiley full-text endpoint returned HTTP 403 during automated fetch, "
            "so this benchmark grounds agents on Crossref/OpenAlex title, abstract, "
            "journal metadata, and DOI."
        ),
    }


def write_paper_brief(path: Path, paper: dict[str, Any]) -> None:
    content = f"""# Paper Brief

Title: {paper['title']}
DOI: {paper['doi']}
Source URL: {paper['source_url']}
Journal: {paper.get('journal') or 'unknown'}
Authors: {', '.join(paper.get('authors') or [])}

## Source Note
{paper['source_note']}

## Abstract
{paper['abstract']}
"""
    path.write_text(content, encoding="utf-8")


def wait_for_mcp(url: str, timeout_s: float = 30.0) -> None:
    deadline = time.time() + timeout_s
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "paper-artifact-eval", "version": "0.1.0"},
        },
    }
    while time.time() < deadline:
        try:
            resp = requests.post(
                url,
                json=payload,
                timeout=5,
                headers={
                    "Accept": "application/json, text/event-stream",
                    "Content-Type": "application/json",
                },
            )
            if resp.ok:
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise RuntimeError(f"memd MCP server did not become ready at {url}")


def mcp_tool_call(url: str, name: str, arguments: dict[str, Any], req_id: int) -> dict[str, Any]:
    payload = {
        "jsonrpc": "2.0",
        "id": req_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    resp = requests.post(
        url,
        json=payload,
        timeout=30,
        headers={
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        },
    )
    resp.raise_for_status()
    body = resp.json()
    if "error" in body:
        raise RuntimeError(f"MCP tool error for {name}: {body['error']}")
    text = body["result"]["content"][0]["text"]
    return json.loads(text)


def build_schema(path: Path) -> None:
    schema = {
        "type": "object",
        "properties": {
            "agent_label": {"type": "string"},
            "paper_summary": {"type": "string"},
            "chosen_research_task": {"type": "string"},
            "rationale": {"type": "string"},
            "task_id": {"type": "string"},
            "artifact_id": {"type": "string"},
            "challenge_id": {"type": "string"},
            "complementary_to": {
                "type": "array",
                "items": {"type": "string"},
            },
        },
        "required": [
            "agent_label",
            "paper_summary",
            "chosen_research_task",
            "rationale",
            "task_id",
            "artifact_id",
            "challenge_id",
            "complementary_to",
        ],
        "additionalProperties": False,
    }
    path.write_text(json.dumps(schema, indent=2), encoding="utf-8")


def build_prompt(
    agent_label: str,
    paper_path: Path,
    paper: dict[str, Any],
    tenant_id: str,
    project_id: str,
    challenge_id: str,
    agent_id: str,
    session_id: str,
    complementary: bool,
) -> str:
    complement_text = (
        "You must inspect existing memd artifacts and choose a research task that is clearly "
        "complementary to the earlier agents rather than duplicative. Reuse their artifact IDs "
        "in your final response if they influenced your choice."
        if complementary
        else
        "Choose one concrete research direction grounded in the paper. If earlier artifacts "
        "already exist, avoid duplicating them."
    )
    artifact_role = "complementary_direction" if complementary else "paper_readout"
    return textwrap.dedent(
        f"""
        You are {agent_label} in a shared memd paper-coordination benchmark.

        Use the local paper briefing at:
        {paper_path}

        You should not need web access or codebase search. The benchmark paper
        content is included here directly for stability.

        Paper title:
        {paper['title']}

        Source note:
        {paper['source_note']}

        Abstract:
        {paper['abstract']}

        Use these memd identifiers exactly:
        - tenant_id: {tenant_id}
        - project_id: {project_id}
        - challenge_id: {challenge_id}
        - agent_id: {agent_id}
        - session_id: {session_id}

        Requirements:
        1. Ground yourself on the paper content above and the local paper briefing.
        2. Search memd first using artifact.search and/or task.search.
        3. Pick one concrete follow-up research task grounded only in the paper briefing.
        4. Record your work in memd:
           - task.start
           - task.add_evidence or task.progress
           - task.finish
           - artifact.create
        5. Only use the memd tools above plus ordinary file reading if needed.
           Do not use code.*, debug.*, context.*, or memory.metrics for this benchmark.
        6. For artifact.create use:
           - artifact_kind = "review"
           - artifact_role = "{artifact_role}"
           - challenge_id = "{challenge_id}"
           - requested_action = "research_task"
           - verification_status = "pending"
           - task_id = the task_id you created with task.start
        7. {complement_text}
        8. Do not edit repository files.
        9. Do not only describe tool calls; actually call memd tools.

        Your final response must be valid JSON matching the provided schema with:
        - agent_label
        - paper_summary
        - chosen_research_task
        - rationale
        - task_id
        - artifact_id
        - challenge_id
        - complementary_to
        """
    ).strip()


def run_codex(
    agent_label: str,
    prompt: str,
    schema_path: Path,
    output_path: Path,
    mcp_url: str,
) -> AgentRun:
    start = time.time()
    cmd = [
        "codex",
        "-a",
        "never",
        "-s",
        "read-only",
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "-C",
        str(REPO_ROOT),
        "-c",
        f'mcp_servers.memd.url="{mcp_url}"',
        "--output-schema",
        str(schema_path),
        "-o",
        str(output_path),
        prompt,
    ]
    proc = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=900,
    )
    (output_path.parent / f"{output_path.stem}.stdout.log").write_text(
        proc.stdout,
        encoding="utf-8",
    )
    parsed = None
    raw_output = output_path.read_text(encoding="utf-8") if output_path.exists() else proc.stdout
    if not output_path.exists():
        output_path.write_text(raw_output, encoding="utf-8")
    try:
        parsed = json.loads(raw_output)
    except Exception:
        parsed = None
    return AgentRun(
        label=agent_label,
        cli="codex",
        prompt_path=Path(),
        stdout_path=output_path,
        returncode=proc.returncode,
        duration_s=time.time() - start,
        parsed_output=parsed,
    )


def run_claude(
    agent_label: str,
    prompt: str,
    schema_path: Path,
    output_path: Path,
    mcp_config_path: Path,
) -> AgentRun:
    start = time.time()
    cmd = [
        "claude",
        "-p",
        "--output-format",
        "json",
        "--json-schema",
        schema_path.read_text(encoding="utf-8"),
        "--permission-mode",
        "bypassPermissions",
        "--mcp-config",
        str(mcp_config_path),
        "--strict-mcp-config",
        "--add-dir",
        str(REPO_ROOT),
    ]
    proc = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        input=prompt,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=900,
    )
    output_path.write_text(proc.stdout, encoding="utf-8")
    parsed = None
    try:
        raw = json.loads(proc.stdout)
        parsed = raw.get("structured_output", raw)
    except Exception:
        parsed = None
    return AgentRun(
        label=agent_label,
        cli="claude",
        prompt_path=Path(),
        stdout_path=output_path,
        returncode=proc.returncode,
        duration_s=time.time() - start,
        parsed_output=parsed,
    )


def summarize_benchmark(
    paper: dict[str, Any],
    artifact_results: dict[str, Any],
    task_histories: dict[str, Any],
    agent_runs: list[AgentRun],
) -> dict[str, Any]:
    unique_tasks = []
    for run in agent_runs:
        if run.parsed_output and run.parsed_output.get("chosen_research_task"):
            unique_tasks.append(run.parsed_output["chosen_research_task"])
    artifact_ids = [hit["artifact"]["artifact_id"] for hit in artifact_results.get("results", [])]
    agent3 = agent_runs[-1].parsed_output or {}
    return {
        "paper_title": paper["title"],
        "artifact_count": len(artifact_results.get("results", [])),
        "task_count": len(task_histories),
        "agent_outputs_ok": all(run.returncode == 0 and run.parsed_output for run in agent_runs),
        "unique_task_count": len(set(unique_tasks)),
        "artifact_ids": artifact_ids,
        "claude_complementary_to": next(
            (
                run.parsed_output.get("complementary_to", [])
                for run in agent_runs
                if run.label.startswith("agent2_") and run.parsed_output
            ),
            [],
        ),
        "agent3_complementary_to": agent3.get("complementary_to", []),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--paper-url", default=DEFAULT_URL)
    parser.add_argument("--tenant-id", default=DEFAULT_TENANT)
    parser.add_argument("--project-id", default=DEFAULT_PROJECT)
    parser.add_argument("--challenge-id", default=DEFAULT_CHALLENGE)
    parser.add_argument("--output-dir", type=Path, default=None)
    parser.add_argument("--http-bind", default="127.0.0.1:8787")
    args = parser.parse_args()

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output_dir = (
        args.output_dir
        if args.output_dir is not None
        else REPO_ROOT / "evals" / "results" / f"paper-artifact-eval-{timestamp}"
    )
    output_dir.mkdir(parents=True, exist_ok=True)

    paper = fetch_paper_metadata(args.paper_url)
    paper_path = output_dir / "paper_brief.md"
    write_paper_brief(paper_path, paper)

    schema_path = output_dir / "agent_output_schema.json"
    build_schema(schema_path)

    claude_mcp_config = output_dir / "claude_mcp.json"
    claude_mcp_config.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "memd": {"type": "http", "url": f"http://{args.http_bind}/mcp"}
                }
            },
            indent=2,
        ),
        encoding="utf-8",
    )

    subprocess.run(
        ["cargo", "build", "-p", "memd", "--bin", "memd"],
        cwd=REPO_ROOT,
        check=True,
    )

    memd_bin = REPO_ROOT / "target" / "debug" / "memd"
    data_dir = output_dir / "memd_data"
    memd_log = output_dir / "memd.log"
    memd_proc = subprocess.Popen(
        [
            str(memd_bin),
            "--mode",
            "mcp",
            "--transport",
            "http",
            "--http-bind",
            args.http_bind,
            "--data-dir",
            str(data_dir),
        ],
        cwd=REPO_ROOT,
        stdout=memd_log.open("w"),
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        wait_for_mcp(f"http://{args.http_bind}/mcp")

        agent_specs = [
            ("agent1_codex", "codex_reader_1", False, "codex"),
            ("agent2_claude", "claude_reader_2", False, "claude"),
            ("agent3_codex", "codex_reader_3", True, "codex"),
        ]

        runs: list[AgentRun] = []
        for idx, (agent_label, agent_id, complementary, cli_name) in enumerate(agent_specs, start=1):
            session_id = str(uuid.uuid4())
            prompt = build_prompt(
                agent_label=agent_label,
                paper_path=paper_path,
                paper=paper,
                tenant_id=args.tenant_id,
                project_id=args.project_id,
                challenge_id=args.challenge_id,
                agent_id=agent_id,
                session_id=session_id,
                complementary=complementary,
            )
            prompt_path = output_dir / f"{agent_label}_prompt.txt"
            prompt_path.write_text(prompt, encoding="utf-8")
            output_path = output_dir / f"{agent_label}_output.json"

            if cli_name == "codex":
                run = run_codex(
                    agent_label=agent_label,
                    prompt=prompt,
                    schema_path=schema_path,
                    output_path=output_path,
                    mcp_url=f"http://{args.http_bind}/mcp",
                )
            else:
                run = run_claude(
                    agent_label=agent_label,
                    prompt=prompt,
                    schema_path=schema_path,
                    output_path=output_path,
                    mcp_config_path=claude_mcp_config,
                )
            run.prompt_path = prompt_path
            if run.returncode != 0 or run.parsed_output is None:
                raise RuntimeError(
                    f"{agent_label} failed to produce structured output; see {output_path}"
                )
            runs.append(run)

        artifact_results = mcp_tool_call(
            f"http://{args.http_bind}/mcp",
            "artifact.search",
            {
                "tenant_id": args.tenant_id,
                "query": "",
                "k": 20,
                "filters": {"challenge_id": args.challenge_id, "project_id": args.project_id},
            },
            100,
        )

        task_histories: dict[str, Any] = {}
        for run in runs:
            if not run.parsed_output:
                continue
            task_id = run.parsed_output.get("task_id")
            if not task_id:
                continue
            task_histories[task_id] = mcp_tool_call(
                f"http://{args.http_bind}/mcp",
                "task.get",
                {"tenant_id": args.tenant_id, "task_id": task_id},
                101 + len(task_histories),
            )

        benchmark = {
            "paper": paper,
            "tenant_id": args.tenant_id,
            "project_id": args.project_id,
            "challenge_id": args.challenge_id,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "agents": [
                {
                    "label": run.label,
                    "cli": run.cli,
                    "prompt_path": str(run.prompt_path),
                    "output_path": str(run.stdout_path),
                    "returncode": run.returncode,
                    "duration_s": round(run.duration_s, 2),
                    "parsed_output": run.parsed_output,
                }
                for run in runs
            ],
            "artifact_search": artifact_results,
            "task_histories": task_histories,
        }
        benchmark["summary"] = summarize_benchmark(
            paper=paper,
            artifact_results=artifact_results,
            task_histories=task_histories,
            agent_runs=runs,
        )

        summary_md = output_dir / "benchmark_summary_complete.md"
        summary_md.write_text(
            textwrap.dedent(
                f"""
                # Paper Artifact Benchmark

                Paper: {paper['title']}
                Challenge: {args.challenge_id}
                Tenant: {args.tenant_id}

                ## Agent Outputs
                {json.dumps(benchmark['agents'], indent=2)}

                ## Summary
                {json.dumps(benchmark['summary'], indent=2)}
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )

        (output_dir / "benchmark_output_complete.json").write_text(
            json.dumps(benchmark, indent=2),
            encoding="utf-8",
        )
        print(json.dumps({"output_dir": str(output_dir), "summary": benchmark["summary"]}, indent=2))
        return 0
    finally:
        if memd_proc.poll() is None:
            memd_proc.send_signal(signal.SIGTERM)
            try:
                memd_proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                memd_proc.kill()


if __name__ == "__main__":
    sys.exit(main())
