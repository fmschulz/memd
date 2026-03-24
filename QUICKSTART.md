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
- `task.resume`

## 5. Use `artifact.*` for critique, verification, and thread inspection

Example:

```json
{
  "jsonrpc": "2.0",
  "id": 11,
  "method": "tools/call",
  "params": {
    "name": "artifact.create",
    "arguments": {
      "tenant_id": "quickstart",
      "artifact_kind": "review",
      "task_id": "reuse-the-task-id-from-task.start",
      "artifact_role": "critique",
      "summary": "Need a clearer verification path for this task",
      "requested_action": "review",
      "verification_status": "pending"
    }
  }
}
```

Then inspect or search the thread with:

- `artifact.get`
- `artifact.search`
- `artifact.find_failures`
- `artifact.find_decisions`
- `artifact.find_evidence`
- `artifact.list_thread`

Optional safety metadata such as `compute_budget`, `cost_actual`,
`data_access_level`, `policy_tags`, `allowed_tools`, and `approval_state` can
also be sent through `artifact.create`. They are optional in the current local
prototype.

## 6. Use `memory.*` for raw content

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

## 7. Use summary-first retrieval when you need a briefing

Example:

```json
{
  "jsonrpc": "2.0",
  "id": 21,
  "method": "tools/call",
  "params": {
    "name": "context.brief_project",
    "arguments": {
      "tenant_id": "quickstart",
      "project_id": "auth",
      "query": "What changed most recently?"
    }
  }
}
```

Other summary-first helpers:

- `task.resume`
- `artifact.find_failures`
- `artifact.find_decisions`
- `artifact.find_evidence`

`memory.search`, `task.search`, and `artifact.search` also accept `mode` with `brief_project`, `resume_task`, `find_failures`, `find_decisions`, or `find_evidence`.

To refresh project brief and library digests explicitly, call `memory.compact` with `project_id` and, when needed, `digest_modes` plus `force_digest_rebuild`.

## 8. Register clients and install the skill if needed

See:

- [memd-skill/INSTALL.md](memd-skill/INSTALL.md)
- [memd-skill/SKILL.md](memd-skill/SKILL.md)

The skill includes a bundled Linux binary:

- [memd-skill/bin/linux-x64/memd](memd-skill/bin/linux-x64/memd)

Register the shared daemon with current clients:

```bash
codex mcp add memd --url http://127.0.0.1:8787/mcp
claude mcp add --transport http --scope user memd http://127.0.0.1:8787/mcp
```

If you want stronger habitual `memd` usage from both clients, run:

```bash
./memd-skill/install_memd_enforcement.sh
```

## 9. Optional ONNX cross-encoder reranker

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

See the ONNX section in [README.md](README.md) for cache location, runtime downloads, and env vars.
