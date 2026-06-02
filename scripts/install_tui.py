#!/usr/bin/env python3
"""Interactive selector to install or uninstall memd components.

Components:
  - binary       the memd CLI on PATH (~/.local/bin/memd)
  - skill        the agent skill in ~/.agents, ~/.claude, ~/.codex /skills/memd
  - enforcement  CLI-first agent rules + the Claude SessionStart hook

Drives the repo Makefile (`make install*` / `make uninstall*`). Falls back to a
plain status printout when not attached to a TTY. No private meta-repo paths.
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HOME = Path.home()
BIN = HOME / ".local" / "bin" / "memd"
AGENTS_SKILL = HOME / ".agents" / "skills" / "memd"
CLAUDE_MD = HOME / ".claude" / "CLAUDE.md"

# key, label, install targets, uninstall targets (None = no clean uninstall)
COMPONENTS = [
    ("binary", "Binary       ~/.local/bin/memd", ["install"], ["uninstall-binary"]),
    ("skill", "Skill        ~/.agents,~/.claude,~/.codex /skills/memd", ["install-skill"], ["uninstall-skill"]),
    ("enforcement", "Enforcement  agent rules + SessionStart hook", ["install-enforcement"], None),
]


def installed(key: str) -> bool:
    if key == "binary":
        return BIN.exists() or BIN.is_symlink()
    if key == "skill":
        return AGENTS_SKILL.exists() or AGENTS_SKILL.is_symlink()
    if key == "enforcement":
        try:
            return "memd-enforcement:start" in CLAUDE_MD.read_text(encoding="utf-8")
        except OSError:
            return False
    return False


def status_line(key: str) -> str:
    path = BIN if key == "binary" else AGENTS_SKILL if key == "skill" else None
    if path is not None:
        if path.is_symlink():
            return f"installed -> {os.readlink(path)}"
        if path.exists():
            return "installed (copy)"
        return "not installed"
    return "wired" if installed("enforcement") else "not wired"


def run_make(make: str, repo: Path, method: str, targets: list[str]) -> int:
    env = dict(os.environ, INSTALL_METHOD=method)
    rc = 0
    for target in targets:
        print(f"\n$ {make} -C {repo} {target}")
        rc |= subprocess.call([make, "-C", str(repo), target], env=env)
    return rc


def non_tty_status(make: str) -> int:
    print("memd components:")
    for key, label, _, _ in COMPONENTS:
        box = "x" if installed(key) else " "
        print(f"  [{box}] {label}  ({status_line(key)})")
    print("\nNot a TTY. Run one of:")
    print(f"  {make} install-all   # binary + skill + enforcement")
    print(f"  {make} install-skill-bundle   # copy skill + built binary into existing skill dirs")
    print(f"  {make} uninstall     # binary + skill")
    print(f"  {make} status")
    return 0


def _curses_ui(stdscr, make: str, repo: Path, method: str):
    import curses

    curses.curs_set(0)
    mode = "install"
    selected = {key: False for key, *_ in COMPONENTS}
    idx = 0

    def put(y: int, x: int, text: str, attr: int = 0) -> None:
        h, w = stdscr.getmaxyx()
        if 0 <= y < h and x < w:
            stdscr.addstr(y, x, text[: w - x - 1], attr)

    while True:
        stdscr.erase()
        put(0, 0, "memd installer", curses.A_BOLD)
        put(1, 0, f"mode: {mode.upper()}   [i] install   [u] uninstall   "
                  "space toggle   enter apply   q quit")
        for i, (key, label, _inst, uninst) in enumerate(COMPONENTS):
            available = mode == "install" or uninst is not None
            mark = "x" if selected[key] else " "
            attr = curses.A_REVERSE if i == idx else 0
            if not available:
                attr |= curses.A_DIM
            put(3 + i, 0, f"[{mark}] {label}  ({status_line(key)})", attr)
        put(3 + len(COMPONENTS) + 1, 0,
            "enforcement has no clean uninstall (edit agent rule files manually)"
            if mode == "uninstall" else "")
        stdscr.refresh()

        c = stdscr.getch()
        if c in (ord("q"), 27):
            return None
        if c in (curses.KEY_UP, ord("k")):
            idx = (idx - 1) % len(COMPONENTS)
        elif c in (curses.KEY_DOWN, ord("j")):
            idx = (idx + 1) % len(COMPONENTS)
        elif c == ord("i"):
            mode = "install"
        elif c == ord("u"):
            mode = "uninstall"
        elif c == ord(" "):
            key, _, _, uninst = COMPONENTS[idx]
            if mode == "install" or uninst is not None:
                selected[key] = not selected[key]
        elif c in (curses.KEY_ENTER, 10, 13):
            chosen = [(k, inst, un) for k, _, inst, un in COMPONENTS if selected[k]]
            if chosen:
                return mode, chosen


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--make-program", default=os.environ.get("MAKE", "make"))
    parser.add_argument("--install-method", default="symlink")
    args = parser.parse_args(argv)

    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        return non_tty_status(args.make_program)

    import curses

    result = curses.wrapper(_curses_ui, args.make_program, args.repo, args.install_method)
    if not result:
        print("Cancelled.")
        return 0
    mode, chosen = result
    rc = 0
    for _key, inst, uninst in chosen:
        targets = inst if mode == "install" else uninst
        if targets:
            rc |= run_make(args.make_program, args.repo, args.install_method, targets)
    print("\nDone." if rc == 0 else "\nCompleted with errors.")
    return rc


if __name__ == "__main__":
    sys.exit(main())
