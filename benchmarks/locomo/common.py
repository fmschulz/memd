"""Shared plumbing for the repo-local LoCoMo benchmark harness.

Hermetic rule: every memd process launched here runs with HOME pointed at
fable-work/home/ and MEMD_DATA_DIR pointed at a per-run store directory, so
model caches, config, and stores stay inside this repository.
"""

import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DATASET_PATH = REPO_ROOT / "benchmark-data" / "locomo10.json"
DATASET_URL = "https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json"
DATASET_SHA256 = "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4"
MEMD_BIN = REPO_ROOT / "target" / "release" / "memd"
HERMETIC_HOME = REPO_ROOT / "fable-work" / "home"
RUN_OUTPUT = REPO_ROOT / "run-output"
TENANT = "locomo"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def ensure_dataset() -> Path:
    if not DATASET_PATH.exists():
        sys.exit(
            f"dataset missing: {DATASET_PATH}\n"
            f"fetch it first:\n  curl -fsSL -o {DATASET_PATH} {DATASET_URL}"
        )
    actual = sha256_file(DATASET_PATH)
    if actual != DATASET_SHA256:
        sys.exit(f"dataset SHA256 mismatch: expected {DATASET_SHA256}, got {actual}")
    return DATASET_PATH


def load_dataset():
    return json.loads(ensure_dataset().read_text())


def hermetic_env(data_dir: Path) -> dict:
    """Environment for a memd process: repo-local HOME, cache, and store.

    memd resolves its data dir from $HOME/.config/memd/config.toml (there is
    no MEMD_DATA_DIR env override — the string appears only in a source
    comment), so we write a per-run config into the hermetic HOME. XDG vars
    are pinned explicitly because the caller's environment may point them at
    the real user cache.
    """
    HERMETIC_HOME.mkdir(parents=True, exist_ok=True)
    config_dir = HERMETIC_HOME / ".config" / "memd"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.toml").write_text(
        f'data_dir = "{data_dir}"\nlog_level = "error"\n'
    )
    return {
        "HOME": str(HERMETIC_HOME),
        "XDG_CACHE_HOME": str(HERMETIC_HOME / ".cache"),
        "XDG_CONFIG_HOME": str(HERMETIC_HOME / ".config"),
        "XDG_DATA_HOME": str(HERMETIC_HOME / ".local" / "share"),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "RUST_LOG": "error",
        "NO_COLOR": "1",
    }


def run_batch(requests, data_dir: Path, timeout=3600):
    """Feed JSONL requests to one `memd batch` process.

    Returns (rows, row_wall_times): parsed response rows and a parallel list
    of monotonic timestamps when each stdout line was read (for latency
    percentiles without per-process startup noise).
    """
    # --warm off: direct in-process writes/reads. The default (--warm auto)
    # spawns a background warm worker whose 30s client timeout aborts large
    # seeding batches while it rebuilds indexes; a daemon also breaks
    # run-to-run isolation.
    proc = subprocess.Popen(
        [str(MEMD_BIN), "batch", "--jsonl", "-", "--continue-on-error", "--warm", "off"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=hermetic_env(data_dir),
        text=True,
    )
    payload = "\n".join(json.dumps(r, ensure_ascii=False) for r in requests) + "\n"
    # Write in a thread-free way: batch streams responses per line, but its
    # stdin buffer is large enough for our request sizes; if this ever
    # deadlocks, switch to a writer thread.
    import threading

    def _feed():
        try:
            proc.stdin.write(payload)
            proc.stdin.close()
        except BrokenPipeError:
            pass

    feeder = threading.Thread(target=_feed)
    feeder.start()

    rows = []
    row_times = []
    start = time.monotonic()
    for line in proc.stdout:
        now = time.monotonic()
        line = line.strip()
        if not line:
            continue
        rows.append(json.loads(line))
        row_times.append(now - start)
    feeder.join()
    proc.wait(timeout=timeout)
    stderr = proc.stderr.read()
    if proc.returncode != 0:
        sys.exit(f"memd batch failed rc={proc.returncode}: {stderr[-2000:]}")
    return rows, row_times


def memd_version(data_dir: Path) -> str:
    out = subprocess.run(
        [str(MEMD_BIN), "--version"],
        capture_output=True,
        text=True,
        env=hermetic_env(data_dir),
    )
    return out.stdout.strip() or out.stderr.strip()


def write_manifest(run_dir: Path, manifest: dict):
    manifest = dict(manifest)
    manifest["dataset_url"] = DATASET_URL
    manifest["dataset_sha256"] = DATASET_SHA256
    lock = REPO_ROOT / "Cargo.lock"
    if lock.exists():
        manifest["cargo_lock_sha256"] = sha256_file(lock)
    head = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    )
    manifest["git_commit"] = head.stdout.strip()
    dirty = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "status", "--porcelain"],
        capture_output=True,
        text=True,
    )
    manifest["git_dirty"] = bool(dirty.stdout.strip())
    (run_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


def iter_turns(conversation: dict):
    """Yield (session_key, session_datetime, turn dict) in stable file order."""
    session_keys = sorted(
        (k for k in conversation if k.startswith("session_") and isinstance(conversation[k], list)),
        key=lambda k: int(k.split("_")[1]),
    )
    for key in session_keys:
        date_time = conversation.get(f"{key}_date_time")
        for turn in conversation[key]:
            yield key, date_time, turn


def turn_text(turn: dict, session_datetime, fmt: str) -> str:
    """Render one turn for seeding.

    fmt "plain":  "<speaker>: <text>[ [shares photo: caption]]"
    fmt "dated":  "[<session datetime>] <speaker>: <text>[ ...]"
    """
    parts = []
    text = (turn.get("text") or "").strip()
    caption = (turn.get("blip_caption") or "").strip()
    body = text
    if caption:
        suffix = f"[shares a photo: {caption}]"
        body = f"{text} {suffix}".strip() if text else suffix
    speaker = turn.get("speaker") or "unknown"
    if fmt == "dated" and session_datetime:
        parts.append(f"[{session_datetime}]")
    parts.append(f"{speaker}: {body}")
    return " ".join(parts)
