#!/usr/bin/env python3
"""Strip Codex exec transcript down to the model's final answer.

Claude outputs are already just the model answer (pass through).
Codex outputs include tool-call logs — we take everything after the
LAST 'codex' marker line until the 'tokens used' footer.
"""
import pathlib, re

BENCH = pathlib.Path(__file__).resolve().parent.parent
RUNS = BENCH / "results" / "runs"
OUT = BENCH / "results" / "final"
OUT.mkdir(parents=True, exist_ok=True)

codex_marker = re.compile(r"^codex\s*$", re.MULTILINE)
tokens_footer = re.compile(r"^tokens used.*", re.MULTILINE)


def extract(path: pathlib.Path) -> str:
    txt = path.read_text(errors="replace")
    if not path.name.startswith("codex"):
        return txt.strip()
    matches = list(codex_marker.finditer(txt))
    if not matches:
        return txt.strip()
    tail = txt[matches[-1].end():].lstrip("\n")
    m = tokens_footer.search(tail)
    if m:
        tail = tail[:m.start()].rstrip()
    return tail.strip()


for p in sorted(RUNS.glob("*.txt")):
    final = extract(p)
    (OUT / p.name).write_text(final + "\n")
    print(f"{p.name}: {len(final)} chars")
