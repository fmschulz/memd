# Dispatch Scheduler Fixture

This fixture models a small dispatch export pipeline. The failures look like
they could come from export ordering, blackout policy, audit keys, or reminder
math, but the contract bug is in shared time normalization.

Fix the implementation without changing public function names.

Run:

```bash
python3 -m unittest -q
```
