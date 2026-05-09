# Stream Backpressure Fixture

This fixture models a log exporter with a buffered writer. The failures could
look like filtering, parsing, or chunk framing, but the contract bug is that the
exporter flushes before the writer has drained pending chunks.

Fix the implementation without changing public function names.

Run:

```bash
python3 -m unittest -q
```
