#!/usr/bin/env python3
"""Generate the benchmark figure notebooks from cell sources.

We author the notebooks here (as ordered cell-source lists) and assemble real
`.ipynb` files with nbformat, so the notebooks stay diff-friendly and the
prose/plot logic lives in normal Python instead of hand-edited JSON. Run:

    uv run --with nbformat python evals/notebooks/build_notebooks.py

then execute with `run_notebooks.sh`. Re-running is idempotent.
"""

from __future__ import annotations

from pathlib import Path

import nbformat as nbf

HERE = Path(__file__).resolve().parent


def md(*lines: str) -> nbf.NotebookNode:
    return nbf.v4.new_markdown_cell("\n".join(lines))


def code(*lines: str) -> nbf.NotebookNode:
    return nbf.v4.new_code_cell("\n".join(lines))


def write(name: str, cells: list[nbf.NotebookNode]) -> None:
    nb = nbf.v4.new_notebook()
    nb.cells = cells
    nb.metadata = {
        "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
        "language_info": {"name": "python"},
    }
    out = HERE / name
    nbf.write(nb, out)
    print("wrote", out)


# ---------------------------------------------------------------------------
# Notebook 1: LoCoMo cross-system retrieval
# ---------------------------------------------------------------------------

locomo_cells = [
    md(
        "# LoCoMo cross-system retrieval",
        "",
        "Headline benchmark: direct retrieval on upstream `locomo10.json` (10",
        "conversations, 5,882 facts, 1,536 questions, top-k = 10). Each system is",
        "seeded with the same conversation turns and scored against LoCoMo evidence",
        "IDs. memd is compared against `mem0` and `SuperLocalMemory`.",
        "",
        "Data: `evals/notebooks/data/locomo_2026-05-22.json` (summary slice of the",
        "checked-in `evals/benchmarks/locomo/results/` report). Figures are written",
        "to `docs/figures/` for the benchmark writeup.",
    ),
    code(
        "import sys, pathlib",
        "sys.path.insert(0, str(pathlib.Path.cwd()))",
        "import matplotlib.pyplot as plt",
        "import numpy as np",
        "from memd_plotting import (apply_house_style, color_for, label_bars,",
        "                            load_json, locomo_systems, figures_dir)",
        "apply_house_style()",
        "FIG = figures_dir()",
        "report = load_json('data/locomo_2026-05-22.json')",
        "systems = locomo_systems(report)",
        "order = ['memd', 'mem0', 'superlocalmemory']",
        "systems.sort(key=lambda s: order.index(s['name']) if s['name'] in order else 99)",
        "[(s['display'], round(s['mrr_at_10'], 3)) for s in systems]",
    ),
    md(
        "## Figure 1 — retrieval quality (MRR@10 and Hit@k)",
        "",
        "Grouped horizontal bars: the single quality summary readers care about.",
        "memd leads on every metric.",
    ),
    code(
        "metrics = [('mrr_at_10', 'MRR@10'), ('hit1', 'Hit@1'), ('hit3', 'Hit@3'), ('hit10', 'Hit@10')]",
        "names = [s['display'] for s in systems]",
        "y = np.arange(len(metrics))",
        "h = 0.78 / len(systems)",
        "fig, ax = plt.subplots(figsize=(8.2, 4.6))",
        "for i, s in enumerate(systems):",
        "    vals = [s[k] for k, _ in metrics]",
        "    offset = (i - (len(systems) - 1) / 2) * h",
        "    bars = ax.barh(y + offset, vals, height=h, color=color_for(s['name']),",
        "                   label=s['display'], edgecolor='white', linewidth=0.6)",
        "    for bar, v in zip(bars, vals):",
        "        ax.text(bar.get_width() + 0.006, bar.get_y() + bar.get_height()/2,",
        "                f'{v:.3f}', va='center', ha='left', fontsize=8, color='#333333')",
        "ax.set_yticks(y)",
        "ax.set_yticklabels([lbl for _, lbl in metrics])",
        "ax.invert_yaxis()",
        "ax.set_xlim(0, 0.78)",
        "ax.set_xlabel('score (higher is better)')",
        "ax.set_title('LoCoMo retrieval quality by system')",
        "# Anchor the legend in the empty upper-right (MRR@10/Hit@1 rows top out",
        "# near 0.42), clear of the long Hit@10 bars at the bottom.",
        "ax.legend(loc='upper right', ncol=1, fontsize=9, borderaxespad=0.6)",
        "ax.grid(axis='y', visible=False)",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'locomo_quality.png')",
        "fig.savefig(FIG / 'locomo_quality.svg')",
        "plt.show()",
    ),
    md(
        "## Figure 2 — quality vs. search latency",
        "",
        "The other axis that matters operationally: memd is both the most accurate",
        "and among the fastest to query. Bubble area is proportional to p95 search",
        "latency; the x-axis is mean search latency (log scale — SuperLocalMemory is",
        "an order of magnitude slower).",
    ),
    code(
        "fig, ax = plt.subplots(figsize=(7.6, 4.8))",
        "for s in systems:",
        "    ax.scatter(s['avg_search_ms'], s['mrr_at_10'],",
        "               s=max(s['p95_search_ms'], 1) * 1.2 + 60,",
        "               color=color_for(s['name']), alpha=0.85, edgecolor='white', linewidth=1.2,",
        "               zorder=3)",
        "    ax.annotate(f\"{s['display']}\\nMRR {s['mrr_at_10']:.3f}, p95 {s['p95_search_ms']:.0f}ms\",",
        "                (s['avg_search_ms'], s['mrr_at_10']),",
        "                textcoords='offset points', xytext=(12, 6), fontsize=9)",
        "ax.set_xscale('log')",
        "ax.set_xlabel('mean search latency (ms, log scale — lower is better)')",
        "ax.set_ylabel('MRR@10 (higher is better)')",
        "ax.set_title('LoCoMo: retrieval quality vs. query latency')",
        "ax.set_ylim(0.33, 0.45)",
        "ax.margins(x=0.18)",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'locomo_quality_latency.png')",
        "fig.savefig(FIG / 'locomo_quality_latency.svg')",
        "plt.show()",
    ),
    md(
        "## Figure 3 — per-category MRR@10 heatmap",
        "",
        "LoCoMo's four question categories stress different memory behaviours.",
        "memd is strongest on category 2 (temporal/multi-hop) and competitive",
        "everywhere; the heatmap shows where each system's advantage lies.",
    ),
    code(
        "cats = sorted({c for s in systems for c in s['per_category']}, key=int)",
        "matrix = np.array([[s['per_category'].get(c, {}).get('mrr_at_10', np.nan) for c in cats]",
        "                   for s in systems])",
        "fig, ax = plt.subplots(figsize=(6.8, 3.6))",
        "im = ax.imshow(matrix, cmap='BuPu', aspect='auto', vmin=0.2, vmax=0.55)",
        "ax.set_xticks(range(len(cats)))",
        "ax.set_xticklabels([f'Cat {c}' for c in cats])",
        "ax.set_yticks(range(len(systems)))",
        "ax.set_yticklabels([s['display'] for s in systems])",
        "for i in range(matrix.shape[0]):",
        "    for j in range(matrix.shape[1]):",
        "        v = matrix[i, j]",
        "        ax.text(j, i, f'{v:.3f}', ha='center', va='center', fontsize=9,",
        "                color='white' if v > 0.42 else '#222222')",
        "ax.set_title('LoCoMo MRR@10 by question category')",
        "cbar = fig.colorbar(im, ax=ax, fraction=0.046, pad=0.04)",
        "cbar.set_label('MRR@10')",
        "ax.grid(False)",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'locomo_per_category.png')",
        "fig.savefig(FIG / 'locomo_per_category.svg')",
        "plt.show()",
    ),
]

