# memd Skill

This skill teaches agents to use `memd` through the CLI as a shared local
knowledge base.

It covers:

- when to retrieve with `memd agent-context`
- when to search with `memd search`
- when to refresh project-root `memory.md` with `memd memory-md`
- when to write with `memd add`
- when to keep retrieval hot with `memd warm start` and `--warm required`
- when to amortize scripted operations with `memd batch --jsonl`
- how to record progress, runs, evidence, decisions, and outcomes as durable
  CLI memories
- how multiple agents share the same tenant and project scope
- how to install CLI-first enforcement into `~/.codex/AGENTS.md` and
  `~/.claude/CLAUDE.md`
- how to require a CLI memory check before agents say work is impossible or
  blocked

It also includes a bundled Linux binary:

- [bin/linux-x64/memd](bin/linux-x64/memd)

Start with:

- [INSTALL.md](INSTALL.md)
- [SKILL.md](SKILL.md)
- [examples/INDEX.md](examples/INDEX.md)
