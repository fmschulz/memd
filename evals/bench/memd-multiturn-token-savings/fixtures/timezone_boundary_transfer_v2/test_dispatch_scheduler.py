from __future__ import annotations

import unittest

from schedule_builder import build_dispatch_record, export_dispatch_batch
from time_math import parse_client_instant


class DispatchSchedulerContractTests(unittest.TestCase):
    def test_requested_start_is_contract_utc_instant(self) -> None:
        record = build_dispatch_record(
            {
                "tenant": "acme",
                "job_id": "spring-a",
                "requested_start": "2026-03-08T01:30:00-08:00",
                "technicians": ["lee", "morgan"],
            }
        )

        self.assertEqual(record["start_utc"], "2026-03-08T09:30:00Z")
        self.assertEqual(record["technicians"], "lee,morgan")

    def test_fall_back_start_is_not_shifted_twice(self) -> None:
        instant = parse_client_instant("2026-11-01T01:30:00-07:00")

        self.assertEqual(instant.isoformat(), "2026-11-01T08:30:00+00:00")

    def test_reminder_offsets_cross_hour_boundary(self) -> None:
        record = build_dispatch_record(
            {
                "tenant": "acme",
                "job_id": "spring-b",
                "requested_start": "2026-03-08T01:15:00-08:00",
                "reminder_offsets": [45, 90],
            }
        )

        self.assertEqual(
            record["reminders_utc"],
            ["2026-03-08T08:30:00Z", "2026-03-08T07:45:00Z"],
        )

    def test_batch_sort_uses_actual_utc_not_local_clock_or_offset(self) -> None:
        rows = export_dispatch_batch(
            [
                {
                    "tenant": "acme",
                    "job_id": "after-jump",
                    "requested_start": "2026-03-08T03:05:00-07:00",
                    "reminder_offsets": [],
                },
                {
                    "tenant": "acme",
                    "job_id": "before-jump",
                    "requested_start": "2026-03-08T01:20:00-08:00",
                    "reminder_offsets": [],
                },
            ]
        )

        self.assertEqual([row["job_id"] for row in rows], ["before-jump", "after-jump"])
        self.assertEqual(
            [row["start_utc"] for row in rows],
            ["2026-03-08T09:20:00Z", "2026-03-08T10:05:00Z"],
        )

    def test_blackout_policy_uses_local_wall_clock(self) -> None:
        record = build_dispatch_record(
            {
                "tenant": "acme",
                "job_id": "blocked-local",
                "requested_start": "2026-03-08T01:15:00-08:00",
                "blackout_windows": [("01:00", "02:00")],
                "reminder_offsets": [],
            }
        )

        self.assertEqual(record["policy"], "blocked")

    def test_audit_key_keeps_customer_visible_service_day(self) -> None:
        record = build_dispatch_record(
            {
                "tenant": "acme",
                "job_id": "audit-local",
                "requested_start": "2026-03-08T23:50:00-08:00",
            }
        )

        self.assertEqual(record["audit_key"], "acme:audit-local:2026-03-08")


if __name__ == "__main__":
    unittest.main()
