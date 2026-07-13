"""Shared plotting style and data loaders for the memd benchmark figures.

Imported by the benchmark notebooks in this directory. Keeping the style and
the JSON readers here means every figure shares one visual language and the
notebooks stay about *what* to plot, not *how* to format axes.

Design choices (publication-oriented, colour-blind safe):
- A small, fixed palette keyed by system/dataset name so the same entity is
  the same colour across every figure.
- Horizontal bars with value labels for ranked comparisons (easy to read,
  no rotated tick labels).
- 95% bootstrap confidence intervals drawn as error bars wherever the
  benchmark reports them.
- Muted gridlines, no top/right spines, generous whitespace.
"""

from __future__ import annotations

import json
from pathlib import Path

import matplotlib as mpl

# --- house style -----------------------------------------------------------

# Okabe-Ito colour-blind-safe palette, assigned to recurring entities.
PALETTE = {
    "BEIR FiQA": "#0072B2",
    "BEIR SciDocs": "#56B4E9",
    "cross-corpus": "#CC79A7",
    "baseline": "#999999",
    "candidate": "#0072B2",
    "accent": "#D55E00",
}


def apply_house_style() -> None:
    """Set rcParams once per notebook for a consistent, clean look."""
    mpl.rcParams.update(
        {
            "figure.dpi": 130,
            "savefig.dpi": 200,
            "savefig.bbox": "tight",
            "figure.facecolor": "white",
            "axes.facecolor": "white",
            "font.family": "sans-serif",
            "font.sans-serif": ["Helvetica", "Arial", "DejaVu Sans"],
            "font.size": 11,
            "axes.titlesize": 13,
            "axes.titleweight": "bold",
            "axes.labelsize": 11,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "axes.grid": True,
            "axes.axisbelow": True,
            "grid.color": "#E6E6E6",
            "grid.linewidth": 0.8,
            "xtick.color": "#444444",
            "ytick.color": "#444444",
            "axes.edgecolor": "#999999",
            "legend.frameon": False,
            "figure.titlesize": 14,
            "figure.titleweight": "bold",
        }
    )


def color_for(name: str, default: str = "#0072B2") -> str:
    return PALETTE.get(name, default)


# --- data loaders ----------------------------------------------------------


def load_json(path: str | Path) -> dict:
    with open(path) as fh:
        return json.load(fh)


def beir_datasets(report: dict) -> list[dict]:
    """Normalize a cross-corpus BEIR report into per-dataset metric dicts.

    Each entry: name, queries, recall/mrr/precision/latency (mean+CI),
    ndcg_at_k (dict).
    """
    out = []
    for ds in report.get("datasets", []):
        summary = ds.get("summary", {})

        def metric(key):
            m = summary.get(key, {})
            return {
                "mean": m.get("mean"),
                "ci_lower": m.get("ci_lower"),
                "ci_upper": m.get("ci_upper"),
            }

        out.append(
            {
                "name": ds.get("dataset_description", ds.get("dataset_path", "?")).split(":")[0],
                "path": ds.get("dataset_path"),
                "queries": ds.get("queries_evaluated"),
                "documents": ds.get("documents_indexed"),
                "recall": metric("recall"),
                "mrr": metric("mrr"),
                "precision": metric("precision"),
                "latency_ms": metric("latency_ms"),
                "ndcg_at_k": summary.get("ndcg_at_k", {}),
            }
        )
    return out


def normalized_summary(report: dict) -> dict:
    """Macro-averaged cross-corpus summary block from a BEIR report."""
    return report.get("normalized_summary", {})


def figures_dir() -> Path:
    """docs/figures, resolved relative to this file (evals/notebooks/)."""
    here = Path(__file__).resolve()
    repo_root = here.parents[2]
    out = repo_root / "docs" / "figures"
    out.mkdir(parents=True, exist_ok=True)
    return out
