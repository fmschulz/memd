from __future__ import annotations

import unittest

from source_api import SourceApi
from state import SyncState
from storage import BackfillStore
from sync_worker import sync_once, sync_until_done


def records() -> list[dict]:
    return [{"id": i, "payload": f"event-{i}"} for i in range(1, 7)]


class PaginationCursorTests(unittest.TestCase):
    def test_retry_does_not_skip_failed_page(self) -> None:
        api = SourceApi(records())
        store = BackfillStore(fail_once_on_id=2)
        state = SyncState()

        self.assertEqual(sync_once(api, store, state, batch_size=2)["status"], "retry")
        self.assertIsNone(state.cursor)

        result = sync_until_done(api, store, state, batch_size=2)

        self.assertEqual(result["status"], "done")
        self.assertEqual(sorted(store.rows), [1, 2, 3, 4, 5, 6])
        self.assertEqual(api.calls[:2], [None, None])

    def test_successful_pages_advance_cursor_after_write(self) -> None:
        api = SourceApi(records())
        store = BackfillStore()
        state = SyncState()

        self.assertEqual(sync_once(api, store, state, batch_size=2)["status"], "ok")

        self.assertEqual(state.cursor, 2)
        self.assertEqual(sorted(store.rows), [1, 2])

    def test_empty_page_does_not_change_cursor(self) -> None:
        api = SourceApi(records())
        store = BackfillStore()
        state = SyncState()
        state.set_cursor(6)

        self.assertEqual(sync_once(api, store, state, batch_size=2)["status"], "done")

        self.assertEqual(state.cursor, 6)

    def test_idempotent_retry_does_not_duplicate_rows(self) -> None:
        api = SourceApi(records())
        store = BackfillStore()
        state = SyncState()

        sync_once(api, store, state, batch_size=2)
        state.cursor = None
        sync_once(api, store, state, batch_size=2)

        self.assertEqual(sorted(store.rows), [1, 2])


if __name__ == "__main__":
    unittest.main()
