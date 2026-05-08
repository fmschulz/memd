#!/usr/bin/env python3
"""Estimate with-memd vs without-memd token overhead from benchmark transcripts.

This script reports two different signals:

1. Exact total tokens when an agent transcript includes a `tokens used` footer.
2. A rough transcript-size estimate for every paired run, using
   `ceil(transcript_bytes / 4)`.

The exact footer is the preferred whole-agent signal. The transcript estimate is
only a visible-output proxy and should not be treated as provider billing usage.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import re
from dataclasses import dataclass
from statistics import mean, median


ROOT = pathlib.Path(__file__).resolve().parents[3]
DEFAULT_RUN_DIRS = [
    ROOT / "evals/bench/v2-xproject/results/runs",
    ROOT / "evals/bench/v2-xproject/results/runs_sweep2_partial",
    ROOT / "evals/bench/memd-xproject-pilot/runs",
]
TOKEN_FOOTER = re.compile(r"^tokens used\s*\n\s*([0-9][0-9,]*)", re.MULTILINE)


@dataclass(frozen=True)
class RunRecord:
    suite: str
    run_set: str
    agent: str
    condition: str
    qid: str
    project: str
    path: pathlib.Path
    exact_tokens: int | None
    estimated_transcript_tokens: int


def load_v2_projects() -> dict[str, str]:
    prompts_path = ROOT / "evals/bench/v2-xproject/questions/prompts.json"
    if not prompts_path.exists():
        return {}
    prompts = json.loads(prompts_path.read_text())
    out = {}
    for item in prompts.get("questions", []):
        cwd = pathlib.PurePosixPath(item.get("cwd", "unknown"))
        out[item["id"]] = cwd.name
    return out


def parse_record(path: pathlib.Path, v2_projects: dict[str, str]) -> RunRecord | None:
    parts = path.stem.split("__")
    if len(parts) < 3:
        return None
    agent, condition = parts[0], parts[1]
    if condition not in {"with", "without"}:
        return None

    rel = path.relative_to(ROOT)
    text = path.read_text(errors="replace")
    matches = list(TOKEN_FOOTER.finditer(text))
    exact_tokens = None
    if matches:
        exact_tokens = int(matches[-1].group(1).replace(",", ""))

    if "v2-xproject" in rel.parts:
        suite = "v2-xproject"
        qid = parts[2]
        project = v2_projects.get(qid, "unknown")
    elif "memd-xproject-pilot" in rel.parts:
        suite = "memd-xproject-pilot"
        qid = parts[-1]
        project = "__".join(parts[2:-1]) or "unknown"
    else:
        suite = rel.parts[2] if len(rel.parts) > 2 else "unknown"
        qid = parts[-1]
        project = "unknown"

    return RunRecord(
        suite=suite,
        run_set=path.parent.name,
        agent=agent,
        condition=condition,
        qid=qid,
        project=project,
        path=path,
        exact_tokens=exact_tokens,
        estimated_transcript_tokens=math.ceil(len(text.encode("utf-8")) / 4),
    )


def collect(paths: list[pathlib.Path]) -> list[RunRecord]:
    v2_projects = load_v2_projects()
    records: list[RunRecord] = []
    for run_dir in paths:
        if not run_dir.exists():
            continue
        for path in sorted(run_dir.glob("*.txt")):
            rec = parse_record(path, v2_projects)
            if rec:
                records.append(rec)
    return records


def paired_rows(records: list[RunRecord], field: str) -> list[tuple[tuple[str, ...], int, int]]:
    by_key: dict[tuple[str, ...], dict[str, RunRecord]] = {}
    for rec in records:
        key = (rec.suite, rec.run_set, rec.agent, rec.project, rec.qid)
        by_key.setdefault(key, {})[rec.condition] = rec

    rows = []
    for key, pair in sorted(by_key.items()):
        if "with" not in pair or "without" not in pair:
            continue
        with_value = getattr(pair["with"], field)
        without_value = getattr(pair["without"], field)
        if with_value is None or without_value is None:
            continue
        rows.append((key, int(with_value), int(without_value)))
    return rows


def print_rows(title: str, rows: list[tuple[tuple[str, ...], int, int]]) -> None:
    print(f"\n## {title}\n")
    if not rows:
        print("No paired rows.")
        return
    print("| suite | run_set | agent | project | qid | with | without | delta | delta_pct |")
    print("|---|---|---|---|---:|---:|---:|---:|---:|")
    deltas = []
    for key, with_value, without_value in rows:
        delta = with_value - without_value
        deltas.append(delta)
        pct = 100 * delta / without_value if without_value else 0
        suite, run_set, agent, project, qid = key
        print(
            f"| {suite} | {run_set} | {agent} | {project} | {qid} | "
            f"{with_value} | {without_value} | {delta:+} | {pct:+.1f}% |"
        )
    print("")
    print(
        f"Pairs: {len(rows)}; mean delta: {mean(deltas):+.0f}; "
        f"median delta: {median(deltas):+.0f}"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "run_dirs",
        nargs="*",
        type=pathlib.Path,
        default=DEFAULT_RUN_DIRS,
        help="Benchmark run directories containing *.txt transcripts.",
    )
    args = parser.parse_args()

    records = collect(args.run_dirs)
    print("# memd token overhead report")
    print(f"\nTranscripts parsed: {len(records)}")
    print_rows("Exact footer tokens", paired_rows(records, "exact_tokens"))
    print_rows(
        "Estimated transcript tokens",
        paired_rows(records, "estimated_transcript_tokens"),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
