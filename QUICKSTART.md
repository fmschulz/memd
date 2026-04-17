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

Start a task. In v0.4+ the only hard-required field is `goal`; `tenant_id` is
optional and falls back to `$MEMD_DEFAULT_TENANT` / `~/.memd/default_tenant` /
the literal `"default"`:

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "tools/call",
  "params": {
    "name": "task.start",
    "arguments": {
      "goal": "Diagnose token validation failures",
      "project_id": "auth"
    }
  }
}
```

You can still pass richer fields (`motivation`, `hypothesis`,
`scientific_question`, `expected_outputs`, …) when you have them; they are
optional now. Then continue with:

- `task.progress`
- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`
- `task.get`
- `task.search`
- `task.resume`

## 5. Use focused artifact tools for critique, revision, decisions, and verification

v0.4 replaces the single 50-field `artifact.create` schema with four focused
tools, each with a tight schema. The legacy `artifact.create` remains for
backwards compatibility.

```json
{
  "jsonrpc": "2.0",
  "id": 11,
  "method": "tools/call",
  "params": {
    "name": "artifact.review",
    "arguments": {
      "task_id": "reuse-the-task-id-from-task.start",
      "agent_id": "reviewer-1",
      "summary": "Need a clearer verification path for this task",
      "requested_action": "review"
    }
  }
}
```

Sibling wrappers:

- `artifact.revision` — supersede an earlier artifact
- `artifact.decision` — choose between alternatives with `why_chosen`
- `artifact.verification` — distinct-writer countersignature; with a different
  `agent_id` than the parent's and `supports_claim = true` it promotes the
  underlying claim to `VerifiedRecord` trust tier

Inspect or search the thread:

- `artifact.get`
- `artifact.search`
- `artifact.find_related` (the `artifact.verify` alias still works)
- `artifact.find_failures`
- `artifact.find_decisions`
- `artifact.find_evidence`
- `artifact.find_highlights`
- `artifact.list_thread`

`artifact.find_related` is a retrieval helper, not a trust primitive — it
surfaces canonical artifacts that overlap a claim. Trust requires a
countersignature from a different agent, not just retrieval overlap.

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

For structural tools such as `code.find_definition` and `code.find_callers`,
add code with a real `source.path` so `memd` can build the structural index.

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
      "text": "pub fn process_data(input: &str) -> String { input.to_string() }",
      "type": "code",
      "source": {
        "path": "src/lib.rs"
      }
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

That stronger path now also injects a pre-refusal rule: for substantive work, agents must check `memd` before saying the task is impossible, blocked, or unknowable.

If you also want runtime refusal guarding for one-shot runs, install wrappers and use:

- `codex-memd-guard` for `codex exec`-style runs
- `claude-memd-guard` for `claude -p` / `--print` runs

Set `MEMD_URL` and `MEMD_GUARD_TENANT_ID` when the audited memd endpoint or tenant is not the default local setup.

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
