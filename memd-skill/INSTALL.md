# Installing the memd Skill

This skill tells agents when to use `memory.*`, `task.*`, and `artifact.*`.

It also ships with a bundled Linux binary:

- [bin/linux-x64/memd](bin/linux-x64/memd)

The preferred shared-session setup is a single local `memd` HTTP daemon that both Codex CLI and Claude Code connect to.

## Install the skill files

### Claude Code

```bash
mkdir -p ~/.claude/skills
cp -r memd-skill ~/.claude/skills/memd
```

### Codex CLI

```bash
mkdir -p ~/.codex/skills
cp -r memd-skill ~/.codex/skills/memd
```

Symlinks are fine during development:

```bash
ln -s /path/to/memd/memd-skill ~/.claude/skills/memd
ln -s /path/to/memd/memd-skill ~/.codex/skills/memd
```

## Install the bundled binary

If `memd` is not already on `PATH`, use the bundled binary.

```bash
mkdir -p ~/.local/bin
cp ~/.claude/skills/memd/bin/linux-x64/memd ~/.local/bin/memd
chmod +x ~/.local/bin/memd
```

Or use a symlink:

```bash
ln -sf ~/.claude/skills/memd/bin/linux-x64/memd ~/.local/bin/memd
```

Check it:

```bash
which memd
memd --version
```

## Start the shared local daemon

Use persistent mode for cross-session sharing:

```bash
memd --mode mcp --transport http --http-bind 127.0.0.1:8787
```

For a disposable local test run:

```bash
memd --mode mcp --transport http --http-bind 127.0.0.1:8787 --in-memory --data-dir /tmp/memd-demo
```

The default data dir is still `~/.memd/data`. You can override it globally in `~/.config/memd/config.toml`.

## Register the MCP server with clients

Prefer the client CLIs over manual file editing.

If you want the strongest available default behavior, use the enforcement installer:

```bash
./memd-skill/install_memd_enforcement.sh
```

That script:

- registers `memd` for Codex CLI and Claude Code
- upserts stronger `memd`-usage blocks into `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`
- makes `memd` mandatory for substantive multi-step technical and scientific work
- tells agents not to silently skip memory writes when `memd` is available

Optional wrapper install:

```bash
./memd-skill/install_memd_enforcement.sh --install-wrappers
```

### Codex CLI

```bash
codex mcp add memd --url http://127.0.0.1:8787/mcp
```

Codex stores this in `~/.codex/config.toml`.

The skill includes a matching template at:

- [mcp_config_codex.toml](mcp_config_codex.toml)

### Claude Code

```bash
claude mcp add --transport http --scope user memd http://127.0.0.1:8787/mcp
```

From a repo checkout, you can also run the helper:

```bash
./scripts/install_shared_http_clients.sh --append-snippets
```

That older helper is still useful for lightweight shared setup, but the stronger enforcement path is:

```bash
./memd-skill/install_memd_enforcement.sh
```

Claude stores this in `~/.claude.json`.

The skill includes a matching template at:

- [mcp_config_claude.json](mcp_config_claude.json)

## Add instruction snippets

MCP registration makes the tools available. The instruction snippets below make the agents use them reliably.

### `~/.codex/AGENTS.md`

```md
Use the `memd` MCP server as a shared knowledge base across sessions and agents.

Before substantive work, search `memd` with the current `tenant_id`.
For meaningful work, record `task.start`, `task.progress`, `task.run_start`, `task.run_finish`, `task.add_evidence`, and `task.finish`.
Use `artifact.create`, `artifact.search`, `artifact.get`, and `artifact.list_thread` when critique, revision, verification, or thread inspection matters.
Use the same `tenant_id` for agents that should share knowledge unless the user asks for a different memory scope.
```

### `~/.claude/CLAUDE.md`

```md
Use the `memd` MCP server as a shared knowledge base across sessions and agents.

Before substantive work, search `memd` with the current `tenant_id`.
For meaningful work, record `task.start`, `task.progress`, `task.run_start`, `task.run_finish`, `task.add_evidence`, and `task.finish`.
Use `artifact.create`, `artifact.search`, `artifact.get`, and `artifact.list_thread` when critique, revision, verification, or thread inspection matters.
Use the same `tenant_id` for agents that should share knowledge unless the user asks for a different memory scope.
```

## Verify the install

### Check the daemon directly

```bash
curl -sS -X POST http://127.0.0.1:8787/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"quickcheck","version":"0.1.0"}}}'
```

### Check Codex config

```bash
codex mcp list
codex mcp get memd
```

### Check Claude config

```bash
claude mcp list
claude mcp get memd
```

### Run the full cross-session verification

```bash
./scripts/verify_shared_http_clients.sh
```

### Verify the stronger enforcement setup

```bash
./memd-skill/verify_memd_enforcement.sh
```

That verifier checks:

- the enforcement blocks exist in `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`
- `memd` is registered for both clients
- Codex can write a task artifact into `memd`
- Claude can recover that task artifact from the same shared tenant

After that, verify that `memd` exposes the current tool surface, especially:

- `memory.add`
- `memory.add_batch`
- `memory.search`
- `task.start`
- `task.progress`
- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`
- `task.get`
- `task.search`
- `artifact.create`
- `artifact.get`
- `artifact.search`
- `artifact.list_thread`

## Troubleshooting

### `task.*` tools are missing

Likely causes:

1. The daemon is not running
2. The client points at the wrong URL
3. The client was not restarted after config changes

Check:

```bash
curl -sS http://127.0.0.1:8787/mcp -H 'Accept: text/event-stream' -i
codex mcp get memd
claude mcp get memd
```

`GET /mcp` returning `405 Method Not Allowed` is expected. It confirms the endpoint exists even though `memd` does not expose an SSE stream.

### Agents are not sharing memory

Check:

1. Same `tenant_id`
2. Same daemon URL
3. Same `data_dir`
4. Persistent mode, not `--in-memory`

### Agents still are not using `memd` often enough

Check:

1. You ran `./memd-skill/install_memd_enforcement.sh`
2. The enforcement block exists in both `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`
3. The clients were restarted after the files changed
4. The work is actually substantive enough to trigger the contract

### Manual config locations

If you prefer editing config files directly instead of using the client CLIs:

- Codex: `~/.codex/config.toml`
- Claude: `~/.claude.json`

## Next

Read [SKILL.md](SKILL.md).
