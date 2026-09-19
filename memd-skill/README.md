# memd Skill

This skill teaches agents to use `memd` through the CLI as a shared local
memory store. Repository files remain authoritative for source, task state,
commands, logs, corrections, and handoffs.

The skill covers:

- when to retrieve with `memd agent-context`
- when to search with `memd search`
- when to refresh project-root `memory.md` with `memd memory-md`
- when to write with `memd add`
- when to record a checked problem-attempt-lesson case with `memd experience`
- how to review proposals, route corrections and suggest skills from checked cases
- when to keep retrieval hot with `memd warm start` and `--warm required`
- when to amortize scripted operations with `memd batch --jsonl`
- how to link concise reusable cases to evidence stored in repository files
- how multiple agents share the same tenant and project scope
- how to install CLI-first enforcement into `~/.codex/AGENTS.md` and
  `~/.claude/CLAUDE.md`
- how to require a CLI memory check before agents say work is impossible or
  blocked

Install memd 1.8.0 or later for `memd experience` and detailed consolidation
review. See [INSTALL.md](INSTALL.md).

Start with:

- [INSTALL.md](INSTALL.md)
- [SKILL.md](SKILL.md)
- [Reflection workflow](references/reflection.md)
- [examples/INDEX.md](examples/INDEX.md)
