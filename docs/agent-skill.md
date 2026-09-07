# Agent skill

The agent skill lives in
[memd-skill/](https://github.com/fmschulz/memd/tree/main/memd-skill).
The `memd` binary is distributed as prebuilt release artifacts (macOS arm64/x64,
Linux x86_64/aarch64 as static musl) built by cargo-dist.

## What it does

The installer provides the skill, upserts CLI guardrail blocks into
`~/.codex/AGENTS.md` and
`~/.claude/CLAUDE.md`, writes the matching Cursor user rule to
`~/.cursor/rules/memd.mdc`, and wires a Claude Code `SessionStart` hook in
`~/.claude/settings.json`, so agent sessions are told to:

1. Read the generated `memory.md` if present.
2. Search for a specific environment failure, cross-machine fact, or conflict
   with an earlier observation.
3. Store only facts that readable repository files cannot answer. Keep project
   plans, commands, corrections, and handoffs in the repository.
4. Verify retrieved facts against current evidence and attribute only memories
   that affected an independently verified task outcome.
5. Keep secrets and sensitive log values out of memory.

No memory call is required for routine work. A retrieval hit shows that a
record was found; it does not show that the record saved work.

The installer does not register external client tools or wrap commands.
The write-quality rules are documented in the
[Operational contract](operational-contract.md): agents should write concise
durable facts, avoid transcript-like process notes, and use audit/eval commands
when startup context looks noisy.

## Install

Option A (recommended): clone and install the binary, skill, and enforcement:

```bash
git clone --depth 1 https://github.com/fmschulz/memd
cd memd
make install-prebuilt   # prebuilt binary (seconds; compiles only if needed) + skill + enforcement
memd doctor
```

Option B: prebuilt binary only (no clone):

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
```

The prebuilt installer installs only the binary. For everything without
compiling, run `make install-prebuilt` from a clone — it tests the prebuilt
release binary and builds from source only if that fails. `make install`
uses the same path. `make install-source` always builds from source,
`make install-binary` installs only the binary. `make menu` opens an
interactive TUI to pick components, and
`make uninstall` removes what `make install` installed.

Linux releases are static musl, so there is no `GLIBC_... not found` pitfall.
Rust users can use `cargo binstall memd` (best-effort) or `cargo install memd`
as manual source-install paths.

What `make install` does:

1. Stops any running warm worker.
2. Installs a compatible prebuilt binary, or builds from source, on `PATH`.
3. Installs the agent skill.
4. Upserts a CLI-first instruction block into:
    - `~/.codex/AGENTS.md`
    - `~/.claude/CLAUDE.md`
5. Writes the Cursor user rule at `~/.cursor/rules/memd.mdc`.
6. Wires the Claude Code `SessionStart` hook.
7. Prints a verification recipe.

`memd session-start` reads `.memd/project_scope.json` first. When that file is
absent, it uses a valid legacy `.memd/config.json` scope. When neither provides
a scope, it creates a minimal project scope from `$MEMD_DEFAULT_TENANT` (then
`$USER`, then `"default"`) and the repo basename. Derived IDs are lowercased,
separator runs become underscores, and IDs are capped at 64 characters.
Set `MEMD_AUTO_SCOPE=0` to disable scope creation. Add `.memd-skip` in the repo
root to skip startup even with an existing scope. Invalid or unreadable scopes
are preserved and reported, and startup stops before refreshing `memory.md`.
A valid `.memd/config.json` with neither `tenant_id` nor `project_id`, such as
a wiki-only configuration, allows automatic scope creation. A legacy file with
a project but no tenant is invalid.

Generated `memory.md` and explanation files are replaced atomically, so readers
see a complete prior or current file. Output symlinks are followed even when
their target file does not yet exist; the target directory must exist. Existing
Unix mode bits are preserved. New output files use mode `0600` (owner read/write).
For shared access, change the target file's permissions after it is first written.
Replacement requires write access to both an existing target and its directory.
It changes the inode and does not preserve ownership, access-control lists,
extended attributes, or hard-link identity. Complete-file visibility does not
guarantee recovery after a system crash. A process killed before replacement can
leave its temporary output file behind.

An asynchronous host hook can still leave a reader with the prior file; check
its generation date when freshness matters. Concurrent first starts can briefly
report a scope as invalid while another process finishes creating it; retry
startup after the first process exits.

For a repo-local install (writes `.memd/` plus per-repo `AGENTS.md` and
`CLAUDE.md` guardrail blocks):

```bash
memd init --tenant-id <tenant> --project-id <project>
```

During local development, `make install-skill` installs the skill as symlinks.
Use `make install-skill-bundle` when you need copied skill directories that
also carry the current repo-built binary at `bin/linux-x64/memd`; it updates
only unique existing standard skill directories among `~/.agents/skills`,
`~/.claude/skills`, and `~/.codex/skills`.

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
