from __future__ import annotations

import unittest

from scheduler import event_utc_iso, reminder_utc_iso


class SchedulerBoundaryTests(unittest.TestCase):
    def test_spring_forward_event_is_normalized_once(self) -> None:
        self.assertEqual(
            event_utc_iso("2026-03-08T01:30:00-08:00"),
            "2026-03-08T09:30:00Z",
        )

    def test_fall_back_event_is_normalized_once(self) -> None:
        self.assertEqual(
            event_utc_iso("2026-11-01T01:30:00-07:00"),
            "2026-11-01T08:30:00Z",
        )

    def test_reminder_uses_normalized_event_instant(self) -> None:
        self.assertEqual(
            reminder_utc_iso("2026-03-08T01:30:00-08:00", 15),
            "2026-03-08T09:15:00Z",
        )


if __name__ == "__main__":
    unittest.main()
