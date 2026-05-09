"""In-memory store with optional transient write failure."""

from __future__ import annotations


class TransientWriteError(RuntimeError):
    pass


class BackfillStore:
    def __init__(self, fail_once_on_id: int | None = None):
        self.fail_once_on_id = fail_once_on_id
        self.failed = False
        self.rows: dict[int, dict] = {}
        self.write_batches: list[list[int]] = []

    def upsert_many(self, records: list[dict]) -> None:
        ids = [record["id"] for record in records]
        self.write_batches.append(ids)
        if (
            self.fail_once_on_id is not None
            and self.fail_once_on_id in ids
            and not self.failed
        ):
            self.failed = True
            raise TransientWriteError(f"temporary write failure for {self.fail_once_on_id}")
        for record in records:
            self.rows[record["id"]] = dict(record)
