#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Write the documentation figures as theme-aware SVG.

Run from the repository root: ``uv run --script docs/figures/make_figures.py``.
The pages include each SVG inline, and ``docs/stylesheets/figures.css`` maps the
site's light and dark palettes to the ``--fig-*`` variables that the SVG reads.
The fallbacks keep a standalone file readable. Marks are grey; the one accent
colour marks the memd path.
"""

from dataclasses import dataclass
from pathlib import Path

OUT = Path(__file__).resolve().parent
WIDTH = 760
SANS = "var(--fig-font,Inter,system-ui,sans-serif)"
MONO = "var(--fig-mono,'JetBrains Mono',ui-monospace,monospace)"
FG = "var(--fig-fg,#1f2328)"
MUTED = "var(--fig-muted,#5b6470)"
LINE = "var(--fig-line,#c3c8cf)"
CARD = "var(--fig-card,#f4f5f7)"
ACCENT = "var(--fig-accent,#d55e00)"
WIRE = "var(--fig-wire,#8a929c)"
BG = "var(--fig-bg,#ffffff)"
RULES = {
    "bg": f"fill:{BG}",
    "card": f"fill:{CARD};stroke:{LINE};stroke-width:1",
    "hot": f"fill:{CARD};stroke:{ACCENT};stroke-width:2",
    "dash": f"fill:none;stroke:{WIRE};stroke-width:1.2;stroke-dasharray:5 4",
    "h": f"fill:{FG};font:600 16px {SANS}",
    "b": f"fill:{FG};font:400 14.5px {SANS}",
    "m": f"fill:{MUTED};font:400 13.5px {SANS}",
    "c": f"fill:{FG};font:500 13px {MONO}",
    "a": f"fill:{ACCENT}",
    "ink": f"fill:{FG}",
    "grey": f"fill:{MUTED}",
    "wire": f"fill:none;stroke:{WIRE};stroke-width:1.5",
    "path": f"fill:none;stroke:{ACCENT};stroke-width:2.2",
    "track": f"stroke:{LINE};stroke-width:1.5",
    "cross": f"fill:none;stroke:{MUTED};stroke-width:2",
    "diamond": f"fill:{BG};stroke:{ACCENT};stroke-width:2",
}
WITH_CASE = "with a memd case"
WITHOUT_CASE = "without a memd case"


@dataclass(frozen=True, slots=True)
class Session:
    """One test session: its label, its command count, and how it ended."""

    label: str
    steps: int
    solved: bool
    found_at: int | None = None
    note: str = ""


# Commands each session ran from the tool's first error to the fix, or to the
# point where it gave up, counted from the session transcripts. `found_at` is
# the command that returned the recorded case. Counts include each agent's own
# bookkeeping commands.
TESTS = {
    "Test 1 · calibration profile": {
        WITHOUT_CASE: [
            Session(
                "Codex, nothing recorded yet",
                21,
                solved=True,
                note="read another project's notes",
            ),
            Session("Codex, no memd", 16, solved=False),
        ],
        WITH_CASE: [
            Session("Codex, other project", 10, solved=True, found_at=2),
            Session("Claude Code, other project", 5, solved=True, found_at=1),
            Session("Codex, other machine", 4, solved=True, found_at=1),
        ],
    },
    "Test 2 · site endpoint": {
        WITHOUT_CASE: [
            Session("Codex, no memd", 16, solved=False),
            Session("Codex, no memd", 13, solved=False),
        ],
        WITH_CASE: [
            Session("Codex, other project", 11, solved=True, found_at=3),
            Session("Codex, service blocked", 13, solved=True, found_at=8),
        ],
    },
}


def svg(prefix: str, height: int, title: str, body: list[str]) -> str:
    """Wrap figure elements in an accessible, scalable SVG document."""
    style = "".join(f".{prefix}-{name}{{{rule}}}" for name, rule in RULES.items())
    head = (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" '
        f'role="img" aria-labelledby="{prefix}-title">'
    )
    return "\n".join(
        [
            head,
            f'<title id="{prefix}-title">{title}</title>',
            f"<style>{style}</style>",
            f'<rect class="{prefix}-bg" width="100%" height="100%"/>',
            arrow_markers(prefix),
            *body,
            "</svg>",
            "",
        ]
    )


def arrow_markers(prefix: str) -> str:
    """Define a grey and an accent arrowhead with figure-unique IDs."""
    shape = (
        'viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" '
        'orient="auto-start-reverse"'
    )
    head = '<path d="M0,1 L9,5 L0,9 z" class="{cls}"/>'
    grey = head.format(cls=f"{prefix}-grey")
    accent = head.format(cls=f"{prefix}-a")
    return (
        f'<defs><marker id="{prefix}-arrow" {shape}>{grey}</marker>'
        f'<marker id="{prefix}-arrow-a" {shape}>{accent}</marker></defs>'
    )


def text(
    p: str, kind: str, xy: tuple[float, float], value: str, anchor: str = "start"
) -> str:
    """Place one line of text in a figure style (h, b, m, c)."""
    x, y = xy
    where = f'x="{x}" y="{y}" text-anchor="{anchor}"'
    return f'<text class="{p}-{kind}" {where}>{value}</text>'


def card(p: str, box: tuple[float, float, float, float], *, hot: bool = False) -> str:
    """Draw a rounded card; a hot card carries the accent outline."""
    x, y, w, h = box
    kind = "hot" if hot else "card"
    size = f'width="{w}" height="{h}"'
    return f'<rect class="{p}-{kind}" x="{x}" y="{y}" {size} rx="10"/>'


def wire(p: str, d: str, *, accent: bool = False, dashed: bool = False) -> str:
    """Draw a connector with an arrowhead along an SVG path."""
    kind, marker = ("path", "arrow-a") if accent else ("wire", "arrow")
    dash = ' stroke-dasharray="6 5"' if dashed else ""
    return f'<path class="{p}-{kind}" d="{d}"{dash} marker-end="url(#{p}-{marker})"/>'


def fix_reuse_figure() -> str:
    """One agent records a fix; later agents elsewhere find and apply it."""
    p = "fr"
    case_lines = ["error words", "the fix", "how it was checked", "where it applies"]
    body = [
        card(p, (10, 30, 236, 250)),
        text(p, "h", (30, 62), "Agent A"),
        text(p, "m", (30, 82), "first to hit the error"),
        *numbered(p, (30, 122), "1", "runs the tool"),
        text(p, "c", (50, 142), "error E-LK17"),
        *numbered(p, (30, 182), "2", "works out the fix"),
        text(p, "m", (50, 200), "from notes or by trial"),
        *numbered(p, (30, 240), "3", "records the fix"),
        text(p, "c", (50, 260), "memd experience note"),
        card(p, (300, 90, 166, 150), hot=True),
        text(p, "h", (383, 122), "memd case", "middle"),
        *(
            text(p, "m", (383, 148 + 19 * i), line, "middle")
            for i, line in enumerate(case_lines)
        ),
        wire(p, "M246,165 L294,165", accent=True),
        text(p, "m", (270, 157), "note", "middle"),
        *reuse_card(p, (520, 10), "Agent B", "other project, same machine"),
        *reuse_card(p, (520, 164), "Agent C", "other machine, gets a copy"),
        wire(p, "M466,140 C493,140 493,78 514,78", accent=True),
        wire(p, "M466,190 C493,190 493,232 514,232", accent=True, dashed=True),
    ]
    title = "Agent A records a fix in memd; agents B and C find it and apply it."
    return svg(p, 310, title, body)


def reuse_card(p: str, origin: tuple[float, float], name: str, where: str) -> list[str]:
    """Draw an agent that hits the same error, finds the case, and applies it."""
    x, y = origin
    return [
        card(p, (x, y, 230, 136)),
        text(p, "h", (x + 20, y + 30), name),
        text(p, "m", (x + 20, y + 50), where),
        *numbered(p, (x + 20, y + 80), "1", "hits the same error"),
        text(p, "b", (x + 20, y + 102), "2"),
        text(p, "c", (x + 40, y + 102), "memd experience find"),
        *numbered(p, (x + 20, y + 124), "3", "applies the fix"),
    ]


def numbered(p: str, xy: tuple[float, float], number: str, step: str) -> list[str]:
    """Write one numbered step, with the text aligned past the number."""
    x, y = xy
    return [text(p, "b", (x, y), number), text(p, "b", (x + 20, y), step)]


def results_figure() -> str:
    """Dot chart: commands from the first error to a result, per session."""
    p, x0, scale = "rs", 240, 12
    body = [text(p, "m", (x0, 18), "commands from the first error to a result")]
    y = 54
    for test, groups in TESTS.items():
        body.append(text(p, "h", (10, y), test))
        y += 28
        for condition, sessions in groups.items():
            body.append(text(p, "b", (26, y), condition))
            y += 22
            for session in sessions:
                accent = condition == WITH_CASE
                body.extend(session_row(p, session, (x0, y), scale, accent=accent))
                y += 24
            y += 6
        y += 12
    body.extend(legend(p, (x0, y + 2)))
    title = (
        "Sessions with a memd case solved both problems soon after they found it; "
        "three of four sessions without one gave up."
    )
    return svg(p, y + 16, title, body)


def session_row(
    p: str, s: Session, origin: tuple[float, float], scale: float, *, accent: bool
) -> list[str]:
    """Draw one session: its label, track, end mark, find marker, and count."""
    x0, y = origin
    end = x0 + s.steps * scale
    parts = [
        text(p, "m", (x0 - 16, y + 4), s.label, "end"),
        f'<line class="{p}-track" x1="{x0}" y1="{y}" x2="{end}" y2="{y}"/>',
    ]
    if s.found_at is not None:
        parts.append(diamond(p, (x0 + s.found_at * scale, y)))
    if s.solved:
        parts.append(dot(p, (end, y), accent=accent))
    else:
        parts.append(cross(p, (end, y)))
    count = f"{s.steps} · {s.note}" if s.note else str(s.steps)
    parts.append(text(p, "m", (end + 12, y + 4), count))
    return parts


def legend(p: str, origin: tuple[float, float]) -> list[str]:
    """Explain the three marks in one row."""
    x, y = origin
    return [
        dot(p, (x + 5, y), accent=False),
        text(p, "m", (x + 16, y + 4), "solved"),
        cross(p, (x + 80, y)),
        text(p, "m", (x + 91, y + 4), "gave up"),
        diamond(p, (x + 165, y)),
        text(p, "m", (x + 176, y + 4), "found the recorded case"),
    ]


def dot(p: str, xy: tuple[float, float], *, accent: bool) -> str:
    """Draw a filled dot for a solved session; accent marks the memd group."""
    x, y = xy
    kind = "a" if accent else "ink"
    return f'<circle class="{p}-{kind}" cx="{x}" cy="{y}" r="5.5"/>'


def cross(p: str, xy: tuple[float, float]) -> str:
    """Draw a grey cross for a session that gave up."""
    x, y = xy
    first = f"M{x - 4.5},{y - 4.5} L{x + 4.5},{y + 4.5}"
    second = f"M{x - 4.5},{y + 4.5} L{x + 4.5},{y - 4.5}"
    return f'<path class="{p}-cross" d="{first} {second}"/>'


def diamond(p: str, xy: tuple[float, float]) -> str:
    """Draw an accent outline diamond where a session found the case."""
    x, y = xy
    d = f"M{x},{y - 6} L{x + 6},{y} L{x},{y + 6} L{x - 6},{y} z"
    return f'<path class="{p}-diamond" d="{d}"/>'


def experience_case_figure() -> str:
    """The records of one case, from problem to lesson and a later correction."""
    p = "ec"
    steps = [
        ((10, 120), "Problem", "error words"),
        ((165, 120), "Attempt 1", "what was tried"),
        ((320, 120), "Attempt 2", "the fix"),
    ]
    body = []
    for (x, w), name, detail in steps:
        body += [
            card(p, (x, 30, w, 70)),
            text(p, "h", (x + w / 2, 60), name, "middle"),
            text(p, "m", (x + w / 2, 80), detail, "middle"),
        ]
    body += [
        card(p, (475, 30, 130, 70), hot=True),
        text(p, "h", (540, 60), "Lesson", "middle"),
        text(p, "m", (540, 80), "fix + conditions", "middle"),
        f'<rect class="{p}-dash" x="640" y="30" width="110" height="70" rx="10"/>',
        text(p, "h", (695, 60), "Correction", "middle"),
        text(p, "m", (695, 80), "if needed", "middle"),
        wire(p, "M130,65 L159,65"),
        wire(p, "M285,65 L314,65"),
        wire(p, "M440,65 L469,65"),
        wire(p, "M605,65 L634,65", dashed=True),
        card(p, (170, 140, 110, 44)),
        text(p, "b", (225, 167), "check failed", "middle"),
        card(p, (325, 140, 110, 44)),
        text(p, "b", (380, 167), "check passed", "middle"),
        wire(p, "M225,100 L225,134"),
        wire(p, "M380,100 L380,134"),
        wire(p, "M435,162 C470,162 515,150 532,106", accent=True),
        text(p, "m", (492, 178), "supports", "middle"),
    ]
    title = (
        "An experience case: a problem, attempts with their checks, a lesson, "
        "and a later correction."
    )
    return svg(p, 196, title, body)


def architecture_figure() -> str:
    """Layers from callers to the files on disk, with the read and write paths."""
    p = "ar"
    callers = "coding agents (Claude Code, Codex)  ·  AI scientists  ·  people, scripts"
    retrieval = [
        "BM25 sparse index",
        "HNSW dense index, local embeddings",
        "fusion, reranking, hot cache",
    ]
    store = [
        "SQLite: metadata, tasks, experience cases",
        "append-only segments per tenant",
        "write-ahead log per tenant",
    ]
    concurrency = "reads open the store read-only; writes go through one writer"
    tenant = "tenants/&lt;id&gt;/{wal.log, segments/, warm_index/}"
    files = f"metadata.db · sparse_index/ · {tenant}"
    body = [
        card(p, (10, 10, 740, 64)),
        text(p, "h", (30, 38), "Callers"),
        text(p, "m", (30, 58), callers),
        wire(p, "M380,74 L380,98"),
        card(p, (10, 104, 740, 96)),
        text(p, "h", (30, 132), "memd CLI"),
        text(p, "m", (126, 132), "one binary; a warm worker keeps the indexes loaded"),
        text(p, "c", (30, 162), "search · agent-context"),
        text(p, "c", (410, 162), "add · experience note · find"),
        text(p, "m", (30, 186), concurrency),
        wire(p, "M190,200 L190,236"),
        text(p, "m", (200, 223), "ranked search"),
        wire(p, "M570,200 L570,236"),
        text(p, "m", (580, 223), "records and cases"),
        card(p, (10, 242, 360, 132)),
        text(p, "h", (30, 270), "Hybrid retrieval"),
        *(text(p, "b", (30, 298 + 22 * i), line) for i, line in enumerate(retrieval)),
        card(p, (390, 242, 360, 132)),
        text(p, "h", (410, 270), "Store"),
        *(text(p, "b", (410, 298 + 22 * i), line) for i, line in enumerate(store)),
        wire(p, "M190,374 L190,404"),
        wire(p, "M570,374 L570,404"),
        card(p, (10, 410, 740, 76)),
        text(p, "h", (30, 438), "On disk"),
        text(p, "c", (110, 438), "~/.memd/data/"),
        text(p, "c", (30, 466), files),
    ]
    title = "memd layers: callers, the CLI, retrieval and the store, and the files."
    return svg(p, 496, title, body)


def main() -> int:
    """Write every figure next to this script."""
    figures = {
        "fix-reuse.svg": fix_reuse_figure(),
        "fix-reuse-results.svg": results_figure(),
        "experience-case.svg": experience_case_figure(),
        "architecture-layers.svg": architecture_figure(),
    }
    for name, content in figures.items():
        (OUT / name).write_text(content, encoding="utf-8")
        print(f"wrote {OUT / name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
