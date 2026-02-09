#!/usr/bin/env python3
"""
Analyze benchmark protocol JSON reports and generate a comparison summary.
"""

from __future__ import annotations

import json
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Tuple


@dataclass
class QueryMetric:
    query_id: str
    recall_at_10: float
    mrr: float
    precision_at_10: float


@dataclass
class ReportView:
    path: Path
    dataset: str
    model: str
    recall_mean: float
    recall_ci: Tuple[float, float]
    mrr_mean: float
    mrr_ci: Tuple[float, float]
    precision_mean: float
    precision_ci: Tuple[float, float]
    queries: List[QueryMetric]
    gate_passed: bool
    gate_message: str


def load_reports(results_dir: Path) -> List[ReportView]:
    reports: List[ReportView] = []
    for path in sorted(results_dir.glob("*.json")):
        with path.open("r", encoding="utf-8") as f:
            data = json.load(f)

        if "summary" not in data or "query_metrics" not in data:
            continue

        summary = data["summary"]
        queries = [
            QueryMetric(
                query_id=q["query_id"],
                recall_at_10=float(q["recall_at_10"]),
                mrr=float(q["mrr"]),
                precision_at_10=float(q["precision_at_10"]),
            )
            for q in data.get("query_metrics", [])
        ]
        reports.append(
            ReportView(
                path=path,
                dataset=data.get("dataset_description", path.stem),
                model=data.get("embedding_model", "unknown"),
                recall_mean=float(summary["recall"]["mean"]),
                recall_ci=(
                    float(summary["recall"]["ci_lower"]),
                    float(summary["recall"]["ci_upper"]),
                ),
                mrr_mean=float(summary["mrr"]["mean"]),
                mrr_ci=(
                    float(summary["mrr"]["ci_lower"]),
                    float(summary["mrr"]["ci_upper"]),
                ),
                precision_mean=float(summary["precision"]["mean"]),
                precision_ci=(
                    float(summary["precision"]["ci_lower"]),
                    float(summary["precision"]["ci_upper"]),
                ),
                queries=queries,
                gate_passed=bool(data.get("quality_gate_passed", False)),
                gate_message=data.get("quality_gate_message", ""),
            )
        )
    return reports


def paired_delta(a: ReportView, b: ReportView) -> Dict[str, float]:
    a_map = {q.query_id: q for q in a.queries}
    b_map = {q.query_id: q for q in b.queries}
    common = sorted(set(a_map.keys()) & set(b_map.keys()))
    if not common:
        return {
            "n": 0,
            "recall_delta": 0.0,
            "mrr_delta": 0.0,
            "precision_delta": 0.0,
            "wins": 0,
            "losses": 0,
            "ties": 0,
        }

    recall_diffs = [b_map[q].recall_at_10 - a_map[q].recall_at_10 for q in common]
    mrr_diffs = [b_map[q].mrr - a_map[q].mrr for q in common]
    precision_diffs = [b_map[q].precision_at_10 - a_map[q].precision_at_10 for q in common]

    wins = sum(1 for x in recall_diffs if x > 0)
    losses = sum(1 for x in recall_diffs if x < 0)
    ties = sum(1 for x in recall_diffs if x == 0)

    return {
        "n": len(common),
        "recall_delta": statistics.mean(recall_diffs),
        "mrr_delta": statistics.mean(mrr_diffs),
        "precision_delta": statistics.mean(precision_diffs),
        "wins": wins,
        "losses": losses,
        "ties": ties,
    }


def generate_markdown(reports: List[ReportView], output_path: Path) -> None:
    grouped: Dict[str, List[ReportView]] = {}
    for report in reports:
        grouped.setdefault(report.dataset, []).append(report)

    lines: List[str] = []
    lines.append("# Statistical Analysis of Benchmark Protocol Reports")
    lines.append("")
    lines.append("## Aggregate Results")
    lines.append("")
    lines.append("| Dataset | Model | Recall@10 (95% CI) | MRR (95% CI) | P@10 (95% CI) | Gate |")
    lines.append("|---|---|---|---|---|---|")

    for dataset in sorted(grouped.keys()):
        for report in sorted(grouped[dataset], key=lambda r: r.model):
            gate = "PASS" if report.gate_passed else "FAIL"
            lines.append(
                "| {} | {} | {:.3} [{:.3}, {:.3}] | {:.3} [{:.3}, {:.3}] | {:.3} [{:.3}, {:.3}] | {} |".format(
                    dataset,
                    report.model,
                    report.recall_mean,
                    report.recall_ci[0],
                    report.recall_ci[1],
                    report.mrr_mean,
                    report.mrr_ci[0],
                    report.mrr_ci[1],
                    report.precision_mean,
                    report.precision_ci[0],
                    report.precision_ci[1],
                    gate,
                )
            )

    lines.append("")
    lines.append("## Paired Comparisons")
    lines.append("")

    had_comparison = False
    for dataset in sorted(grouped.keys()):
        reports_for_dataset = grouped[dataset]
        if len(reports_for_dataset) < 2:
            continue
        base = sorted(reports_for_dataset, key=lambda r: r.model)[0]
        for contender in sorted(reports_for_dataset, key=lambda r: r.model)[1:]:
            delta = paired_delta(base, contender)
            had_comparison = True
            lines.append(f"### {dataset}: {base.model} -> {contender.model}")
            lines.append("")
            lines.append(f"- Paired queries: {delta['n']}")
            lines.append(f"- Recall@10 mean delta: {delta['recall_delta']:.4f}")
            lines.append(f"- MRR mean delta: {delta['mrr_delta']:.4f}")
            lines.append(f"- P@10 mean delta: {delta['precision_delta']:.4f}")
            lines.append(
                f"- Recall wins/losses/ties: {int(delta['wins'])}/{int(delta['losses'])}/{int(delta['ties'])}"
            )
            lines.append("")

    if not had_comparison:
        lines.append("Only one model report per dataset was found, so paired comparisons were skipped.")
        lines.append("")

    output_path.write_text("\n".join(lines), encoding="utf-8")


def generate_json_summary(reports: List[ReportView], output_path: Path) -> None:
    payload = []
    for report in reports:
        payload.append(
            {
                "report_path": str(report.path),
                "dataset": report.dataset,
                "model": report.model,
                "recall_mean": report.recall_mean,
                "recall_ci": list(report.recall_ci),
                "mrr_mean": report.mrr_mean,
                "mrr_ci": list(report.mrr_ci),
                "precision_mean": report.precision_mean,
                "precision_ci": list(report.precision_ci),
                "gate_passed": report.gate_passed,
                "gate_message": report.gate_message,
                "query_count": len(report.queries),
            }
        )
    output_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")


def main() -> int:
    if len(sys.argv) < 2:
        print("Usage: analyze_benchmark_results.py <results_dir>", file=sys.stderr)
        return 1

    results_dir = Path(sys.argv[1])
    if not results_dir.exists():
        print(f"Error: Directory not found: {results_dir}", file=sys.stderr)
        return 1

    reports = load_reports(results_dir)
    if not reports:
        print("No benchmark protocol JSON reports found.", file=sys.stderr)
        return 1

    md_path = results_dir / "statistical_analysis.md"
    json_path = results_dir / "statistical_analysis.json"
    generate_markdown(reports, md_path)
    generate_json_summary(reports, json_path)

    print(f"Wrote markdown summary: {md_path}")
    print(f"Wrote json summary: {json_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
