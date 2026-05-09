# Schema Defaults Fixture

This fixture models an in-memory migration and reporting pipeline. The failures
could look like report formatting or ingestion drift, but the contract bug is
that the migration adds a required field without backfilling old rows.

Fix the implementation without changing public function names.

Run:

```bash
python3 -m unittest -q
```
