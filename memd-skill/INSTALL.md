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

## Install CLI Enforcement Instructions

```bash
./memd-skill/install_memd_enforcement.sh
```

The script:

- upserts CLI-first `memd` rules into `~/.codex/AGENTS.md`
- upserts CLI-first `memd` rules into `~/.claude/CLAUDE.md`
- makes CLI retrieval mandatory before substantive work
- makes CLI writes mandatory before final substantive answers
- adds a pre-refusal rule requiring a relevant CLI memory search before an
  agent says work is impossible, blocked, or unknowable

It does not register external client tools and does not install wrapper guards.

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

## Repository Guardrails

Initialize a repository-scoped `.memd/` directory:

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

### Agents are not sharing memory

Check:

1. Same `tenant_id`
2. Same `project_id` when project scope matters
3. Same `--data-dir` or default `~/.memd/data`
4. Persistent mode, not a disposable temp data directory

### Agents are not using `memd`

Check:

1. You ran `./memd-skill/install_memd_enforcement.sh`
2. The enforcement block exists in both instruction files
3. The clients were restarted after the files changed
4. The work is substantive enough to trigger the contract

## Next

Read [SKILL.md](SKILL.md).
