"""Audit-key helpers for dispatch exports."""

from __future__ import annotations

from datetime import datetime


def local_service_day(local_iso: str) -> str:
    """Return the customer-visible service day from the supplied timestamp."""

    return datetime.fromisoformat(local_iso).date().isoformat()


def audit_key(job: dict) -> str:
    """Build a stable key used by downstream idempotency checks."""

    return f"{job['tenant']}:{job['job_id']}:{local_service_day(job['requested_start'])}"