# ---------------------------------------------------------------------------
# Notebook 2: BEIR offline retrieval + regression gate
# ---------------------------------------------------------------------------

beir_cells = [
    md(
        "# BEIR offline retrieval gate",
        "",
        "Internal regression gate: hybrid retrieval (all-MiniLM dense + BM25 sparse,",
        "feature reranker) on BEIR FiQA and SciDocs, capped at 30 queries / 500",
        "documents per dataset, seed 42, 1,000 bootstrap iterations — the exact",
        "parameters the CI `retrieval-gate` workflow uses. This is what protects the",
        "current code against retrieval regressions on every PR.",
        "",
        "Data: `evals/notebooks/data/beir_cross_corpus_2026-06-11.json` (current code)",
        "and `beir_regression_2026-06-11.json` (paired-query gate vs. the checked-in",
        "`beir_v1.json` baseline).",
    ),
    code(
        "import sys, pathlib",
        "sys.path.insert(0, str(pathlib.Path.cwd()))",
        "import matplotlib.pyplot as plt",
        "import numpy as np",
        "from memd_plotting import (apply_house_style, color_for, load_json,",
        "                            beir_datasets, normalized_summary, figures_dir)",
        "apply_house_style()",
        "FIG = figures_dir()",
        "report = load_json('data/beir_cross_corpus_2026-06-11.json')",
        "datasets = beir_datasets(report)",
        "norm = normalized_summary(report)",
        "regression = load_json('data/beir_regression_2026-06-11.json')",
        "[(d['name'], d['ndcg_at_k'].get('10')) for d in datasets]",
    ),
    md(
        "## Figure 1 — nDCG@k retrieval curves",
        "",
        "nDCG at increasing cutoffs per dataset, plus the macro-averaged",
        "cross-corpus curve. FiQA (financial QA) retrieves cleanly; SciDocs",
        "(citation-style, high semantic drift) is the harder corpus.",
    ),
    code(
        "ks = [1, 5, 10, 100]",
        "fig, ax = plt.subplots(figsize=(7.8, 4.8))",
        "for d in datasets:",
        "    ys = [d['ndcg_at_k'].get(str(k)) for k in ks]",
        "    ax.plot(ks, ys, marker='o', linewidth=2, color=color_for(d['name']),",
        "            label=d['name'])",
        "    for k, y in zip(ks, ys):",
        "        if y is not None:",
        "            ax.annotate(f'{y:.3f}', (k, y), textcoords='offset points',",
        "                        xytext=(0, 8), fontsize=8, ha='center', color='#333333')",
        "norm_ndcg = norm.get('ndcg_at_k', {})",
        "if norm_ndcg:",
        "    ys = [norm_ndcg.get(str(k)) for k in ks]",
        "    ax.plot(ks, ys, marker='s', linewidth=2.4, linestyle='--',",
        "            color=color_for('cross-corpus'), label='cross-corpus (macro avg)')",
        "ax.set_xscale('log')",
        "ax.set_xticks(ks)",
        "ax.set_xticklabels([str(k) for k in ks])",
        "ax.set_xlabel('cutoff k')",
        "ax.set_ylabel('nDCG@k (higher is better)')",
        "ax.set_ylim(0, 0.75)",
        "ax.set_title('BEIR retrieval: nDCG@k by dataset')",
        "ax.legend(loc='upper left', fontsize=9)",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'beir_ndcg_curves.png')",
        "fig.savefig(FIG / 'beir_ndcg_curves.svg')",
        "plt.show()",
    ),
    md(
        "## Figure 2 — recall / MRR / precision with 95% CIs",
        "",
        "The headline metrics per dataset, with bootstrap 95% confidence intervals.",
        "The wide CIs reflect the 30-query cap — this gate is a fast regression",
        "tripwire, not a precise leaderboard.",
    ),
    code(
        "metrics = [('recall', 'Recall@10'), ('mrr', 'MRR'), ('precision', 'P@10')]",
        "fig, axes = plt.subplots(1, 3, figsize=(11, 4), sharey=False)",
        "for ax, (key, title) in zip(axes, metrics):",
        "    names = [d['name'] for d in datasets]",
        "    means = [d[key]['mean'] for d in datasets]",
        "    los = [d[key]['mean'] - d[key]['ci_lower'] for d in datasets]",
        "    his = [d[key]['ci_upper'] - d[key]['mean'] for d in datasets]",
        "    colors = [color_for(n) for n in names]",
        "    bars = ax.bar(range(len(datasets)), means, color=colors, width=0.6,",
        "                  yerr=[los, his], capsize=5, edgecolor='white',",
        "                  error_kw={'ecolor': '#555555', 'lw': 1.2})",
        "    ax.set_xticks(range(len(datasets)))",
        "    ax.set_xticklabels(names, rotation=12, ha='right', fontsize=9)",
        "    ax.set_title(title)",
        "    ax.set_ylim(0, 1.05)",
        "    ax.grid(axis='x', visible=False)",
        "    for bar, m in zip(bars, means):",
        "        ax.text(bar.get_x() + bar.get_width()/2, 0.02, f'{m:.3f}',",
        "                ha='center', va='bottom', fontsize=9, color='#222222')",
        "fig.suptitle('BEIR metrics with 95% bootstrap CIs')",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'beir_metrics_ci.png')",
        "fig.savefig(FIG / 'beir_metrics_ci.svg')",
        "plt.show()",
    ),
    md(
        "## Figure 3 — regression gate: baseline vs. current code",
        "",
        "The paired-query nDCG@10 gate. The current code (after the v0.60/0.61",
        "quality work) clears the checked-in baseline with a statistically",
        "significant improvement — wins outnumber losses across the paired queries.",
    ),
    code(
        "m = regression['metrics'][0]",
        "fig, (axL, axR) = plt.subplots(1, 2, figsize=(10.5, 4.2),",
        "                               gridspec_kw={'width_ratios': [1, 1.1]})",
        "# left: baseline vs candidate mean nDCG@10",
        "labels = ['baseline\\n(beir_v1)', 'current code']",
        "means = [m['baseline_mean'], m['candidate_mean']]",
        "colors = [color_for('baseline'), color_for('candidate')]",
        "bars = axL.bar(labels, means, color=colors, width=0.6, edgecolor='white')",
        "for bar, v in zip(bars, means):",
        "    axL.text(bar.get_x() + bar.get_width()/2, v + 0.008, f'{v:.3f}',",
        "             ha='center', va='bottom', fontsize=11, fontweight='bold')",
        "axL.set_ylabel('mean nDCG@10')",
        "axL.set_ylim(0, max(means) * 1.25)",
        "axL.set_title('Paired-query nDCG@10')",
        "axL.grid(axis='x', visible=False)",
        "delta = m['candidate_mean'] - m['baseline_mean']",
        "axL.annotate(f'Δ +{delta:.3f}\\np = {m[\"p_value\"]:.4f}\\neffect {m[\"effect_size\"]:.2f}',",
        "             xy=(1, m['candidate_mean']), xytext=(0.45, max(means) * 1.05),",
        "             fontsize=9, color=color_for('accent'), fontweight='bold')",
        "# right: win / loss / tie breakdown",
        "wlt = [m['wins'], m['losses'], m['ties']]",
        "wlt_labels = [f\"wins ({m['wins']})\", f\"losses ({m['losses']})\", f\"ties ({m['ties']})\"]",
        "wlt_colors = [color_for('memd'), color_for('accent'), '#BBBBBB']",
        "axR.barh(range(3), wlt, color=wlt_colors, edgecolor='white')",
        "axR.set_yticks(range(3))",
        "axR.set_yticklabels(wlt_labels)",
        "axR.invert_yaxis()",
        "axR.set_xlabel(f\"paired queries (n = {m['n_pairs']})\")",
        "axR.set_title('Per-query outcome vs. baseline')",
        "axR.grid(axis='y', visible=False)",
        "for i, v in enumerate(wlt):",
        "    axR.text(v + 0.4, i, str(v), va='center', fontsize=10)",
        "verdict = 'PASS' if regression.get('overall_passed') else 'FAIL'",
        "fig.suptitle(f'BEIR regression gate — {verdict}', color=color_for('memd'))",
        "fig.tight_layout()",
        "fig.savefig(FIG / 'beir_regression_gate.png')",
        "fig.savefig(FIG / 'beir_regression_gate.svg')",
        "plt.show()",
    ),
]

write("locomo_cross_system.ipynb", locomo_cells)
write("beir_retrieval_gate.ipynb", beir_cells)
print("done")
