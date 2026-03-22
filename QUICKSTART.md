# Quick Start

## 1. Build

```bash
cargo build --release
```

## 2. Start the shared local daemon

```bash
./target/release/memd --mode mcp --transport http --http-bind 127.0.0.1:8787
```

Or for a disposable local run:

```bash
./target/release/memd --mode mcp --transport http --http-bind 127.0.0.1:8787 --in-memory --data-dir /tmp/memd-quickstart
```

For legacy subprocess mode:

```bash
./target/release/memd --mode mcp
```

## 3. Check that HTTP MCP starts

```bash
curl -sS -X POST http://127.0.0.1:8787/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"quickstart","version":"0.1.0"}}}'
```

## 4. Use `task.*` for real work

Start a task:

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "tools/call",
  "params": {
    "name": "task.start",
    "arguments": {
      "tenant_id": "quickstart",
      "project_id": "auth",
      "goal": "Diagnose token validation failures",
      "motivation": "Production requests are failing",
      "hypothesis": "Time handling is inconsistent",
      "scientific_question": "Where does timestamp skew happen?",
      "expected_outputs": ["root cause", "fix"]
    }
  }
}
```

Then continue with:

- `task.progress`
- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`
- `task.get`
- `task.search`

## 5. Use `memory.*` for raw content

Example:

```json
{
  "jsonrpc": "2.0",
  "id": 20,
  "method": "tools/call",
  "params": {
    "name": "memory.add",
    "arguments": {
      "tenant_id": "quickstart",
      "text": "parseConfig reads TOML and validates fields",
      "type": "code",
      "project_id": "backend",
      "tags": ["rust", "config"]
    }
  }
}
```

## 6. Register clients and install the skill if needed

See:

- [memd-skill/INSTALL.md](/home/fschulz/dev/software/memd/memd-skill/INSTALL.md)
- [memd-skill/SKILL.md](/home/fschulz/dev/software/memd/memd-skill/SKILL.md)

The skill includes a bundled Linux binary:

- [memd-skill/bin/linux-x64/memd](/home/fschulz/dev/software/memd/memd-skill/bin/linux-x64/memd)

Register the shared daemon with current clients:

```bash
codex mcp add memd --url http://127.0.0.1:8787/mcp
claude mcp add --transport http --scope user memd http://127.0.0.1:8787/mcp
```

## 7. Optional ONNX cross-encoder reranker

ONNX here means the optional cross-encoder reranker, not the default embedding path.

Build it with:

```bash
cargo build --release --features cross-encoder-reranker
```

Run it with:

```bash
./target/release/memd --mode mcp --search-variant hybrid-cross-encoder
```

For the real ONNX smoke test:

```bash
cargo test -p memd --features cross-encoder-reranker smoke_real_onnx_scores_relevant_pair_higher -- --ignored --nocapture
```

See the ONNX section in [README.md](/home/fschulz/dev/software/memd/README.md) for cache location, runtime downloads, and env vars.
