"""Buffered writer used by the exporter."""

from __future__ import annotations


class BufferedWriter:
    def __init__(self, high_watermark: int = 2):
        self.high_watermark = high_watermark
        self.pending: list[str] = []
        self.output: list[str] = []
        self.flushes = 0
        self.drains = 0

    def write(self, chunk: str) -> bool:
        self.pending.append(chunk)
        return len(self.pending) < self.high_watermark

    def drain(self) -> None:
        self.drains += 1
        self.output.extend(self.pending)
        self.pending.clear()

    def flush(self) -> None:
        self.flushes += 1

    def text(self) -> str:
        return "".join(self.output)
