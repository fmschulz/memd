"""Log stream exporter."""

from __future__ import annotations

from filters import should_export
from formatter import format_record


def export_logs(records: list[dict], writer) -> dict:
    """Export filtered records to a buffered writer."""

    written = 0
    for record in records:
        if not should_export(record):
            continue
        ready = writer.write(format_record(record))
        written += 1
        if not ready:
            writer.flush()
            writer.drain()
    writer.flush()
    return {"written": written, "bytes": len(writer.text())}
