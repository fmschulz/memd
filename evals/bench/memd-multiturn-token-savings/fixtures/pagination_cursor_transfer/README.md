# Pagination Cursor Fixture

This fixture models a webhook backfill that retries after transient write
failures. The visible failures could come from API pagination, deduplication, or
store retry behavior, but the contract bug is cursor advancement before the
page has been durably written.

Fix the implementation without changing public function names.

Run:

```bash
python3 -m unittest -q
```
