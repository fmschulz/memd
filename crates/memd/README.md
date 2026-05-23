# memd

`memd` is a local memory CLI for AI agents. It stores project-scoped memories,
task traces, decisions, evidence, and reusable lessons on disk, then retrieves
them through CLI commands such as `memd search`, `memd agent-context`, and
`memd memory-md`.

The crate ships both a library and the `memd` binary. The CLI is designed for
Codex, Claude Code, Cursor, and other agent workflows that need a durable,
machine-local memory layer without a hosted service.

## Install

```bash
cargo install memd
```

## Quick Check

```bash
memd --version
memd doctor
```

Full documentation is available at <https://fmschulz.github.io/memd/>.
