# Agent skill

The agent skill lives in
[memd-skill/](https://github.com/fmschulz/memd/tree/main/memd-skill).
It ships with a bundled Linux binary at
[memd-skill/bin/linux-x64/memd](https://github.com/fmschulz/memd/tree/main/memd-skill/bin/linux-x64).

## What it does

The skill is the **default way to make an agent use `memd` correctly**. It
upserts CLI guardrail blocks into `~/.codex/AGENTS.md` and
`~/.claude/CLAUDE.md`, so any agent run from those tools is told to:

1. Refresh `memory.md` at session start.
2. Search `memd` before substantive work.
3. Record meaningful progress, evidence, and decisions with `memd add`.
4. Run a `memd` search **before** claiming a task is impossible, blocked,
   or unknowable.

The installer touches only instruction files. It does not register external
client tools or wrap commands.

## Install

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

What the script does:

1. Stops any running warm worker.
2. Copies the bundled binary into `~/.local/bin/memd` (only when
   `--install-binary` is passed).
3. Upserts a CLI-first instruction block into:
    - `~/.codex/AGENTS.md`
    - `~/.claude/CLAUDE.md`
4. Prints a verification recipe.

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
output, audit logs, and the upserted instruction blocks.

## Start here

- [memd-skill/SKILL.md](https://github.com/fmschulz/memd/blob/main/memd-skill/SKILL.md)
- [memd-skill/INSTALL.md](https://github.com/fmschulz/memd/blob/main/memd-skill/INSTALL.md)
- [Codex session-start hook example](https://github.com/fmschulz/memd/blob/main/memd-skill/examples/codex_session_start_hook.json)

The bundled binary is refreshed automatically on every release tag — see the
[release workflow](https://github.com/fmschulz/memd/blob/main/.github/workflows/release-skill-binary.yml).
