# Installing the memd Skill

This skill makes `memd` a skill + CLI workflow. Agents retrieve context with
`memd agent-context` or `memd search` and record durable memory with `memd add`.

## Quick Install (repo checkout)

From the repository root:

```bash
make install
memd doctor
```

`make install` installs the binary, skill, and enforcement wiring in one
idempotent command. Use `make install-binary` for the binary only. The sections
below are the piecewise/advanced path.

The `memd` binary is distributed as prebuilt release artifacts (macOS arm64/x64,
Linux x86_64/aarch64 as static musl) built by cargo-dist — see "Install the
Binary" below.

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

From the repository root, this keeps the installed skill as symlinks:

```bash
make install-skill
```

For the advanced/offline variant, materialize the current skill plus the
locally built `target/release/memd` binary into each unique existing standard
skill directory among
`~/.agents/skills`, `~/.claude/skills`, and `~/.codex/skills`, run:

```bash
make install-skill-bundle
```

That command skips missing parent skill directories and duplicate directories
that resolve to the same path, then writes the bundled binary as
`bin/linux-x64/memd` inside each copied `memd` skill.

## Install the Binary

If `memd` is not already on `PATH`, install a prebuilt release binary. Linux
builds are **static musl**, so there are no `GLIBC_... not found` errors on old
or HPC hosts.

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
```

Rust users can instead use `cargo binstall memd` (prebuilt, best-effort) or
`cargo install memd` (builds from source). The enforcement installer can run the
one-liner for you:

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

Check it:

```bash
which memd
memd --version
```

> Note: the prebuilt installer requires a published cargo-dist release (the first
> release produced by `.github/workflows/release.yml`). Until that exists, use
> `cargo install memd` or build from source with `cargo build --release -p memd`.

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
- directs agents to read an existing `memory.md` and search when a specific
  environment failure, cross-machine question, or conflicting fact calls for it
- reserves memory writes for operational facts that readable repository files
  cannot answer; project plans, test results, and handoffs stay in those files
- makes clear that no memory call is required for routine work
- tells agents not to store full chat logs, play-by-play transcripts, cookies,
  tokens, API keys, passwords, verification codes, ID numbers, bank cards,
  private contact details, third-party account configuration, or sensitive log
  values in `memd`

It does not register external client tools and does not install wrapper guards.

After upgrading memd, re-run this script (or `make install-enforcement`) so the
installed contract matches the new binary.

After the installer runs, verify the wiring:

```bash
memd doctor
```

`memd doctor` reports the state of: the `memd` binary, data directory,
global agent rules (Claude / Codex / Cursor), the Claude `SessionStart`
hook, and the current project's `.memd` scope. Use `--format json` for
machine-readable output.

## Basic CLI Workflow

Use the disposable `quickstart` tenant below to check the installation.

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

Refresh project-root `memory.md` when checking startup wiring or stale content:

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

Record a verified operational fact that repository files cannot answer:

```bash
memd add \
  --tenant-id quickstart \
  --project-id auth \
  --chunk-type summary \
  --tags kind:evidence \
  --text "$OPERATIONAL_FACT"
```

Set `OPERATIONAL_FACT` to the verified observation, affected machine or scope,
evidence, and an action for the next agent. Keep project progress and validation
in repository files. Never store chat transcripts or sensitive credentials.

Use `memd audit` or `memd cleanup-plan` when checking older stores. They report
legacy routine progress summaries without expiry and keep the generated
retention-review action non-destructive.

## Repository Guardrails

### Implicit (no per-repo setup)

When the `SessionStart` hook fires in a repo with no `.memd/project_scope.json`,
`memd session-start` auto-creates a minimal scope file using:

- `tenant_id`: `$MEMD_DEFAULT_TENANT`, then `$USER`, then `"default"`
- `project_id`: lower-cased basename of the repo

Auto-scope writes only `.memd/project_scope.json`. It does not edit agent rules
or tenant guardrails. `MEMD_AUTO_SCOPE=0` disables scope creation; an empty
`.memd-skip` file skips startup even for an initialized repository. Invalid
existing scopes are preserved and reported for repair.

### Explicit (full guardrails)

For richer per-repo guardrails — pinned tenant scope, AGENTS.md / CLAUDE.md
upserts — run:

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
- the Claude `SessionStart` hook is wired in `~/.claude/settings.json`
- `memd session-start` creates a project scope in a temp project
- `memd doctor --strict --format json` passes against a temp project scope
- `memd add` stores a test memory
- `memd memory-md` renders `Project Fact Library` and `Memory health` headers
- `memd search` recovers it
- `memd agent-context` writes a CLI-only context file and JSONL audit log

## Uninstall

```bash
make uninstall
```

This removes the binary (stopping the warm worker first), the skill from all
three skill dirs, and the enforcement wiring via
`memd-skill/uninstall_memd_enforcement.sh`: rule blocks in
`~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md`, the Cursor rule, and the
Claude `SessionStart` hook. It keeps `~/.memd` (the global memory store) and
per-project `.memd/` directories — delete those manually for a clean slate.

Granular targets:

```bash
make uninstall-binary
make uninstall-skill
make uninstall-enforcement
```

## Troubleshooting

### `memd` is not found

Install a prebuilt release binary:

```bash
./memd-skill/install_memd_enforcement.sh --install-binary
```

Then make sure `~/.local/bin` is on `PATH`. Linux releases are static musl, so
there is no glibc-version pitfall. If no prebuilt release exists yet, build from
source on the target host instead:

```bash
cargo install memd
# or: cargo build --release -p memd && install -m 0755 target/release/memd ~/.local/bin/memd
./memd-skill/install_memd_enforcement.sh
memd doctor
```

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
