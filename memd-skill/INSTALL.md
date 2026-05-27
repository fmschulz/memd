# Installing the memd Skill

This skill makes `memd` a skill + CLI workflow. Agents retrieve context with
`memd agent-context` or `memd search` and record durable memory with `memd add`.

It ships with a bundled Linux binary:

- [bin/linux-x64/memd](bin/linux-x64/memd)

## Install the Skill Files

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

## Install the Bundled Binary

If `memd` is not already on `PATH`, use the bundled binary.

```bash
mkdir -p ~/.local/bin
cp ~/.claude/skills/memd/bin/linux-x64/memd ~/.local/bin/memd
chmod +x ~/.local/bin/memd
```

Or use the installer:

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

Check it:

```bash
which memd
memd --version
```

On older enterprise or HPC Linux hosts, the bundled binary can fail before it
prints a version, with an error like:

```text
memd: /lib64/libc.so.6: version `GLIBC_2.xx' not found
```

That means the release binary was built against a newer glibc than the host
provides. Build and install a host-compatible binary from the checkout instead:

```bash
cargo build --release -p memd
install -m 0755 target/release/memd ~/.local/bin/memd
hash -r
memd --version
./memd-skill/install_memd_enforcement.sh
memd doctor
```

After a local build, run the installer without `--install-binary`; passing
`--install-binary` again will overwrite the host-built binary with the bundled
one.

## Install CLI Enforcement Instructions

```bash
./memd-skill/install_memd_enforcement.sh
```

The script:

- upserts CLI-first `memd` rules into `~/.codex/AGENTS.md`
- upserts CLI-first `memd` rules into `~/.claude/CLAUDE.md`
- writes the same contract as a Cursor user rule at `~/.cursor/rules/memd.mdc`
  (`alwaysApply: true`)
- wires a Claude Code `SessionStart` hook in `~/.claude/settings.json`
- makes CLI retrieval mandatory before substantive work
- makes CLI writes mandatory before final substantive answers
- adds a pre-refusal rule requiring a relevant CLI memory search before an
  agent says work is impossible, blocked, or unknowable
- tells agents not to store full chat logs, play-by-play transcripts, cookies,
  tokens, API keys, passwords, verification codes, ID numbers, bank cards,
  private contact details, third-party account configuration, or sensitive log
  values in `memd`

It does not register external client tools and does not install wrapper guards.

After the installer runs, verify the wiring:

```bash
memd doctor
```

`memd doctor` reports the state of: the `memd` binary, data directory,
global agent rules (Claude / Codex / Cursor), the Claude `SessionStart`
hook, and the current project's `.memd` scope. Use `--format json` for
machine-readable output.

## Basic CLI Workflow

Add a memory:

```bash
memd add \
  --tenant-id quickstart \
  --project-id auth \
  --chunk-type summary \
  --tags kind:note,source:install \
  --text "parseConfig reads TOML and validates required auth fields"
```

Search:

```bash
memd search \
  --tenant-id quickstart \
  --project-id auth \
  --query "auth config validation" \
  --compact \
  --token-budget 2000 \
  --format markdown
```

Build bounded context for an agent:

```bash
memd agent-context \
  --tenant-id quickstart \
  --project-id auth \
  --query "auth config validation prior work" \
  --k 2 \
  --token-budget 700 \
  --format markdown \
  --output .memd/context.md \
  --log-dir .memd/search-logs
```

Refresh project-root `memory.md` at session start:

```bash
memd memory-md \
  --tenant-id quickstart \
  --project-id auth \
  --project-dir . \
  --output memory.md
```

When `.memd/project_scope.json` exists, `memd memory-md --project-dir .
--output memory.md` can infer the tenant and project scope.

Keep repeated retrieval hot with the private CLI warm worker:

```bash
memd warm start
memd search --warm required \
  --tenant-id quickstart \
  --project-id auth \
  --query "auth config validation"
memd warm stop
```

Run many structured operations in one loaded process:

```bash
printf '%s\n' \
  '{"tool":"memory.search","arguments":{"tenant_id":"quickstart","project_id":"auth","query":"auth config validation","k":3}}' \
  | memd batch --jsonl -
```

Record progress:

```bash
memd add \
  --tenant-id quickstart \
  --project-id auth \
  --chunk-type summary \
  --tags kind:progress,task:jwt-auth \
  --text "Mapped auth middleware touchpoints; next step is RS256 issuance and validation tests."
```

Keep writes concise and reusable. `memd` is for durable facts, decisions,
evidence, commands, parameters, validation, and follow-ups; it is not a place
to archive full chat logs or sensitive credentials.

## Repository Guardrails

### Implicit (no per-repo setup)

When the `SessionStart` hook fires in a repo with no `.memd/project_scope.json`,
`memd session-start` auto-creates a minimal scope file using:

- `tenant_id`: `$MEMD_DEFAULT_TENANT`, then `$USER`, then `"default"`
- `project_id`: lower-cased basename of the repo

This is what makes the "clone the installer, open any repo in Claude Code /
Codex / Cursor, and memd just works" UX hold. Auto-scope writes ONLY
`.memd/project_scope.json` — it never touches `AGENTS.md`, `CLAUDE.md`, or
writes tenant guardrails on the user's behalf. Opt out by setting
`MEMD_AUTO_SCOPE=0` in the environment, or dropping an empty `.memd-skip`
file in the repo root.

### Explicit (full guardrails)

For richer per-repo guardrails — tenant scope rules, AGENTS.md / CLAUDE.md
upserts, custom `read_tenants` — run:

```bash
memd init --tenant-id quickstart --project-id auth
```

Generated files:

- `.memd/memory_guardrails.md`
- `.memd/tenant_scope.json`
- `.memd/project_scope.json`

If `AGENTS.md` and `CLAUDE.md` are writable in the project root, `memd init`
can upsert local CLI guardrail sections there as well.

## Verify the Install

```bash
./memd-skill/verify_memd_enforcement.sh
```

That verifier checks:

- the enforcement blocks exist in `~/.codex/AGENTS.md` and
  `~/.claude/CLAUDE.md`
- both blocks describe the CLI contract
- `~/.cursor/rules/memd.mdc` exists and carries the CLI contract
- `memd doctor --format json` runs cleanly
- `memd add` stores a test memory
- `memd search` recovers it
- `memd agent-context` writes a CLI-only context file and JSONL audit log

## Troubleshooting

### `memd` is not found

Install the bundled binary:

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

Then make sure `~/.local/bin` is on `PATH`.

If that command installs a binary that fails with `GLIBC_... not found`, build
from source on the target host instead:

```bash
cargo build --release -p memd
install -m 0755 target/release/memd ~/.local/bin/memd
./memd-skill/install_memd_enforcement.sh
memd doctor
```

Do not rerun the installer with `--install-binary` on that host unless the
bundled binary has been rebuilt for its glibc version.

### Agents are not sharing memory

Check:

1. Same `tenant_id`
2. Same `project_id` when project scope matters
3. Same `--data-dir` or default `~/.memd/data`
4. Persistent mode, not a disposable temp data directory

### Agents are not using `memd`

Check:

1. You ran `./memd-skill/install_memd_enforcement.sh`
2. The enforcement block exists in both instruction files and the Cursor rule
3. The clients were restarted after the files changed
4. The work is substantive enough to trigger the contract

## Next

Read [SKILL.md](SKILL.md).
