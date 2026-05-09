"""Dispatch export builder."""

from __future__ import annotations

from audit import audit_key
from formatting import comma_join, utc_z
from policy import policy_status
from time_math import minutes_before, parse_client_instant


def build_dispatch_record(job: dict) -> dict:
    """Build one dispatch record for the downstream export contract."""

    start = parse_client_instant(job["requested_start"])
    reminder_offsets = job.get("reminder_offsets", [30])
    reminder_times = [
        utc_z(minutes_before(job["requested_start"], minutes))
        for minutes in reminder_offsets
    ]
    return {
        "tenant": job["tenant"],
        "job_id": job["job_id"],
        "audit_key": audit_key(job),
        "start_utc": utc_z(start),
        "reminders_utc": reminder_times,
        "policy": policy_status(job["requested_start"], job.get("blackout_windows", [])),
        "technicians": comma_join(job.get("technicians", [])),
    }


def export_dispatch_batch(jobs: list[dict]) -> list[dict]:
    """Build records sorted by their actual UTC start time."""

    records = [build_dispatch_record(job) for job in jobs]
    return sorted(records, key=lambda record: (record["start_utc"], record["job_id"]))
