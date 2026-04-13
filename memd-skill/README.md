# memd Skill

This skill teaches agents how to use `memd` correctly as a shared knowledge base.

It covers:

- when to use `memory.*`
- when to use `task.*`
- when to use `artifact.*`
- how agents should record progress, runs, evidence, and outcomes
- how agents should record critique, revisions, verification, and thread metadata
- how multiple agents should share the same tenant
- how Codex CLI and Claude Code connect to one shared local `memd` HTTP daemon
- how to install stronger `memd`-usage enforcement into `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`
- how to require a pre-refusal `memd` check before agents say work is impossible or blocked
- how to use guarded one-shot wrappers that fail closed on unsupported refusal-style outputs

It also includes a bundled Linux binary:

- [bin/linux-x64/memd](bin/linux-x64/memd)

Start with:

- [INSTALL.md](INSTALL.md)
- [SKILL.md](SKILL.md)
- [examples/INDEX.md](examples/INDEX.md)
