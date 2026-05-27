# Agent skill

The agent skill lives in
[memd-skill/](https://github.com/fmschulz/memd/tree/main/memd-skill).
It ships with a bundled Linux binary at
[memd-skill/bin/linux-x64/memd](https://github.com/fmschulz/memd/tree/main/memd-skill/bin/linux-x64).

## What it does

The skill is the **default way to make an agent use `memd` correctly**. It
upserts CLI guardrail blocks into `~/.codex/AGENTS.md` and
`~/.claude/CLAUDE.md`, writes the matching Cursor user rule to
`~/.cursor/rules/memd.mdc`, and wires a Claude Code `SessionStart` hook in
`~/.claude/settings.json`, so agent sessions are told to:

1. Refresh `memory.md` at session start.
2. Search `memd` before substantive work.
3. Record meaningful progress, evidence, and decisions with `memd add`.
4. Run a `memd` search **before** claiming a task is impossible, blocked,
   or unknowable.
5. Keep stored memories concise and reusable; do not store full chat logs,
   secrets, credentials, private account data, or sensitive values copied from
   logs.

The installer does not register external client tools or wrap commands.

## Install

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

If the bundled binary fails with a `GLIBC_... not found` error, the host is
older than the release build environment. Build a host-compatible binary from
the checkout instead:

```bash
cargo build --release -p memd
install -m 0755 target/release/memd ~/.local/bin/memd
./memd-skill/install_memd_enforcement.sh
memd doctor
```

Run the installer without `--install-binary` after a local build; otherwise it
will replace the host-built binary with the bundled one again.

What the script does:

1. Stops any running warm worker.
2. Copies the bundled binary into `~/.local/bin/memd` (only when
   `--install-binary` is passed).
3. Upserts a CLI-first instruction block into:
    - `~/.codex/AGENTS.md`
    - `~/.claude/CLAUDE.md`
4. Writes the Cursor user rule at `~/.cursor/rules/memd.mdc`.
5. Wires the Claude Code `SessionStart` hook.
6. Prints a verification recipe.

When the SessionStart hook fires in a repo without `.memd/project_scope.json`,
`memd session-start` auto-creates a minimal scope from
`$MEMD_DEFAULT_TENANT` (then `$USER`, then `"default"`) and the repo basename.
Set `MEMD_AUTO_SCOPE=0` or add `.memd-skip` in the repo root to opt out.

For a repo-local install (writes `.memd/` plus per-repo `AGENTS.md` and
`CLAUDE.md` guardrail blocks):

```bash
memd init --tenant-id <tenant> --project-id <project>
```

## Verify

```bash
./memd-skill/verify_memd_enforcement.sh
```

The script exercises the skill + CLI path: add, search, agent-context
output, audit logs, the upserted instruction blocks, the Cursor rule, and
`memd doctor`.

For a quick host wiring check:

```bash
memd doctor
```

## Start here

- [memd-skill/SKILL.md](https://github.com/fmschulz/memd/blob/main/memd-skill/SKILL.md)
- [memd-skill/INSTALL.md](https://github.com/fmschulz/memd/blob/main/memd-skill/INSTALL.md)
- [Codex session-start hook example](https://github.com/fmschulz/memd/blob/main/memd-skill/examples/codex_session_start_hook.json)

The bundled binary is refreshed automatically on every release tag — see the
[release workflow](https://github.com/fmschulz/memd/blob/main/.github/workflows/release-skill-binary.yml).
