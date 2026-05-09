from __future__ import annotations

import unittest

from exporter import export_logs
from writer import BufferedWriter


def log_records() -> list[dict]:
    return [
        {"ts": "10:00", "level": "DEBUG", "message": "skip-me"},
        {"ts": "10:01", "level": "INFO", "message": "first"},
        {"ts": "10:02", "level": "WARN", "message": "second"},
        {"ts": "10:03", "level": "ERROR", "message": "third"},
    ]


class StreamBackpressureTests(unittest.TestCase):
    def test_final_partial_buffer_is_drained_before_flush(self) -> None:
        writer = BufferedWriter(high_watermark=10)

        result = export_logs(log_records(), writer)

        self.assertEqual(result["written"], 3)
        self.assertIn("first", writer.text())
        self.assertIn("second", writer.text())
        self.assertIn("third", writer.text())

    def test_backpressure_drains_full_buffer(self) -> None:
        writer = BufferedWriter(high_watermark=2)

        export_logs(log_records(), writer)

        self.assertEqual(writer.drains, 2)
        self.assertEqual(writer.text().count("\n"), 3)

    def test_debug_records_are_not_exported(self) -> None:
        writer = BufferedWriter(high_watermark=1)

        export_logs(log_records(), writer)

        self.assertNotIn("skip-me", writer.text())

    def test_reported_bytes_match_output(self) -> None:
        writer = BufferedWriter(high_watermark=10)

        result = export_logs(log_records(), writer)

        self.assertEqual(result["bytes"], len(writer.text()))


if __name__ == "__main__":
    unittest.main()
