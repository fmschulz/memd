# Agent skill

The agent skill lives in
[memd-skill/](https://github.com/fmschulz/memd/tree/main/memd-skill).
The `memd` binary is distributed as prebuilt release artifacts (macOS arm64/x64,
Linux x86_64/aarch64 as static musl) built by cargo-dist.

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
The write-quality rules are documented in the
[Operational contract](operational-contract.md): agents should write concise
durable facts, avoid transcript-like process notes, and use audit/eval commands
when startup context looks noisy.

## Install

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
./memd-skill/install_memd_enforcement.sh
```

Linux releases are static musl, so there is no `GLIBC_... not found` pitfall.
`--install-binary` runs the one-line installer for you. Rust users can instead
`cargo binstall memd` (best-effort) or `cargo install memd` (from source).

What the script does:

1. Stops any running warm worker.
2. Installs the latest release binary into `~/.local/bin/memd` (only when
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

Release binaries are built for macOS and Linux (static musl) on every release
tag — see the [release workflow](https://github.com/fmschulz/memd/blob/main/.github/workflows/release.yml).
