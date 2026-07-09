"""Hand-laid memd architecture figure.

Run: ``python3 docs/figures/architecture.py``
Outputs ``docs/figures/architecture.svg`` and ``docs/figures/architecture.png``.

The figure shows five layers — clients, CLI surface, hybrid retrieval,
persistent store, on-disk layout — with explicit colour coding for the
two dispatch paths (teal = retrieval, orange = store). It is intentionally
static so the manuscript and mkdocs site render the same image without a
Mermaid runtime.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.patches import FancyArrowPatch, FancyBboxPatch


PRIMARY = "#3949AB"        # indigo-600, matches mkdocs-material accent
PRIMARY_SOFT = "#E8EAF6"   # indigo-50
SURFACE = "#283593"        # indigo-900
SURFACE_SOFT = "#EDE7F6"
RETRIEVE = "#00695C"       # teal-800
RETRIEVE_SOFT = "#E0F2F1"
STORE = "#BF360C"          # deep-orange-900
STORE_SOFT = "#FFF3E0"
NEUTRAL = "#37474F"        # blue-grey-800
DISK_SOFT = "#ECEFF1"
GRID = "#CFD8DC"           # blue-grey-100
TEXT = "#212121"
SUBTLE = "#546E7A"
BG = "#FFFFFF"

FONT_TITLE = {"family": "DejaVu Sans", "weight": "bold", "size": 10.5}
FONT_BODY = {"family": "DejaVu Sans", "size": 8.5}
FONT_MONO = {"family": "DejaVu Sans Mono", "size": 8}
FONT_PATH = {"family": "DejaVu Sans Mono", "size": 7.2, "style": "italic"}


@dataclass
class Box:
    x: float
    y: float
    w: float
    h: float
    title: str
    body: str
    mono: bool = False
    accent: str = PRIMARY
    fill: str = PRIMARY_SOFT
    path: str = ""  # small italic mono caption pinned to box bottom

    @property
    def cx(self) -> float:
        return self.x + self.w / 2

    @property
    def cy(self) -> float:
        return self.y + self.h / 2


def draw_box(ax, box: Box) -> None:
    patch = FancyBboxPatch(
        (box.x, box.y),
        box.w,
        box.h,
        boxstyle="round,pad=0.02,rounding_size=0.16",
        linewidth=1.4,
        edgecolor=box.accent,
        facecolor=box.fill,
        zorder=2,
    )
    ax.add_patch(patch)
    title_y = box.y + box.h - 0.20
    ax.text(
        box.cx,
        title_y,
        box.title,
        ha="center",
        va="top",
        color=box.accent,
        **FONT_TITLE,
    )
    body_font = FONT_MONO if box.mono else FONT_BODY
    body_top = title_y - 0.30
    body_bot = box.y + (0.32 if box.path else 0.14)
    body_cy = (body_top + body_bot) / 2
    ax.text(
        box.cx,
        body_cy,
        box.body,
        ha="center",
        va="center",
        color=TEXT,
        **body_font,
    )
    if box.path:
        ax.text(
            box.cx,
            box.y + 0.14,
            box.path,
            ha="center",
            va="bottom",
            color=SUBTLE,
            **FONT_PATH,
        )


def draw_band(ax, x: float, y: float, w: float, h: float, label: str) -> None:
    patch = FancyBboxPatch(
        (x, y),
        w,
        h,
        boxstyle="round,pad=0.02,rounding_size=0.22",
        linewidth=1.0,
        edgecolor=GRID,
        facecolor="#FAFAFA",
        zorder=1,
    )
    ax.add_patch(patch)
    ax.text(
        x + 0.08,
        y + h + 0.04,
        label,
        ha="left",
        va="bottom",
        color=NEUTRAL,
        fontsize=9.5,
        fontweight="bold",
        family="DejaVu Sans",
    )


def arrow(ax, src: tuple[float, float], dst: tuple[float, float],
          color: str = NEUTRAL, lw: float = 1.2) -> None:
    arr = FancyArrowPatch(
        src,
        dst,
        arrowstyle="-|>",
        mutation_scale=11,
        linewidth=lw,
        color=color,
        shrinkA=2,
        shrinkB=2,
        zorder=3,
    )
    ax.add_patch(arr)


def vline(ax, x: float, y0: float, y1: float, color: str, lw: float = 1.1) -> None:
    ax.plot([x, x], [y0, y1], color=color, linewidth=lw, zorder=3)


def hline(ax, x0: float, x1: float, y: float, color: str, lw: float = 1.1) -> None:
    ax.plot([x0, x1], [y, y], color=color, linewidth=lw, zorder=3)


def build() -> plt.Figure:
    fig, ax = plt.subplots(figsize=(11.0, 8.4), dpi=150)
    ax.set_xlim(0, 14)
    ax.set_ylim(0, 11.0)
    ax.set_aspect("equal")
    ax.axis("off")
    fig.patch.set_facecolor(BG)

    # Layer band geometry --------------------------------------------
    # (y, h, label). Bands are tight; whitespace between them carries
    # the dispatch rails.
    bands = {
        "clients":   (8.55, 1.25, "Clients (same machine)"),
        "cli":       (6.45, 1.80, "CLI surface + operations"),
        "retrieval": (4.10, 2.00, "Hybrid retrieval"),
        "store":     (1.80, 1.95, "Persistent store"),
        "disk":      (0.10, 1.40, "On-disk layout under ~/.memd/data/  (tenants/<id>/ holds per-tenant state)"),
    }
    for y, h, label in bands.values():
        draw_band(ax, 0.4, y, 13.2, h, label)

    # Clients ---------------------------------------------------------
    cli_band_y = bands["clients"][0]
    clients = [
        Box(1.1, cli_band_y + 0.15, 3.4, 0.95,
            "Coding agent", "Claude Code · Codex · others"),
        Box(5.3, cli_band_y + 0.15, 3.4, 0.95,
            "AI scientist", "research workflows · custom"),
        Box(9.5, cli_band_y + 0.15, 3.4, 0.95,
            "Human / controller", "shell · scripts · CI"),
    ]
    for box in clients:
        draw_box(ax, box)

    # CLI surface + operations ----------------------------------------
    surf_band_y = bands["cli"][0]
    surface = [
        Box(
            1.1, surf_band_y + 0.20, 5.6, 1.45,
            "Entry commands",
            "memd agent-context\nmemd search · memd add\nmemd warm · memd batch · memd doctor",
            mono=True, accent=SURFACE, fill=SURFACE_SOFT,
        ),
        Box(
            7.3, surf_band_y + 0.20, 5.6, 1.45,
            "Operation surface",
            "memory · task · artifact\ncontext · code · debug\n(memd call <op> --json)",
            mono=True, accent=SURFACE, fill=SURFACE_SOFT,
        ),
    ]
    for box in surface:
        draw_box(ax, box)

    # Hybrid retrieval ------------------------------------------------
    # 5 lanes. Widths chosen so the row exactly fills the band; Rerank is
    # the widest so it can hold three lines without overflow.
    ret_band_y = bands["retrieval"][0]
    ret_y = ret_band_y + 0.18
    ret_h = 1.55
    retrieval = [
        Box(1.10, ret_y, 2.30, ret_h, "Hybrid", "fusion + rank",
            accent=RETRIEVE, fill=RETRIEVE_SOFT),
        Box(3.55, ret_y, 2.30, ret_h, "Sparse", "BM25\ntantivy",
            accent=RETRIEVE, fill=RETRIEVE_SOFT, path="→ sparse_index/"),
        Box(6.00, ret_y, 2.30, ret_h, "Dense", "HNSW\nCandle embed",
            accent=RETRIEVE, fill=RETRIEVE_SOFT, path="→ warm_index/"),
        Box(8.45, ret_y, 2.45, ret_h, "Hot + cache", "recency LRU\nquery-hash cache",
            accent=RETRIEVE, fill=RETRIEVE_SOFT),
        Box(11.05, ret_y, 2.35, ret_h, "Rerank",
            "feature (default)\nopt-in: ONNX CE,\nMemReranker-4B",
            accent=RETRIEVE, fill=RETRIEVE_SOFT),
    ]
    for box in retrieval:
        draw_box(ax, box)

    # Persistent store ------------------------------------------------
    sto_band_y = bands["store"][0]
    sto_y = sto_band_y + 0.18
    sto_h = 1.55
    store = [
        Box(1.10, sto_y, 3.05, sto_h, "SQLite", "metadata\nWAL · pooled",
            accent=STORE, fill=STORE_SOFT, path="→ metadata.db"),
        Box(4.40, sto_y, 3.05, sto_h, "Code index", "structural\ndefs · refs · calls",
            accent=STORE, fill=STORE_SOFT, path="(shares metadata.db)"),
        Box(7.70, sto_y, 2.90, sto_h, "Segments", "immutable\nappend-only payloads",
            accent=STORE, fill=STORE_SOFT, path="→ segments/"),
        Box(10.85, sto_y, 2.55, sto_h, "WAL", "fsync\nbefore commit",
            accent=STORE, fill=STORE_SOFT, path="→ wal.log"),
    ]
    for box in store:
        draw_box(ax, box)

    # On-disk layout --------------------------------------------------
    # tenants/<id>/ prefix is in the band caption; box titles stay short.
    disk_band_y = bands["disk"][0]
    disk_y = disk_band_y + 0.15
    disk_h = 1.10
    disk = [
        Box(1.10, disk_y, 2.30, disk_h, "metadata.db", "SQLite WAL",
            mono=True, accent=NEUTRAL, fill=DISK_SOFT),
        Box(3.55, disk_y, 2.30, disk_h, "sparse_index/", "tantivy",
            mono=True, accent=NEUTRAL, fill=DISK_SOFT),
        Box(6.00, disk_y, 2.30, disk_h, "warm_index/", "HNSW + mapping.bin\n(per tenant)",
            mono=True, accent=NEUTRAL, fill=DISK_SOFT),
        Box(8.45, disk_y, 2.45, disk_h, "segments/", "append-only payloads\n(per tenant)",
            mono=True, accent=NEUTRAL, fill=DISK_SOFT),
        Box(11.05, disk_y, 2.35, disk_h, "wal.log", "fsynced WAL\n(per tenant)",
            mono=True, accent=NEUTRAL, fill=DISK_SOFT),
    ]
    for box in disk:
        draw_box(ax, box)

    # ----------------------------------------------------------------
    # Dispatch rails. Each layer-to-layer transition uses elbow routing,
    # so every arrow is either horizontal or vertical — no diagonals.
    # ----------------------------------------------------------------

    # Clients → CLI surface. Each client column drops one short arrow.
    cli_top_y = surface[0].y + surface[0].h
    for src in clients:
        # Land on the nearer CLI box.
        target_cx = surface[0].cx if src.cx < 7.0 else surface[1].cx
        # Elbow: drop straight down, then horizontal into target column.
        elbow_y = (src.y + cli_top_y) / 2
        vline(ax, src.cx, src.y, elbow_y, NEUTRAL)
        hline(ax, src.cx, target_cx, elbow_y, NEUTRAL)
        arrow(ax, (target_cx, elbow_y), (target_cx, cli_top_y), NEUTRAL)

    # CLI surface → retrieval (teal trunk).
    trunk_ret_y = ret_y + ret_h + 0.20  # rail just above retrieval band
    for sb in surface:
        # Drop from each CLI box bottom to the teal trunk.
        arrow(ax, (sb.cx, sb.y), (sb.cx, trunk_ret_y), RETRIEVE)
    # Horizontal trunk spanning the retrieval band width.
    hline(ax, retrieval[0].cx, retrieval[-1].cx, trunk_ret_y, RETRIEVE, lw=1.4)
    # Drop one vertical into each lane top.
    for lane in retrieval:
        arrow(ax, (lane.cx, trunk_ret_y), (lane.cx, lane.y + lane.h), RETRIEVE)

    # CLI surface → store (orange dispatch). The store band sits below the
    # retrieval band, so a straight-down route would cross the retrieval
    # lanes. Route the orange dispatch through a thin left-margin column
    # (passthrough_x) between the figure edge and the leftmost lane.
    # Single shared stub: each CLI box drops a vertical to a shared
    # horizontal stub which feeds the passthrough column.
    trunk_sto_y = sto_y + sto_h + 0.20
    passthrough_x = 0.78
    stub_y = surface[0].y - 0.25
    for sb in surface:
        vline(ax, sb.cx, sb.y, stub_y, STORE)
    # Shared horizontal stub from passthrough to the rightmost CLI cx —
    # the leftmost CLI box's drop joins this stub along the way.
    hline(ax, passthrough_x, surface[-1].cx, stub_y, STORE, lw=1.3)
    # Passthrough vertical down past the retrieval band to the store trunk.
    vline(ax, passthrough_x, stub_y, trunk_sto_y, STORE, lw=1.3)
    # Horizontal store trunk into the band.
    hline(ax, passthrough_x, store[-1].cx, trunk_sto_y, STORE, lw=1.4)
    for sbox in store:
        arrow(ax, (sbox.cx, trunk_sto_y), (sbox.cx, sbox.y + sbox.h), STORE)

    # Store/retrieval → on-disk: no arrows. Mapping is conveyed by the
    # italic "→ path" caption inside each box and by vertical alignment
    # with the disk row directly below.

    # Heading ---------------------------------------------------------
    title_y = 10.80
    ax.text(
        0.4, title_y,
        "memd — local memory CLI for coding agents and AI scientists",
        ha="left", va="top",
        color=TEXT, fontsize=12, fontweight="bold", family="DejaVu Sans",
    )
    ax.text(
        13.6, title_y,
        "v1.3.1",
        ha="right", va="top",
        color=NEUTRAL, fontsize=10, family="DejaVu Sans Mono",
    )

    # Inline legend for the two dispatch colours, on its own row
    # between the title and the clients band so neither overlaps.
    legend_y = title_y - 0.45
    legend_x = 0.4
    ax.plot([legend_x, legend_x + 0.45], [legend_y, legend_y],
            color=RETRIEVE, linewidth=2.4)
    ax.text(legend_x + 0.55, legend_y, "retrieval dispatch",
            ha="left", va="center", color=RETRIEVE, fontsize=9, fontweight="bold",
            family="DejaVu Sans")
    ax.plot([legend_x + 2.85, legend_x + 3.30], [legend_y, legend_y],
            color=STORE, linewidth=2.4)
    ax.text(legend_x + 3.40, legend_y, "store dispatch",
            ha="left", va="center", color=STORE, fontsize=9, fontweight="bold",
            family="DejaVu Sans")

    return fig


def main() -> None:
    out_dir = Path(__file__).resolve().parent
    out_dir.mkdir(parents=True, exist_ok=True)
    fig = build()
    fig.savefig(out_dir / "architecture.svg", bbox_inches="tight", facecolor=BG)
    fig.savefig(out_dir / "architecture.png", bbox_inches="tight", facecolor=BG, dpi=200)
    print(f"wrote {out_dir / 'architecture.svg'}")
    print(f"wrote {out_dir / 'architecture.png'}")


if __name__ == "__main__":
    main()
