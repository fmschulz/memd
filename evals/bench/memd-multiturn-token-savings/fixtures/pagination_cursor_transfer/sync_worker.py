"""Webhook backfill worker."""

from __future__ import annotations

from storage import TransientWriteError


def sync_once(api, store, state, batch_size: int = 2) -> dict:
    """Read one source page and write it to the store."""

    page = api.page_after(state.cursor, batch_size)
    if not page:
        return {"written": 0, "status": "done"}

    next_cursor = page[-1]["id"]
    state.set_cursor(next_cursor)
    try:
        store.upsert_many(page)
    except TransientWriteError:
        return {"written": 0, "status": "retry"}
    return {"written": len(page), "status": "ok"}


def sync_until_done(api, store, state, batch_size: int = 2, max_steps: int = 20) -> dict:
    """Run pages until the source is exhausted or a retry is required."""

    total = 0
    for _ in range(max_steps):
        result = sync_once(api, store, state, batch_size)
        total += result["written"]
        if result["status"] != "ok":
            return {"written": total, "status": result["status"]}
    return {"written": total, "status": "stopped"}
